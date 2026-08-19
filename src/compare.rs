//! Build an original-versus-converted pair for pixel peeping.
//!
//! The conversion happens in memory. Nothing is written, because the point is to
//! decide whether the trade is acceptable *before* committing to it.

use std::path::Path;
use std::sync::Arc;

use gpui::RenderImage;
use image::Frame;

use crate::convert::{self, Quality};
use crate::thumbs::to_bgra;

pub struct Pair {
    pub original: Arc<RenderImage>,
    pub converted: Arc<RenderImage>,
    /// Bytes the encoded WebP would occupy on disk.
    pub converted_bytes: u64,
    pub width: u32,
    pub height: u32,
}

impl Pair {
    /// What the conversion saved, as a percentage. Negative when the file grew.
    pub fn saving_percent(&self, source_bytes: u64) -> f32 {
        if source_bytes == 0 {
            return 0.;
        }
        (source_bytes as f32 - self.converted_bytes as f32) / source_bytes as f32 * 100.
    }
}

/// Decode `path`, encode it at `quality`, and decode that back, so both sides are
/// real pixels rather than a promise.
pub fn build(path: &Path, quality: Quality) -> Option<Pair> {
    let original = image::open(path).ok()?;
    let encoded = convert::encode(&original, quality)?;
    let decoded = image::load_from_memory(&encoded).ok()?;

    let (width, height) = (original.width(), original.height());

    Some(Pair {
        original: Arc::new(RenderImage::new(vec![Frame::new(to_bgra(
            original.into_rgba8(),
        ))])),
        converted: Arc::new(RenderImage::new(vec![Frame::new(to_bgra(
            decoded.into_rgba8(),
        ))])),
        converted_bytes: encoded.len() as u64,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    #[test]
    fn both_sides_decode_at_the_source_dimensions() {
        let dir = std::env::temp_dir().join("imageguide-compare");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.png");
        ImageBuffer::from_fn(120, 80, |x, y| {
            Rgb([(x * 2 % 256) as u8, (y * 3 % 256) as u8, 90])
        })
        .save(&path)
        .unwrap();

        let pair = build(&path, Quality::lossy(70.)).expect("pair builds");

        assert_eq!((pair.width, pair.height), (120, 80));
        // The compare view lines the two up pixel for pixel. If the encoder ever
        // changed the geometry the slider would show a lie.
        assert_eq!(u32::from(pair.original.size(0).width), 120);
        assert_eq!(u32::from(pair.converted.size(0).width), 120);
        assert_eq!(u32::from(pair.converted.size(0).height), 80);
        assert!(pair.converted_bytes > 0);
    }

    #[test]
    fn saving_is_reported_against_the_source_size() {
        let pair = Pair {
            original: Arc::new(RenderImage::new(vec![Frame::new(ImageBuffer::from_pixel(
                1,
                1,
                image::Rgba([0u8, 0, 0, 255]),
            ))])),
            converted: Arc::new(RenderImage::new(vec![Frame::new(ImageBuffer::from_pixel(
                1,
                1,
                image::Rgba([0u8, 0, 0, 255]),
            ))])),
            converted_bytes: 250,
            width: 1,
            height: 1,
        };

        assert_eq!(pair.saving_percent(1000), 75.);
        assert_eq!(pair.saving_percent(0), 0.);
        assert!(pair.saving_percent(100) < 0., "growth reads as negative");
    }
}
