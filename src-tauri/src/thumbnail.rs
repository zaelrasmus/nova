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
    pub is_animated: bool,
}

/// Decode `src` once, write a WebP thumbnail to `dest`, and return the
/// placeholder hash, recipe tag, and animation flag
pub fn generate(src: &Path, dest: &Path, mode: ThumbMode) -> Result<ThumbOutput> {
    let img = image::open(src).with_context(|| format!("Failed to decode image: {src:?}"))?;

    let lossless = match mode {
        ThumbMode::Lossy => false,
        ThumbMode::Lossless => true,
        // Auto: alpha or few colors => flat graphics => lossless; else photo => lossy.
        ThumbMode::Auto => img.color().has_alpha() || is_flat_graphic(&img),
    };

    // High-qualiy downscale for the cached thumbnail (aspect preserved)
    let thumb = img.resize(THUMB_MAX, THUMB_MAX, FilterType::Lanczos3);
    let rgba = thumb.to_rgba8();
    let encoder = webp::Encoder::from_rgba(&rgba, thumb.width(), thumb.height());
    let encoded = if lossless {
        encoder.encode_lossless()
    } else {
        encoder.encode(LOSSY_QUALITY)
    };
    std::fs::write(dest, &*encoded)
        .with_context(|| format!("Failed to write thumbnail: {dest:?}"))?;

    // ThumbHash from a small copy (the algorithm expects a <= 100px input)
    let small = img.thumbnail(HASH_MAX, HASH_MAX).to_rgba8();
    let hash =
        thumbhash::rgba_to_thumb_hash(small.width() as usize, small.height() as usize, &small);

    Ok(ThumbOutput {
        thumb_hash: STANDARD.encode(&hash),
        thumb_config: mode.config_tag().to_string(),
        is_animated: detect_animated(src),
    })
}

/// Cheap flat-graphic detector: few distinct colors on a downscaled copy.
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
/// WebP detection is a later refinement (#11).
fn detect_animated(src: &Path) -> bool {
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
