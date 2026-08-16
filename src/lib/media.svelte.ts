// Playback controller for the viewer's <video>/<audio> element.
//
// Same shape as `panzoom.svelte.ts`: a plain class that OWNS one DOM element,
// attaches its own listeners, and mirrors only what the UI reads into runes.
// The element stays the source of truth — we never keep a shadow copy of
// playback state and hope the two agree.
//
// Decoding is the webview's job, deliberately. That buys hardware acceleration
// for free and costs us the containers browsers refuse (MKV, ProRes MOV, AVI);
// those surface as `failed`, which the player turns into an honest fallback
// rather than a black rectangle.
//
// One non-obvious detail drives the whole design: `timeupdate` fires only ~4×
// per second, which makes a scrubber visibly step. While playing we therefore
// read `currentTime` from a rAF pump and let events take over once paused.

/** Arrow-key seek distance. Exported so the player and its tooltips agree. */
export const SEEK_STEP_S = 5;
/** J/L seek distance — the coarser jump, matching common player convention. */
export const SEEK_JUMP_S = 10;

/** Offered playback rates, cycled by the toolbar's speed button. */
export const RATES = [0.5, 0.75, 1, 1.25, 1.5, 2] as const;

export interface MediaControllerOptions {
  /** Initial volume (0-1), restored from preferences. */
  volume?: number;
  muted?: boolean;
  /** Begin playing as soon as metadata arrives. */
  autoplay?: boolean;
  /** Fires when the USER changes volume or mute, so the caller can persist it. */
  onVolumeChange?: (volume: number, muted: boolean) => void;
}

const clamp01 = (n: number) => (n < 0 ? 0 : n > 1 ? 1 : n);

export class MediaController {
  /** Actually playing, as opposed to merely "not paused" (which is true while buffering). */
  playing = $state(false);
  /** Stalled mid-playback waiting for data — drives the buffering spinner. */
  waiting = $state(false);
  /** Metadata has arrived, so `duration` and the first frame are usable. */
  ready = $state(false);
  /** The element gave up: unsupported container/codec, or the file is unreadable. */
  failed = $state(false);

  currentTime = $state(0);
  /** 0 when unknown — a stream with no seekable length reports Infinity. */
  duration = $state(0);
  /** Seconds buffered ahead of the playhead, for the scrubber's load bar. */
  bufferedTo = $state(0);

  volume = $state(1);
  muted = $state(false);
  rate = $state(1);
  looping = $state(false);

  #el: HTMLMediaElement;
  #raf = 0;
  #cleanup: Array<() => void> = [];

  constructor(el: HTMLMediaElement, opts: MediaControllerOptions = {}) {
    this.#el = el;

    // Applied BEFORE the listeners attach, so restoring the saved volume can't
    // bounce back out through onVolumeChange and rewrite the preference we just
    // read.
    this.volume = clamp01(opts.volume ?? 1);
    this.muted = opts.muted ?? false;
    el.volume = this.volume;
    el.muted = this.muted;

    const on = <K extends keyof HTMLMediaElementEventMap>(
      type: K,
      fn: (e: HTMLMediaElementEventMap[K]) => void,
    ) => {
      el.addEventListener(type, fn);
      this.#cleanup.push(() => el.removeEventListener(type, fn));
    };

    on("loadedmetadata", () => {
      this.duration = Number.isFinite(el.duration) ? el.duration : 0;
      this.ready = true;
      if (opts.autoplay) void this.play();
    });
    // Some containers only report a real duration once enough has been parsed.
    on("durationchange", () => {
      if (Number.isFinite(el.duration)) this.duration = el.duration;
    });

    on("play", () => {
      this.playing = true;
      this.#startPump();
    });
    on("pause", () => {
      this.playing = false;
      this.#stopPump();
      this.#sync();
    });
    on("ended", () => {
      this.playing = false;
      this.#stopPump();
      this.#sync();
    });

    on("waiting", () => (this.waiting = true));
    on("playing", () => (this.waiting = false));
    on("canplay", () => (this.waiting = false));

    on("seeked", () => this.#sync());
    // While playing, the rAF pump owns `currentTime`; this only covers the
    // paused case (e.g. the element settling after a load).
    on("timeupdate", () => {
      if (!this.playing) this.#sync();
    });
    on("progress", () => this.#syncBuffered());

    on("volumechange", () => {
      this.volume = el.volume;
      this.muted = el.muted;
      opts.onVolumeChange?.(el.volume, el.muted);
    });
    on("ratechange", () => (this.rate = el.playbackRate));

    on("error", () => {
      this.failed = true;
      this.playing = false;
      this.waiting = false;
      this.#stopPump();
    });
  }

  // ── Transport ──────────────────────────────────────────────────────────────

  /** Play, swallowing the rejection an autoplay policy or a torn-down element throws. */
  async play(): Promise<void> {
    try {
      await this.#el.play();
    } catch {
      // Not an error worth surfacing: the user can always press play.
    }
  }

  pause(): void {
    this.#el.pause();
  }

  toggle(): void {
    if (this.#el.paused) void this.play();
    else this.pause();
  }

  /** Seek to an absolute time, clamped to the media's length. */
  seek(seconds: number): void {
    if (!Number.isFinite(seconds)) return;
    const max = this.duration || 0;
    this.#el.currentTime = Math.min(Math.max(seconds, 0), max);
    // Mirror immediately so a scrub feels instant rather than waiting for `seeked`.
    this.currentTime = this.#el.currentTime;
  }

  seekBy(delta: number): void {
    this.seek(this.#el.currentTime + delta);
  }

  /** Seek to a 0-1 position — what the scrubber and waveform both speak. */
  seekToFraction(fraction: number): void {
    if (this.duration > 0) this.seek(clamp01(fraction) * this.duration);
  }

  // ── Output ─────────────────────────────────────────────────────────────────

  setVolume(v: number): void {
    const next = clamp01(v);
    this.#el.volume = next;
    // Dragging the slider up is an unambiguous "I want to hear this".
    if (next > 0 && this.#el.muted) this.#el.muted = false;
  }

  nudgeVolume(delta: number): void {
    this.setVolume(this.#el.volume + delta);
  }

  toggleMute(): void {
    this.#el.muted = !this.#el.muted;
  }

  setRate(rate: number): void {
    this.#el.playbackRate = rate;
  }

  /** Step to the next offered rate, wrapping — one button, no popover. */
  cycleRate(): void {
    const i = RATES.indexOf(this.rate as (typeof RATES)[number]);
    this.setRate(RATES[(i + 1) % RATES.length] ?? 1);
  }

  toggleLoop(): void {
    this.looping = !this.looping;
    this.#el.loop = this.looping;
  }

  // ── Internals ──────────────────────────────────────────────────────────────

  /** rAF pump — see the note at the top about `timeupdate`'s ~4Hz resolution. */
  #startPump(): void {
    cancelAnimationFrame(this.#raf);
    const step = () => {
      this.#sync();
      this.#raf = requestAnimationFrame(step);
    };
    this.#raf = requestAnimationFrame(step);
  }

  #stopPump(): void {
    cancelAnimationFrame(this.#raf);
    this.#raf = 0;
  }

  #sync(): void {
    this.currentTime = this.#el.currentTime;
    this.#syncBuffered();
  }

  /**
   * Only the buffered range CONTAINING the playhead matters for a "loaded up to
   * here" bar. After a seek the element often holds several disjoint ranges, and
   * taking the last one would draw a load bar over a gap that isn't loaded.
   */
  #syncBuffered(): void {
    const ranges = this.#el.buffered;
    const at = this.#el.currentTime;
    let end = 0;
    for (let i = 0; i < ranges.length; i++) {
      if (ranges.start(i) <= at && ranges.end(i) >= at) {
        end = ranges.end(i);
        break;
      }
    }
    this.bufferedTo = end;
  }

  destroy(): void {
    this.#stopPump();
    for (const off of this.#cleanup) off();
    this.#cleanup = [];
    this.#el.pause();
    // Release the OS file handle rather than waiting for GC. Nova moves, trashes
    // and purges the very files it is playing, and on Windows a live handle makes
    // those operations fail.
    this.#el.removeAttribute("src");
    this.#el.load();
  }
}

// ── Waveform ─────────────────────────────────────────────────────────────────

/** Refuse to pull more than this into memory to draw a decorative waveform. */
const MAX_WAVEFORM_BYTES = 96 * 1024 * 1024;
/** Samples inspected per bucket. Beyond this we stride — a peak is a peak. */
const MAX_SAMPLES_PER_BUCKET = 512;

/**
 * Decode an audio file to normalised 0-1 peaks, one per bucket.
 *
 * Deliberately independent of playback: this fetches and decodes its own copy
 * rather than tapping the playing element through a WebAudio graph. Routing a
 * media element through `createMediaElementSource` SILENCES it outright if the
 * resource is CORS-cross-origin, and the asset protocol is a different origin
 * from the page — so the "nicer" live-analyser approach risks trading working
 * audio for a decoration. This way the worst case is `null`, and the player
 * falls back to a plain scrubber with sound intact.
 */
export async function computeWaveform(url: string, buckets = 400): Promise<Float32Array | null> {
  try {
    const res = await fetch(url);
    if (!res.ok) return null;

    const bytes = await res.arrayBuffer();
    if (bytes.byteLength > MAX_WAVEFORM_BYTES) return null;

    // OfflineAudioContext decodes without opening an output device.
    const Ctor =
      window.OfflineAudioContext ??
      (window as unknown as { webkitOfflineAudioContext?: typeof OfflineAudioContext })
        .webkitOfflineAudioContext;
    if (!Ctor) return null;

    const audio = await new Ctor(1, 1, 44100).decodeAudioData(bytes);
    const samples = audio.getChannelData(0);

    const per = Math.max(1, Math.floor(samples.length / buckets));
    const stride = Math.max(1, Math.floor(per / MAX_SAMPLES_PER_BUCKET));

    const peaks = new Float32Array(buckets);
    let loudest = 0;
    for (let b = 0; b < buckets; b++) {
      const start = b * per;
      const end = Math.min(start + per, samples.length);
      let peak = 0;
      for (let i = start; i < end; i += stride) {
        const v = samples[i] < 0 ? -samples[i] : samples[i];
        if (v > peak) peak = v;
      }
      peaks[b] = peak;
      if (peak > loudest) loudest = peak;
    }

    // Normalise, so a quietly-mastered track still fills the strip.
    if (loudest > 0) {
      for (let b = 0; b < buckets; b++) peaks[b] /= loudest;
    }
    return peaks;
  } catch {
    // Blocked by CSP, an unsupported codec, a file that moved — all the same
    // answer to the caller: draw the simple scrubber instead.
    return null;
  }
}
