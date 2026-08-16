//! Import-time metadata extraction: the cheap, header-only read that gives an
//! asset its dimensions before any pixels are decoded.
//!
//! Split from `thumbnail` on purpose — this runs for every file during import
//! and must stay fast; thumbnails are generated later, on view.

use crate::assets::AssetType;
use crate::thumbnail;
use anyhow::Result;
use std::path::Path;

/// The cheap, import-time visual metadata of an asset. Defaults to "no visual",
/// so a type without a renderer (or a failed read) still yields a valid asset.
#[derive(Default)]
pub struct ExtractedVisual {
    pub width: u32,
    pub height: u32,
    pub is_animated: bool,
}

pub trait MetadataExtractor {
    /// Read only cheap metadata (dimensions, animation flag). Must not decode
    /// full pixel data or write files — that is the thumbnail pipeline's job.
    fn extract(&self, src: &Path) -> Result<ExtractedVisual>;
}

/// Read the cheap visual metadata for a file of `asset_type`.
///
/// Static dispatch. This used to hand back a `Box<dyn MetadataExtractor>`, which
/// heap-allocated once per file — for four ZERO-SIZED structs, inside the Rayon
/// `par_iter` that walks every file in an import. The trait stays because it is
/// what states the contract each extractor has to honour (cheap metadata only,
/// no full decode, no writes); nothing ever needed the trait OBJECT.
pub fn extract_visual(asset_type: AssetType, src: &Path) -> Result<ExtractedVisual> {
    match asset_type {
        AssetType::Image => ImageExtractor.extract(src),
        AssetType::Video => VideoExtractor.extract(src),
        AssetType::Audio => AudioExtractor.extract(src),
        AssetType::Unknown => NoopExtractor.extract(src),
    }
}

struct ImageExtractor;
impl MetadataExtractor for ImageExtractor {
    fn extract(&self, src: &Path) -> Result<ExtractedVisual> {
        // Header read only — no full decode. Dimensions drive masonry layout
        // and are available in the manifest the instant import finishes.
        let (width, height) = image::image_dimensions(src)?;
        Ok(ExtractedVisual {
            width,
            height,
            is_animated: thumbnail::detect_animated(src),
        })
    }
}

// Returns nothing on purpose. `image` cannot open an MP4, so dimensions and a
// keyframe are produced LATER by the webview — it loads the file, seeks past the
// leader and draws a frame to a canvas (`mediathumbs.ts` → `store_media_thumbnail`),
// which is also where the row first learns its real width and height. An ffmpeg
// sidecar would let this happen at import time instead, and would additionally
// cover the containers the webview refuses (MKV, ProRes); it is not needed for
// anything that already plays.
struct VideoExtractor;
impl MetadataExtractor for VideoExtractor {
    fn extract(&self, _src: &Path) -> Result<ExtractedVisual> {
        Ok(ExtractedVisual::default())
    }
}

// Also nothing, and audio genuinely has no dimensions to report. Its thumbnail
// is a WAVEFORM, rendered in the webview from peaks decoded via WebAudio — so
// symphonia would buy only the ability to do it here instead, at import time,
// for a picture that is generated on view anyway.
struct AudioExtractor;
impl MetadataExtractor for AudioExtractor {
    fn extract(&self, _src: &Path) -> Result<ExtractedVisual> {
        Ok(ExtractedVisual::default())
    }
}

struct NoopExtractor;
impl MetadataExtractor for NoopExtractor {
    fn extract(&self, _src: &Path) -> Result<ExtractedVisual> {
        Ok(ExtractedVisual::default())
    }
}
