use crate::assets::AssetType;
use crate::thumbnail::{self, ThumbMode};
use anyhow::Result;
use std::path::Path;
use tracing::warn;

/// Inputs an extractor needs beyond the source path.
pub struct ExtractContext<'a> {
    pub thumbs_dir: &'a Path,
    pub id: &'a str,
    pub mode: ThumbMode,
}

/// The visual portion of an asset's metadata. Defaults to "no visual", so a
/// type without a renderer (or a failed extraction) still yields a valid asset.
#[derive(Default)]
pub struct ExtractedVisual {
    pub width: u32,
    pub height: u32,
    pub thumb_hash: Option<String>,
    pub thumb_config: Option<String>,
    pub is_animated: bool,
    pub has_thumb: bool,
}

pub trait MetadataExtractor {
    fn extract(&self, src: &Path, ctx: &ExtractContext) -> Result<ExtractedVisual>;
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
    fn extract(&self, src: &Path, ctx: &ExtractContext) -> Result<ExtractedVisual> {
        let (width, height) = image::image_dimensions(src)?;
        let thumb_dest = ctx.thumbs_dir.join(format!("{}.webp", ctx.id));

        // Thumbnail is best-effort: a write/encode failure keeps dims, drops thumb.
        match thumbnail::generate(src, &thumb_dest, ctx.mode) {
            Ok(t) => Ok(ExtractedVisual {
                width,
                height,
                thumb_hash: Some(t.thumb_hash),
                thumb_config: Some(t.thumb_config),
                is_animated: t.is_animated,
                has_thumb: true,
            }),
            Err(e) => {
                warn!(path = ?src, error = %e, "Thumbnail generation failed; keeping asset without thumbnail");
                Ok(ExtractedVisual {
                    width,
                    height,
                    ..Default::default()
                })
            }
        }
    }
}

// todo: keyframe thumbnail + dimensions via an ffmpeg sidecar.
struct VideoExtractor;
impl MetadataExtractor for VideoExtractor {
    fn extract(&self, _src: &Path, _ctx: &ExtractContext) -> Result<ExtractedVisual> {
        Ok(ExtractedVisual::default())
    }
}

// todo: waveform rendering via symphonia.
struct AudioExtractor;
impl MetadataExtractor for AudioExtractor {
    fn extract(&self, _src: &Path, _ctx: &ExtractContext) -> Result<ExtractedVisual> {
        Ok(ExtractedVisual::default())
    }
}

struct NoopExtractor;
impl MetadataExtractor for NoopExtractor {
    fn extract(&self, _src: &Path, _ctx: &ExtractContext) -> Result<ExtractedVisual> {
        Ok(ExtractedVisual::default())
    }
}
