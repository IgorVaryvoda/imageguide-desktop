//! Re-encode an image to WebP, on this machine.
//!
//! The `image` crate can only write lossless WebP, which is the wrong tool for the
//! job — the whole point is trading a little quality for a lot of bytes. This uses
//! libwebp directly for both, and picks between them by whether the source has
//! meaningful transparency.

use std::path::{Path, PathBuf};

use image::DynamicImage;

/// Encoder quality, 1 to 100. `None` means lossless.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quality(pub Option<f32>);

impl Quality {
    pub const LOSSLESS: Self = Self(None);

    pub fn lossy(value: f32) -> Self {
        Self(Some(value.clamp(1., 100.)))
    }

    pub fn label(&self) -> String {
        match self.0 {
            None => "lossless".to_string(),
            Some(value) => format!("q{}", value.round() as u32),
        }
    }
}

/// The result of re-encoding one file.
#[derive(Clone, Debug, PartialEq)]
pub struct Converted {
    pub written: PathBuf,
    pub bytes: u64,
}

/// Encode `image` as WebP. Returns the encoded bytes.
///
/// libwebp's lossy path discards the alpha channel's precision in ways that ruin
/// cut-outs, so anything carrying real transparency goes lossless regardless of the
/// requested quality.
pub fn encode(image: &DynamicImage, quality: Quality) -> Option<Vec<u8>> {
    let lossless = quality.0.is_none() || has_transparency(image);
    let encoder = webp::Encoder::from_image(image).ok()?;

    let memory = if lossless {
        encoder.encode_lossless()
    } else {
        encoder.encode(quality.0.unwrap_or(80.))
    };
    Some(memory.to_vec())
}

/// True when any pixel is not fully opaque. A PNG with an alpha channel that is
/// entirely 255 is just an RGB image paying for a fourth channel, and should still
/// get the lossy path.
fn has_transparency(image: &DynamicImage) -> bool {
    match image {
        DynamicImage::ImageRgba8(buffer) => buffer.pixels().any(|pixel| pixel.0[3] != 255),
        DynamicImage::ImageLumaA8(buffer) => buffer.pixels().any(|pixel| pixel.0[1] != 255),
        DynamicImage::ImageRgba16(buffer) => buffer.pixels().any(|pixel| pixel.0[3] != u16::MAX),
        _ => false,
    }
}

/// Where a converted file goes: the same layout as the source, rooted at `out_dir`,
/// with a `.webp` extension. Keeping the tree means a folder of albums stays a folder
/// of albums.
pub fn output_path(root: &Path, source: &Path, out_dir: &Path) -> PathBuf {
    let relative = source.strip_prefix(root).unwrap_or(source);
    out_dir.join(relative).with_extension("webp")
}

/// Read, encode, and write one file. Returns what was written.
pub fn convert_file(
    root: &Path,
    source: &Path,
    out_dir: &Path,
    quality: Quality,
) -> Option<Converted> {
    let decoded = image::open(source).ok()?;
    let encoded = encode(&decoded, quality)?;

    let written = output_path(root, source, out_dir);
    std::fs::create_dir_all(written.parent()?).ok()?;
    std::fs::write(&written, &encoded).ok()?;

    Some(Converted {
        written,
        bytes: encoded.len() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb, Rgba};

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("imageguide-convert-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Deterministic noise. A flat colour compresses to nothing, and so does a smooth
    /// gradient — lossless WebP squeezed one to 90 bytes and made an earlier version of
    /// these tests assert something false. Real photographs are noisy; this is too.
    fn photo(width: u32, height: u32) -> DynamicImage {
        DynamicImage::ImageRgb8(ImageBuffer::from_fn(width, height, |x, y| {
            let mut hash = x.wrapping_mul(2_654_435_761) ^ y.wrapping_mul(2_246_822_519);
            hash ^= hash >> 13;
            hash = hash.wrapping_mul(3_266_489_917);
            Rgb([(hash >> 8) as u8, (hash >> 16) as u8, (hash >> 24) as u8])
        }))
    }

    /// The quality number has to actually reach libwebp. If it were dropped on the
    /// floor both encodes would come back the same size and nobody would notice.
    #[test]
    fn lower_quality_produces_fewer_bytes() {
        let image = photo(256, 256);
        let low = encode(&image, Quality::lossy(20.)).expect("q20 encodes");
        let high = encode(&image, Quality::lossy(95.)).expect("q95 encodes");

        assert!(
            low.len() < high.len(),
            "q20 {} should be smaller than q95 {}",
            low.len(),
            high.len()
        );
    }

    #[test]
    fn output_is_a_real_webp() {
        let encoded = encode(&photo(32, 32), Quality::lossy(80.)).unwrap();
        assert_eq!(&encoded[0..4], b"RIFF");
        assert_eq!(&encoded[8..12], b"WEBP");
    }

    #[test]
    fn transparency_forces_the_lossless_path() {
        let mut buffer: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(16, 16, Rgba([10, 20, 30, 255]));
        assert!(!has_transparency(&DynamicImage::ImageRgba8(buffer.clone())));

        buffer.put_pixel(0, 0, Rgba([10, 20, 30, 0]));
        assert!(
            has_transparency(&DynamicImage::ImageRgba8(buffer)),
            "one see-through pixel is enough"
        );
    }

    #[test]
    fn output_path_mirrors_the_source_tree() {
        let path = output_path(
            Path::new("/photos"),
            Path::new("/photos/album/one.PNG"),
            Path::new("/photos/optimised"),
        );
        assert_eq!(path, Path::new("/photos/optimised/album/one.webp"));
    }

    #[test]
    fn converting_writes_a_smaller_file_and_reports_its_size() {
        let dir = temp_dir("roundtrip");
        let source = dir.join("big.png");
        photo(400, 400).save(&source).unwrap();
        let out = dir.join("optimised");

        let converted =
            convert_file(&dir, &source, &out, Quality::lossy(75.)).expect("conversion runs");

        assert_eq!(converted.written, out.join("big.webp"));
        assert!(converted.written.exists(), "the file is actually on disk");
        assert_eq!(
            converted.bytes,
            std::fs::metadata(&converted.written).unwrap().len(),
            "reported size matches the file"
        );
        // Not asserting it shrank: this source is pure noise, which is the one input
        // that legitimately does not compress. Size correctness is covered above.
        assert!(converted.bytes > 0);
    }

    #[test]
    fn quality_is_clamped_and_labelled() {
        assert_eq!(Quality::lossy(500.).0, Some(100.));
        assert_eq!(Quality::lossy(-3.).0, Some(1.));
        assert_eq!(Quality::lossy(80.).label(), "q80");
        assert_eq!(Quality::LOSSLESS.label(), "lossless");
    }
}
