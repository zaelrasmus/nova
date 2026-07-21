//! Thumbnail + placeholder generation for imported images.
//!
//! Decodes each source image once, writes a WebP thumbnail (alpha preserved),
//! and returns a ThumbHash placeholder, the recipe tag used, and whether the
//! source is animated. Never panics on a bad file — returns Err, and the caller
//! keeps the asset with NULL thumb fields (never drop an asset).

use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use image::{imageops::FilterType, AnimationDecoder, DynamicImage};
use std::collections::HashSet;
use std::path::Path;
use tracing::debug;

/// Longest edge (px) of the cached thumbnail
const THUMB_MAX: u32 = 512;
/// ThumbHash expects a small input; cap the edge used to compute it
const HASH_MAX: u32 = 100;
/// WebP lossy quality (0.0 - 100.0)
const LOSSY_QUALITY: f32 = 82.0;
/// Above this many distinct colors (sampled small), treat the image as a photo
const FLAT_COLOR_LIMIT: usize = 512;

/// How thumbnails should be encoded. Mirrors the user setting
#[derive(Clone, Copy, Debug)]
pub enum ThumbMode {
    /// Lossless for flat graphics / alpha, lossy for photos.
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

    /// Staleness tag stored per asset. Compared against the current setting to
    /// decide whether a thumbnail needs regenerating
    fn config_tag(self) -> &'static str {
        match self {
            ThumbMode::Auto => "webp:auto",
            ThumbMode::Lossy => "webp:lossy:82",
            ThumbMode::Lossless => "webp:lossless",
        }
    }
}

pub struct ThumbOutput {
    pub thumb_hash: String,
    pub thumb_config: String,
}

/// Decode `src` once, write a WebP thumbnail to `dest`, and return the
/// placeholder hash and recipe tag. Runs on the background thumbnail pipeline,
/// never on the import critical path.
pub fn generate(src: &Path, dest: &Path, mode: ThumbMode) -> Result<ThumbOutput> {
    let decode_start = std::time::Instant::now();
    let img = image::open(src).with_context(|| format!("Failed to decode image: {src:?}"))?;
    let decode_ms = decode_start.elapsed().as_millis();
    let encode_start = std::time::Instant::now();

    // Downscale the full-resolution source exactly ONCE. The encode input, the
    // ThumbHash source, and the flat-graphic check are all derived from this
    // 512px thumb — not from three independent resizes of the (up to 12MP)
    // original, which was the import-speed regression. Triangle is ~3x faster
    // than Lanczos3 and visually indistinguishable at thumbnail size. (Switch to
    // FilterType::CatmullRom if you want slightly sharper edges at a small cost.)
    let thumb = img.resize(THUMB_MAX, THUMB_MAX, FilterType::Triangle);

    let lossless = match mode {
        ThumbMode::Lossy => false,
        ThumbMode::Lossless => true,
        // Auto: alpha or few colors => flat graphics => lossless; else photo => lossy.
        // Alpha comes from the source color type; flatness from the cheap thumb.
        ThumbMode::Auto => img.color().has_alpha() || is_flat_graphic(&thumb),
    };

    let rgba = thumb.to_rgba8();
    let encoder = webp::Encoder::from_rgba(&rgba, thumb.width(), thumb.height());
    let encoded = if lossless {
        encoder.encode_lossless()
    } else {
        encoder.encode(LOSSY_QUALITY)
    };

    // Atomic write: encode into a temp file next to the target, then rename.
    // A crash/close mid-write leaves the .tmp (ignored) instead of a truncated
    // .webp that the NULL-checking resume pass would wrongly treat as complete.
    let tmp = dest.with_extension("webp.tmp");
    std::fs::write(&tmp, &*encoded)
        .with_context(|| format!("Failed to write thumbnail temp: {tmp:?}"))?;
    std::fs::rename(&tmp, dest)
        .with_context(|| format!("Failed to finalize thumbnail: {dest:?}"))?;

    // ThumbHash wants a <= 100px input; derive it from the 512px thumb (cheap),
    // not another full-resolution downscale of the original.
    let small = thumb.thumbnail(HASH_MAX, HASH_MAX).to_rgba8();
    let hash =
        thumbhash::rgba_to_thumb_hash(small.width() as usize, small.height() as usize, &small);
    let encode_ms = encode_start.elapsed().as_millis();

    debug!(
        ?src,
        decode_ms,
        encode_ms,
        lossless,
        "Thumbnail generated"
    );

    Ok(ThumbOutput {
        thumb_hash: STANDARD.encode(&hash),
        thumb_config: mode.config_tag().to_string(),
    })
}

/// Cheap flat-graphic detector: few distinct colors on a 64px copy of the
/// already-downscaled 512px thumb (not the full-resolution source).
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
