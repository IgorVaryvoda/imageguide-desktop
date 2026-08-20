//! Build an original-versus-converted pair for pixel peeping.
//!
//! The conversion happens in memory. Nothing is written, because the point is to
//! decide whether the trade is acceptable *before* committing to it.

use std::path::Path;
use std::sync::Arc;

use gpui::RenderImage;
use image::Frame;

use crate::convert::{self, Format, MaxEdge, Quality};
use crate::thumbs::to_bgra;

/// Everything that changes what a `Pair` contains. Reopening the same image with the
/// same settings should not re-encode it, which at AVIF speeds is a two-second wait.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub path: std::path::PathBuf,
    pub format: Format,
    /// Quality as raw bits, because f32 is not `Eq`.
    pub quality: Option<u32>,
    pub max_edge: Option<u32>,
}

impl Key {
    pub fn new(path: &Path, format: Format, quality: Quality, max_edge: MaxEdge) -> Self {
        Self {
            path: path.to_path_buf(),
            format,
            quality: quality.0.map(f32::to_bits),
            max_edge: max_edge.0,
        }
    }
}

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
///
/// When a size budget is set, the *original* side is downscaled too. Comparing a
/// 6400px source against a 2000px export would measure the resize, not the
/// compression, and the resize is not the part you need to eyeball. Both sides are
/// the delivered resolution; only one of them has been through the encoder.
pub fn build(path: &Path, format: Format, quality: Quality, max_edge: MaxEdge) -> Option<Pair> {
    let original = max_edge.apply(crate::scan::decode(path)?);
    let encoded = convert::encode(&original, format, quality)?;
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

        let pair =
            build(&path, Format::WebP, Quality::lossy(70.), MaxEdge::FULL).expect("pair builds");

        assert_eq!((pair.width, pair.height), (120, 80));
        // The compare view lines the two up pixel for pixel. If the encoder ever
        // changed the geometry the slider would show a lie.
        assert_eq!(u32::from(pair.original.size(0).width), 120);
        assert_eq!(u32::from(pair.converted.size(0).width), 120);
        assert_eq!(u32::from(pair.converted.size(0).height), 80);
        assert!(pair.converted_bytes > 0);
    }

    /// The cache is only correct if the key notices every setting that changes the
    /// output. A missed field would serve a WebP pair for an AVIF request.
    #[test]
    fn cache_keys_separate_every_setting() {
        let path = Path::new("/photos/one.png");
        let base = Key::new(path, Format::WebP, Quality::lossy(80.), MaxEdge::FULL);

        assert_eq!(
            base,
            Key::new(path, Format::WebP, Quality::lossy(80.), MaxEdge::FULL)
        );
        assert_ne!(
            base,
            Key::new(path, Format::Avif, Quality::lossy(80.), MaxEdge::FULL)
        );
        assert_ne!(
            base,
            Key::new(path, Format::WebP, Quality::lossy(60.), MaxEdge::FULL)
        );
        assert_ne!(
            base,
            Key::new(path, Format::WebP, Quality::LOSSLESS, MaxEdge::FULL)
        );
        assert_ne!(
            base,
            Key::new(path, Format::WebP, Quality::lossy(80.), MaxEdge(Some(1600)))
        );
        assert_ne!(
            base,
            Key::new(
                Path::new("/photos/two.png"),
                Format::WebP,
                Quality::lossy(80.),
                MaxEdge::FULL
            )
        );
    }

    #[test]
    fn a_size_budget_shrinks_both_sides_together() {
        let dir = std::env::temp_dir().join("imageguide-compare-resize");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wide.png");
        ImageBuffer::from_fn(400, 200, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 40]))
            .save(&path)
            .unwrap();

        let pair = build(&path, Format::WebP, Quality::lossy(70.), MaxEdge(Some(100)))
            .expect("pair builds");

        assert_eq!((pair.width, pair.height), (100, 50));
        assert_eq!(u32::from(pair.original.size(0).width), 100);
        assert_eq!(
            u32::from(pair.converted.size(0).width),
            100,
            "both sides must be the delivered size or the divider compares nothing"
        );
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
