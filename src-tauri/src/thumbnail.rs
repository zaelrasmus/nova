//! Thumbnail + placeholder generation.
//!
//! Decodes each source once, writes a WebP thumbnail (alpha preserved), and
//! returns a ThumbHash placeholder, the recipe tag used, and whether the source
//! is animated. Never panics on a bad file — returns Err, and the caller keeps
//! the asset with NULL thumb fields (never drop an asset).
//!
//! Two entry points, one pipeline. `generate` decodes an image file itself;
//! `generate_from_capture` takes pixels the webview decoded, which is how video
//! and audio get thumbnails at all — `image` cannot open an MP4, and the webview
//! is the only media decoder in the process.

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{imageops::FilterType, AnimationDecoder, DynamicImage};
use std::collections::HashSet;
use std::path::Path;
use tracing::debug;

/// Target for the SHORTER edge of a generated thumbnail (px). Pinning the short
/// edge (not the long one) keeps every masonry card crisp regardless of aspect.
const THUMB_SHORT_EDGE: u32 = 320;
/// Hard cap on the LONGER edge (px) — an out-of-memory/disk guardrail for
/// pathological aspect ratios, NOT a quality knob. At 8192 the short edge keeps
/// its full `THUMB_SHORT_EDGE` up to ~25.6:1 (covers webtoons/tall art); only
/// past that does it start to shrink. Very tall sources whose short edge is
/// already small never reach here — they take the skip branch below.
const THUMB_LONG_MAX: u32 = 8192;
/// ThumbHash expects a small input; cap the edge used to compute it
const HASH_MAX: u32 = 100;
/// Above this many distinct colors (sampled small), treat the image as a photo
const FLAT_COLOR_LIMIT: usize = 512;

/// How thumbnails should be encoded. Mirrors the user setting
#[derive(Clone, Copy, Debug)]
pub enum ThumbMode {
    /// Auto: Lossless for flat graphics / alpha, lossy for photos.
    Auto,
    Lossy,
    Lossless,
}

impl ThumbMode {
    /// Map the frontend setting string to a mode.
    pub fn from_setting(s: &str) -> Self {
        match s {
            "lossy" => ThumbMode::Lossy,
            "lossless" => ThumbMode::Lossless,
            _ => ThumbMode::Auto,
        }
    }
}

/// A resolved thumbnail recipe: the encode mode plus the lossy quality to use.
/// Bundled so the whole pipeline passes one Copy value, and so the staleness tag
/// can fold in the quality (a slider change is then detectable later).
#[derive(Clone, Copy, Debug)]
pub struct ThumbConfig {
    pub mode: ThumbMode,
    /// Lossy quality 0-100. Ignored for lossless (and the lossless branch of Auto).
    pub quality: f32,
}

/// The thumbnail knobs as they cross the IPC boundary — one typed object instead
/// of two loose `thumb_mode` / `quality` args on every generation command.
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ThumbSettings {
    pub mode: String,
    pub quality: f32,
}

impl ThumbConfig {
    /// Build from the frontend's mode string + quality-slider value.
    pub fn from_setting(mode: &str, quality: f32) -> Self {
        Self {
            mode: ThumbMode::from_setting(mode),
            quality: quality.clamp(0.0, 100.0),
        }
    }

    /// Build from the IPC settings object.
    pub fn from_settings(s: &ThumbSettings) -> Self {
        Self::from_setting(&s.mode, s.quality)
    }

    /// Staleness tag stored per asset. Includes the lossy quality so a future
    /// "regenerate stale thumbnails" pass can notice a slider change.
    fn config_tag(self) -> String {
        match self.mode {
            ThumbMode::Auto => format!("webp:auto:q{}", self.quality as u32),
            ThumbMode::Lossy => format!("webp:lossy:q{}", self.quality as u32),
            ThumbMode::Lossless => "webp:lossless".to_string(),
        }
    }
}

pub struct ThumbOutput {
    pub thumb_hash: String,
    pub thumb_config: String,
    /// The SOURCE's dimensions, as decoded. The caller uses these to fill in a
    /// row that never had any — an online image is recorded 0×0 because there
    /// was nothing local to measure.
    pub src_width: u32,
    pub src_height: u32,
    /// False when the source was already small enough that a thumbnail would be
    /// no smaller — the grid shows the original instead.
    pub wrote_file: bool,
    /// Dominant colors, extracted from the same decode. Riding along here is the
    /// whole reason color analysis is affordable: a separate pass would have to
    /// decode every image again, and decode is the expensive part.
    pub palette: Vec<crate::color::PaletteEntry>,
}

/// Decode `src` once, write a WebP thumbnail to `dest`, and return the
/// placeholder hash and recipe tag. Runs on the background thumbnail pipeline,
/// never on the import critical path.
pub fn generate(src: &Path, dest: &Path, config: ThumbConfig) -> Result<ThumbOutput> {
    let img = image::open(src).with_context(|| format!("Failed to decode image: {src:?}"))?;
    from_image(&img, dest, config, false)
}

/// Thumbnail from a PNG the WEBVIEW produced — a video keyframe or a rendered
/// audio waveform (see `mediathumbs.svelte.ts`). The webview owns the only media
/// decoder we have, so for video and audio the pixels arrive from the frontend
/// instead of from `image::open`; from here on the pipeline is identical.
///
/// `always_write` is forced on, and that is load-bearing rather than a detail.
/// The skip-the-file optimisation below tells the grid to fall back to the
/// ORIGINAL, which for an image is another image — but for a video it is an MP4,
/// which no `<img>` can render. A small video would otherwise be marked done and
/// show nothing forever.
pub fn generate_from_capture(png: &[u8], dest: &Path, config: ThumbConfig) -> Result<ThumbOutput> {
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .context("Failed to decode captured frame")?;
    from_image(&img, dest, config, true)
}

/// Thumbnail from image bytes held in memory, format sniffed rather than assumed.
///
/// This is the ONLINE-IMAGE path: there is no file to open, because the asset is
/// only a link, so `remote` fetches the bytes and they arrive here still encoded
/// in whatever the origin served.
///
/// `always_write` for the same reason as a capture — the "source is small enough,
/// let the grid use the original" shortcut has no original to fall back to when
/// the original lives on someone else's server.
pub fn generate_from_bytes(bytes: &[u8], dest: &Path, config: ThumbConfig) -> Result<ThumbOutput> {
    let img = image::load_from_memory(bytes).context("Failed to decode the fetched image")?;
    from_image(&img, dest, config, true)
}

/// The shared body: downscale once, encode WebP, hash, extract the palette.
fn from_image(
    img: &DynamicImage,
    dest: &Path,
    config: ThumbConfig,
    always_write: bool,
) -> Result<ThumbOutput> {
    let (w, h) = (img.width(), img.height());
    let short = w.min(h);

    // Already small enough (short edge <= target): a thumbnail would be as big as
    // the original, so skip the file entirely and let the grid use the original.
    // Still return a ThumbHash so the row is marked done and never re-requested.
    if !always_write && (short == 0 || short <= THUMB_SHORT_EDGE) {
        debug!(w, h, "Source small enough; skipping thumbnail file");
        return Ok(ThumbOutput {
            thumb_hash: thumb_hash_base64(img),
            thumb_config: config.config_tag(),
            src_width: w,
            src_height: h,
            wrote_file: false,
            palette: crate::color::extract_palette(img),
        });
    }

    let encode_start = std::time::Instant::now();

    // Downscale the full-resolution source exactly ONCE: the encode input, the
    // ThumbHash source and the flat-graphic check all derive from this one thumb.
    // Independent resizes of the (up to 12MP) original were the import-speed
    // regression. Triangle is ~3x faster than Lanczos3 and indistinguishable at
    // this size (CatmullRom for sharper edges, at a cost).
    //
    // Pin the SHORT edge, but never let the LONG edge exceed THUMB_LONG_MAX;
    // tighter constraint wins. Normally the short-edge rule dominates (matching
    // Eagle) and the long cap only clamps panoramas and tall comics.
    if w == 0 || h == 0 {
        anyhow::bail!("Refusing to thumbnail a zero-pixel image ({w}x{h})");
    }

    let long = w.max(h);
    // Never above 1.0: with `always_write` a capture smaller than the target
    // would otherwise be UPSCALED, spending bytes to add no detail.
    let scale = (THUMB_SHORT_EDGE as f32 / short as f32)
        .min(THUMB_LONG_MAX as f32 / long as f32)
        .min(1.0);
    let tw = ((w as f32 * scale).round() as u32).max(1);
    let th = ((h as f32 * scale).round() as u32).max(1);
    let thumb = img.resize_exact(tw, th, FilterType::Triangle);

    let rgba = thumb.to_rgba8();

    let lossless = match config.mode {
        ThumbMode::Lossy => false,
        ThumbMode::Lossless => true,
        // Auto: ONLY flat/graphic content (pixel art, logos, few-color images)
        // earns lossless — that's where lossy's hard-edge artifacts actually show.
        // Alpha is deliberately NOT a trigger: WebP lossy preserves the alpha
        // channel, so detailed transparent art (character drawings, stickers) goes
        // lossy and stays small while keeping transparency. Pixel art WITH alpha
        // still hits the flat check, so it stays lossless.
        ThumbMode::Auto => is_flat_graphic(&thumb),
    };

    let encoded = encode_webp(&rgba, thumb.width(), thumb.height(), lossless, config.quality)?;

    // Atomic write: encode into a temp file next to the target, then rename.
    // A crash/close mid-write leaves the .tmp (ignored) instead of a truncated
    // .webp that the NULL-checking resume pass would wrongly treat as complete.
    let tmp = dest.with_extension("webp.tmp");
    std::fs::write(&tmp, &encoded)
        .with_context(|| format!("Failed to write thumbnail temp: {tmp:?}"))?;
    std::fs::rename(&tmp, dest)
        .with_context(|| format!("Failed to finalize thumbnail: {dest:?}"))?;

    let thumb_hash = thumb_hash_base64(&thumb);
    let encode_ms = encode_start.elapsed().as_millis();
    debug!(encode_ms, tw, th, lossless, "Thumbnail generated");

    Ok(ThumbOutput {
        thumb_hash,
        thumb_config: config.config_tag(),
        src_width: w,
        src_height: h,
        wrote_file: true,
        // Sampled from the downscaled thumb, not the original — ~100x fewer pixels
        // for a palette that's indistinguishable at this granularity.
        palette: crate::color::extract_palette(&thumb),
    })
}

/// Encode `rgba` (width×height) as WebP using libwebp's max compression effort
/// (`method = 6`). `method` trades ENCODE TIME for a smaller file at the SAME
/// quality — the crate's `encode()`/`encode_lossless()` leave it at libwebp's
/// default of 4. Since generation runs off the import critical path we always pay
/// for the extra effort: it's a pure size win with no visual cost. `lossless`
/// picks the codec; for lossy, `quality` is the 0-100 visual quality.
fn encode_webp(rgba: &[u8], width: u32, height: u32, lossless: bool, quality: f32) -> Result<Vec<u8>> {
    let mut config =
        webp::WebPConfig::new().map_err(|()| anyhow::anyhow!("failed to initialize WebP config"))?;
    config.method = 6;
    if lossless {
        config.lossless = 1;
        config.alpha_compression = 0;
        // For the lossless codec, `quality` is compression EFFORT, not fidelity:
        // higher means a smaller file for more CPU. Max it out.
        config.quality = 100.0;
    } else {
        config.lossless = 0;
        config.alpha_compression = 1;
        config.quality = quality;
    }

    let mem = webp::Encoder::from_rgba(rgba, width, height)
        .encode_advanced(&config)
        .map_err(|e| anyhow::anyhow!("WebP encode failed: {e:?}"))?;
    Ok(mem.to_vec())
}

/// ThumbHash (base64) from a small copy of `img`. ThumbHash wants a <=100px
/// input, so this is cheap regardless of the source size.
fn thumb_hash_base64(img: &DynamicImage) -> String {
    let small = img.thumbnail(HASH_MAX, HASH_MAX).to_rgba8();
    let hash =
        thumbhash::rgba_to_thumb_hash(small.width() as usize, small.height() as usize, &small);
    STANDARD.encode(&hash)
}

/// Cheap flat-graphic detector: few distinct colors on a 64px copy of the
/// already-downscaled thumb (not the full-resolution source).
fn is_flat_graphic(img: &DynamicImage) -> bool {
    let small = img.thumbnail(64, 64).to_rgb8();
    let mut colors = HashSet::new();
    for px in small.pixels() {
        colors.insert([px[0], px[1], px[2]]);
        if colors.len() > FLAT_COLOR_LIMIT {
            return false; // too many colors -> photographic
        }
    }
    true
}

/// True if the source is a multi-frame animation. GIF only for now; animated
/// WebP detection is a later refinement (#11). Called at import time (cheap:
/// reads at most two frames).
pub fn detect_animated(src: &Path) -> bool {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();

    if ext != "gif" {
        return false;
    }

    let Ok(file) = std::fs::File::open(src) else {
        return false;
    };
    match image::codecs::gif::GifDecoder::new(std::io::BufReader::new(file)) {
        // take(2) decodes at most two frames — enough to know if it's animated.
        Ok(decoder) => decoder.into_frames().take(2).count() > 1,
        Err(_) => false,
    }
}
