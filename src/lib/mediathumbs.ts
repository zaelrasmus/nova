// Thumbnails for video and audio, generated in the WEBVIEW.
//
// The Rust pipeline decodes with the `image` crate, which cannot open an MP4 or
// an MP3 — so every non-image row is skipped there and its `thumb_hash` stays
// NULL forever. The webview, though, already ships a media decoder. This module
// borrows it: load the file into a detached <video>, seek past the leader, draw a
// frame to a canvas, and hand the pixels back to Rust, which encodes, hashes and
// palette-samples them exactly like an image thumbnail (`store_media_thumbnail`).
//
// Audio has no frame to capture, so it gets the same treatment applied to its
// decoded waveform instead — a real picture of the file, not a stock glyph.
//
// Two properties matter more than speed here:
//   · SERIAL. Each capture holds a decoder plus a full-size canvas; running a
//     screenful at once is how you make a 100k-asset grid stutter.
//   · Attempted-once per session. A file the engine refuses (MKV, ProRes) must
//     not be retried on every scroll — `thumb_hash` stays NULL, so without this
//     set the grid would ask again forever.

import { invoke } from "@tauri-apps/api/core";
import { convertFileSrc } from "@tauri-apps/api/core";
import { assetLibrary, type MediaThumbReady } from "./assets.svelte";
import { computeWaveform } from "./media.svelte";

/** Long-edge cap for a captured frame. Rust pins the short edge to 320, so this
 *  leaves it a real downscale to work with while keeping the PNG (and the base64
 *  that carries it over IPC) small. */
const CAPTURE_MAX_EDGE = 720;
/** A file the engine can't handle can hang rather than error; never block the queue. */
const LOAD_TIMEOUT_MS = 15_000;
/** Decoding an hour-long podcast to draw a 512px waveform isn't worth it. */
const AUDIO_MAX_BYTES = 40 * 1024 * 1024;
/** Edge of the square waveform tile. Audio has no aspect, so the grid lays it
 *  out 1:1 and this matches. */
const AUDIO_TILE = 512;
/** Cap the backlog: on a fast scroll through thousands of clips, only what is
 *  near the viewport is still worth generating. Newest requests go to the front. */
const MAX_QUEUE = 120;

interface Capture {
  /** PNG bytes, base64, without the data-URL prefix. */
  png: string;
  /** The asset's NATURAL dimensions (not the capture's) — 0 for audio. */
  width: number;
  height: number;
}

/** Resolve on `event`, reject on `error` or after `ms`. */
function once(el: HTMLMediaElement, event: string, ms: number): Promise<void> {
  return new Promise((resolve, reject) => {
    const cleanup = () => {
      clearTimeout(timer);
      el.removeEventListener(event, onDone);
      el.removeEventListener("error", onError);
    };
    const onDone = () => {
      cleanup();
      resolve();
    };
    const onError = () => {
      cleanup();
      reject(new Error(`media error waiting for ${event}`));
    };
    const timer = setTimeout(() => {
      cleanup();
      reject(new Error(`timed out waiting for ${event}`));
    }, ms);
    el.addEventListener(event, onDone, { once: true });
    el.addEventListener("error", onError, { once: true });
  });
}

/**
 * Serialise a canvas to base64 PNG.
 *
 * Throws `SecurityError` if the canvas was tainted by a cross-origin draw, which
 * is exactly how a CORS misconfiguration would show up. Callers treat it as any
 * other failed capture rather than special-casing it.
 */
function toPngBase64(canvas: HTMLCanvasElement): string {
  const url = canvas.toDataURL("image/png");
  return url.slice(url.indexOf(",") + 1);
}

/** Grab a representative frame, or null if the engine can't play the file. */
async function captureVideoFrame(url: string): Promise<Capture | null> {
  const video = document.createElement("video");
  // MUST be assigned BEFORE `src`. It is what makes the asset-protocol response
  // CORS-clean; without it `drawImage` taints the canvas and `toDataURL` throws.
  // (Same mechanism the viewer's eyedropper relies on for images.)
  video.crossOrigin = "anonymous";
  video.preload = "auto";
  video.muted = true;
  video.playsInline = true;

  try {
    video.src = url;
    await once(video, "loadedmetadata", LOAD_TIMEOUT_MS);

    const width = video.videoWidth;
    const height = video.videoHeight;
    if (!width || !height) return null;

    // One second in is usually past the black leader or fade-up that opens a lot
    // of video; on a very short clip take the midpoint instead of running off
    // the end.
    const duration = Number.isFinite(video.duration) ? video.duration : 0;
    const target = duration > 0 ? Math.min(1, duration * 0.5) : 0;
    if (target > 0) {
      video.currentTime = target;
      await once(video, "seeked", LOAD_TIMEOUT_MS);
    } else if (video.readyState < 2) {
      // No usable duration — at least wait until a frame exists to draw.
      await once(video, "loadeddata", LOAD_TIMEOUT_MS);
    }

    const scale = Math.min(1, CAPTURE_MAX_EDGE / Math.max(width, height));
    const cw = Math.max(1, Math.round(width * scale));
    const ch = Math.max(1, Math.round(height * scale));

    const canvas = document.createElement("canvas");
    canvas.width = cw;
    canvas.height = ch;
    const ctx = canvas.getContext("2d");
    if (!ctx) return null;
    ctx.drawImage(video, 0, 0, cw, ch);

    return { png: toPngBase64(canvas), width, height };
  } finally {
    // Release the decoder and the OS file handle now rather than at GC — Nova
    // moves, trashes and purges the files it is reading.
    video.removeAttribute("src");
    video.load();
  }
}

/** Draw decoded peaks as a square tile, in the grid's own palette. */
function renderWaveform(peaks: Float32Array): string {
  const canvas = document.createElement("canvas");
  canvas.width = AUDIO_TILE;
  canvas.height = AUDIO_TILE;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("no 2d context for waveform");

  // neutral-900: the same colour an empty card shows, so the tile reads as part
  // of the grid rather than as a pasted-in image.
  ctx.fillStyle = "#171717";
  ctx.fillRect(0, 0, AUDIO_TILE, AUDIO_TILE);

  const BAR = 4;
  const GAP = 2;
  const step = BAR + GAP;
  const bars = Math.floor(AUDIO_TILE / step);
  const mid = AUDIO_TILE / 2;
  const maxHeight = AUDIO_TILE * 0.62;

  ctx.fillStyle = "#3b82f6"; // blue-500, the app's accent
  for (let i = 0; i < bars; i++) {
    const peak = peaks[Math.floor((i / bars) * peaks.length)] ?? 0;
    const h = Math.max(2, peak * maxHeight);
    ctx.fillRect(i * step + GAP / 2, mid - h / 2, BAR, h);
  }

  // Drawn from rects only, so this canvas is never tainted.
  return toPngBase64(canvas);
}

class MediaThumbnailer {
  #queue: string[] = [];
  /** Tried this session — success or failure. See the note at the top. */
  #attempted = new Set<string>();
  #running = false;
  #mode = "auto";
  #quality = 80;

  /**
   * Request thumbnails for video/audio ids that are still missing one. Safe to
   * call on every scroll tick: already-queued and already-attempted ids are
   * dropped, and the newest window goes to the front.
   */
  enqueue(ids: string[], mode: string, quality: number): void {
    this.#mode = mode;
    this.#quality = quality;
    const fresh = ids.filter((id) => !this.#attempted.has(id) && !this.#queue.includes(id));
    if (!fresh.length) return;
    this.#queue = [...fresh, ...this.#queue].slice(0, MAX_QUEUE);
    void this.#pump();
  }

  async #pump(): Promise<void> {
    if (this.#running) return;
    this.#running = true;
    try {
      for (;;) {
        const id = this.#queue.shift();
        if (id === undefined) break;
        if (this.#attempted.has(id)) continue;
        this.#attempted.add(id);
        try {
          await this.#capture(id);
        } catch (e) {
          // Unplayable container, a taint, a timeout — all the same outcome:
          // no thumbnail, and the generic card stays. Never fatal.
          console.warn(`Media thumbnail failed for ${id}:`, e);
        }
      }
    } finally {
      this.#running = false;
    }
  }

  async #capture(id: string): Promise<void> {
    await assetLibrary.ensure([id]);
    const heavy = assetLibrary.heavy.get(id);
    if (!heavy?.dest_path) return;
    // Generated meanwhile (a previous session, or this id arrived twice).
    if (heavy.thumb_hash) return;

    const url = convertFileSrc(heavy.dest_path);
    let capture: Capture | null = null;

    if (heavy.asset_type === "video") {
      capture = await captureVideoFrame(url);
    } else if (heavy.asset_type === "audio") {
      if (heavy.file_size > AUDIO_MAX_BYTES) return;
      const peaks = await computeWaveform(url, 256);
      if (peaks) capture = { png: renderWaveform(peaks), width: 0, height: 0 };
    }
    if (!capture) return;

    const ready = await invoke<MediaThumbReady | null>("store_media_thumbnail", {
      id,
      pngBase64: capture.png,
      width: capture.width,
      height: capture.height,
      settings: { mode: this.#mode, quality: this.#quality },
    });
    // null means a rebuild is running and about to regenerate everything anyway.
    if (ready) assetLibrary.applyMediaThumbnail(ready);
  }
}

export const mediaThumbnailer = new MediaThumbnailer();
