use crate::assets::AssetType;
use crate::thumbnail;
use anyhow::Result;
use std::path::Path;

/// The cheap, import-time visual metadata of an asset. Defaults to "no visual",
/// so a type without a renderer (or a failed read) still yields a valid asset.
///
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

/// Dispatch to the extractor for a given asset type.
pub fn extractor_for(asset_type: AssetType) -> Box<dyn MetadataExtractor> {
    match asset_type {
        AssetType::Image => Box::new(ImageExtractor),
        AssetType::Video => Box::new(VideoExtractor),
        AssetType::Audio => Box::new(AudioExtractor),
        AssetType::Unknown => Box::new(NoopExtractor),
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

// todo: keyframe thumbnail + dimensions via an ffmpeg sidecar.
struct VideoExtractor;
impl MetadataExtractor for VideoExtractor {
    fn extract(&self, _src: &Path) -> Result<ExtractedVisual> {
        Ok(ExtractedVisual::default())
    }
}

// todo: waveform rendering via symphonia.
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
