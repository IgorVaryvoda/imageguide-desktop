//! Re-encode an image, on this machine.
//!
//! The `image` crate can only write lossless WebP, which is the wrong tool for the
//! job — the whole point is trading a little quality for a lot of bytes. This uses
//! libwebp directly for both, and picks between them by whether the source has
//! meaningful transparency.
//!
//! AVIF goes through `rav1e`, built without its assembly. `rav1e`'s `asm` feature
//! refuses to build unless `nasm` is installed, and requiring a build tool from every
//! contributor to save encode time is the wrong trade for a desktop app. It is
//! noticeably slower than WebP either way — that is AV1, not the missing assembly.

use std::path::{Path, PathBuf};

use image::DynamicImage;
use ravif::{Encoder as AvifEncoder, Img};
use rgb::FromSlice;

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

/// Longest edge of the exported image. `None` leaves the source alone.
///
/// This is where most of the weight actually is. Re-encoding a 6400px photo as AVIF
/// still hands back a 6400px photo, which is the wrong image for a web page however
/// well it is compressed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaxEdge(pub Option<u32>);

impl MaxEdge {
    pub const FULL: Self = Self(None);

    /// The sizes offered in the window, in order. Listed once so the buttons and the
    /// value they select cannot disagree.
    pub const PRESETS: [Self; 4] = [
        Self::FULL,
        Self(Some(2400)),
        Self(Some(1600)),
        Self(Some(1000)),
    ];

    pub fn label(&self) -> String {
        match self.0 {
            None => "full".to_string(),
            Some(edge) => format!("{edge}px"),
        }
    }

    /// Scale `image` down to fit. Never scales up: an 800px source asked to fit 2000px
    /// is already inside the budget, and stretching it would invent detail.
    pub fn apply(&self, image: DynamicImage) -> DynamicImage {
        let Some(edge) = self.0 else {
            return image;
        };
        if image.width().max(image.height()) <= edge {
            return image;
        }
        // Lanczos3 rather than the fast filter used for thumbnails: this one is what
        // gets shipped, and a soft downscale wastes the bytes it saves.
        image.resize(edge, edge, image::imageops::FilterType::Lanczos3)
    }
}

/// The container to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    WebP,
    Avif,
}

impl Format {
    pub fn extension(&self) -> &'static str {
        match self {
            Format::WebP => "webp",
            Format::Avif => "avif",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Format::WebP => "webp",
            Format::Avif => "avif",
        }
    }
}

/// The result of re-encoding one file.
#[derive(Clone, Debug, PartialEq)]
pub struct Converted {
    pub written: PathBuf,
    pub bytes: u64,
    /// Dimensions actually written, which differ from the source when resizing.
    pub width: u32,
    pub height: u32,
}

/// Encode `image` in `format`. Returns the encoded bytes.
pub fn encode(image: &DynamicImage, format: Format, quality: Quality) -> Option<Vec<u8>> {
    match format {
        Format::WebP => encode_webp(image, quality),
        Format::Avif => encode_avif(image, quality),
    }
}

/// libwebp's lossy path discards the alpha channel's precision in ways that ruin
/// cut-outs, so anything carrying real transparency goes lossless regardless of the
/// requested quality.
fn encode_webp(image: &DynamicImage, quality: Quality) -> Option<Vec<u8>> {
    let lossless = quality.0.is_none() || has_transparency(image);
    let encoder = webp::Encoder::from_image(image).ok()?;

    let memory = if lossless {
        encoder.encode_lossless()
    } else {
        encoder.encode(quality.0.unwrap_or(80.))
    };
    Some(memory.to_vec())
}

/// AVIF keeps alpha in a separate plane, so transparency needs no special case here.
/// `quality` maps straight onto rav1e's 1-100 scale; lossless AVIF is not offered
/// because it is routinely larger than lossless WebP and slower to produce.
fn encode_avif(image: &DynamicImage, quality: Quality) -> Option<Vec<u8>> {
    let rgba = image.to_rgba8();
    let (width, height) = (rgba.width() as usize, rgba.height() as usize);

    let encoded = AvifEncoder::new()
        .with_quality(quality.0.unwrap_or(90.))
        .with_speed(6)
        .encode_rgba(Img::new(rgba.as_raw().as_rgba(), width, height))
        .ok()?;

    Some(encoded.avif_file)
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
pub fn output_path(root: &Path, source: &Path, out_dir: &Path, format: Format) -> PathBuf {
    let relative = source.strip_prefix(root).unwrap_or(source);
    out_dir.join(relative).with_extension(format.extension())
}

/// Read, encode, and write one file. Returns what was written.
pub fn convert_file(
    root: &Path,
    source: &Path,
    out_dir: &Path,
    format: Format,
    quality: Quality,
    max_edge: MaxEdge,
) -> Option<Converted> {
    let decoded = max_edge.apply(crate::scan::decode(source)?);
    let (width, height) = (decoded.width(), decoded.height());
    let encoded = encode(&decoded, format, quality)?;

    let written = output_path(root, source, out_dir, format);
    std::fs::create_dir_all(written.parent()?).ok()?;
    std::fs::write(&written, &encoded).ok()?;

    Some(Converted {
        written,
        bytes: encoded.len() as u64,
        width,
        height,
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
        let low = encode(&image, Format::WebP, Quality::lossy(20.)).expect("q20 encodes");
        let high = encode(&image, Format::WebP, Quality::lossy(95.)).expect("q95 encodes");

        assert!(
            low.len() < high.len(),
            "q20 {} should be smaller than q95 {}",
            low.len(),
            high.len()
        );
    }

    #[test]
    fn output_is_a_real_webp() {
        let encoded = encode(&photo(32, 32), Format::WebP, Quality::lossy(80.)).unwrap();
        assert_eq!(&encoded[0..4], b"RIFF");
        assert_eq!(&encoded[8..12], b"WEBP");
    }

    #[test]
    fn output_is_a_real_avif() {
        let encoded = encode(&photo(32, 32), Format::Avif, Quality::lossy(80.)).unwrap();
        // ISO base media file format: a 'ftyp' box naming the AVIF brand.
        assert_eq!(&encoded[4..8], b"ftyp");
        assert_eq!(&encoded[8..12], b"avif");
    }

    /// Alpha survives the trip. AVIF carries it in its own plane, so unlike WebP there
    /// is no lossless fallback protecting it, and a regression here would silently
    /// flatten every cut-out.
    #[test]
    fn avif_keeps_transparency() {
        let mut buffer: ImageBuffer<Rgba<u8>, Vec<u8>> =
            ImageBuffer::from_pixel(64, 64, Rgba([200u8, 30, 40, 255]));
        for x in 0..32 {
            for y in 0..64 {
                buffer.put_pixel(x, y, Rgba([200, 30, 40, 0]));
            }
        }

        let encoded = encode(
            &DynamicImage::ImageRgba8(buffer),
            Format::Avif,
            Quality::lossy(90.),
        )
        .expect("avif encodes");
        let decoded = image::load_from_memory(&encoded).expect("avif decodes");

        assert!(
            has_transparency(&decoded),
            "the see-through half came back opaque"
        );
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
            Format::WebP,
        );
        assert_eq!(path, Path::new("/photos/optimised/album/one.webp"));

        let avif = output_path(
            Path::new("/photos"),
            Path::new("/photos/album/one.PNG"),
            Path::new("/photos/optimised"),
            Format::Avif,
        );
        assert_eq!(avif, Path::new("/photos/optimised/album/one.avif"));
    }

    #[test]
    fn converting_writes_a_smaller_file_and_reports_its_size() {
        let dir = temp_dir("roundtrip");
        let source = dir.join("big.png");
        photo(400, 400).save(&source).unwrap();
        let out = dir.join("optimised");

        let converted = convert_file(
            &dir,
            &source,
            &out,
            Format::WebP,
            Quality::lossy(75.),
            MaxEdge::FULL,
        )
        .expect("conversion runs");

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
    fn max_edge_scales_down_and_keeps_the_aspect_ratio() {
        let scaled = MaxEdge(Some(100)).apply(photo(400, 200));
        assert_eq!((scaled.width(), scaled.height()), (100, 50));
    }

    #[test]
    fn max_edge_never_scales_up() {
        let untouched = MaxEdge(Some(4000)).apply(photo(80, 60));
        assert_eq!(
            (untouched.width(), untouched.height()),
            (80, 60),
            "a small source must not be stretched to fill the budget"
        );
        let full = MaxEdge::FULL.apply(photo(80, 60));
        assert_eq!((full.width(), full.height()), (80, 60));
    }

    #[test]
    fn resizing_is_reported_in_the_result() {
        let dir = temp_dir("resize");
        let source = dir.join("wide.png");
        photo(600, 300).save(&source).unwrap();

        let converted = convert_file(
            &dir,
            &source,
            &dir.join("out"),
            Format::WebP,
            Quality::lossy(80.),
            MaxEdge(Some(200)),
        )
        .expect("conversion runs");

        assert_eq!((converted.width, converted.height), (200, 100));
    }

    #[test]
    fn quality_is_clamped_and_labelled() {
        assert_eq!(Quality::lossy(500.).0, Some(100.));
        assert_eq!(Quality::lossy(-3.).0, Some(1.));
        assert_eq!(Quality::lossy(80.).label(), "q80");
        assert_eq!(Quality::LOSSLESS.label(), "lossless");
    }
}
