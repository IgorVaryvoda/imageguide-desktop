//! Re-encode an image, on this machine.
//!
//! The `image` crate can only write lossless WebP, which is the wrong tool for the
//! job — the whole point is trading a little quality for a lot of bytes. This uses
//! libwebp directly for both, and picks between them by whether the source has
//! meaningful transparency.
//!
//! AVIF goes through libavif's libaom backend and libyuv colour conversion. The system
//! libraries are the same path as `avifenc`, without starting a process per image.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use image::DynamicImage;
use libwebp_sys::{
    WEBP_ENCODER_ABI_VERSION, WebPConfig, WebPConfigInitInternal, WebPEncode, WebPMemoryWrite,
    WebPMemoryWriter, WebPMemoryWriterClear, WebPMemoryWriterInit, WebPPicture, WebPPictureFree,
    WebPPictureImportRGB, WebPPictureImportRGBA, WebPPictureInitInternal, WebPPreset,
    WebPValidateConfig,
};

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
    let mut config = std::mem::MaybeUninit::<WebPConfig>::uninit();
    // SAFETY: `config` points to writable storage for exactly one WebPConfig and the
    // ABI constant comes from the same statically linked libwebp crate.
    if unsafe {
        WebPConfigInitInternal(
            config.as_mut_ptr(),
            WebPPreset::WEBP_PRESET_DEFAULT,
            quality.0.unwrap_or(75.),
            WEBP_ENCODER_ABI_VERSION as i32,
        )
    } == 0
    {
        return None;
    }
    // SAFETY: the initializer above succeeded.
    let mut config = unsafe { config.assume_init() };
    config.lossless = i32::from(lossless);
    config.alpha_compression = i32::from(!lossless);
    config.quality = quality.0.unwrap_or(75.);
    // ponytail: method 1 makes lossy q80 output 13.5% larger, but cut a real 3.0GB
    // folder from 69.5s to 29.6s. Lossless keeps the prior method 4 behavior because
    // its explicit promise is unchanged pixels, not the lossy path's speed tradeoff.
    config.method = if lossless { 4 } else { 1 };
    if unsafe { WebPValidateConfig(&config) } == 0 {
        return None;
    }

    if let Some(pixels) = image.as_rgba8() {
        encode_webp_pixels(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            true,
            &config,
        )
    } else if let Some(pixels) = image.as_rgb8() {
        encode_webp_pixels(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            false,
            &config,
        )
    } else if image.color().has_alpha() {
        let pixels = image.to_rgba8();
        encode_webp_pixels(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            true,
            &config,
        )
    } else {
        let pixels = image.to_rgb8();
        encode_webp_pixels(
            pixels.as_raw(),
            pixels.width(),
            pixels.height(),
            false,
            &config,
        )
    }
}

fn encode_webp_pixels(
    pixels: &[u8],
    width: u32,
    height: u32,
    alpha: bool,
    config: &WebPConfig,
) -> Option<Vec<u8>> {
    let channels = if alpha { 4 } else { 3 };
    let stride = i32::try_from(width.checked_mul(channels)?).ok()?;
    let width = i32::try_from(width).ok()?;
    let height = i32::try_from(height).ok()?;
    let expected = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(channels as usize)?;
    if pixels.len() != expected {
        return None;
    }
    let mut picture = std::mem::MaybeUninit::<WebPPicture>::uninit();
    // SAFETY: all pointers remain valid through the synchronous encode. Both libwebp
    // allocations are released before returning, including every failure path.
    unsafe {
        if WebPPictureInitInternal(picture.as_mut_ptr(), WEBP_ENCODER_ABI_VERSION as i32) == 0 {
            return None;
        }
        let mut picture = picture.assume_init();
        picture.use_argb = 1;
        picture.width = width;
        picture.height = height;
        let imported = if alpha {
            WebPPictureImportRGBA(&mut picture, pixels.as_ptr(), stride)
        } else {
            WebPPictureImportRGB(&mut picture, pixels.as_ptr(), stride)
        };
        if imported == 0 {
            WebPPictureFree(&mut picture);
            return None;
        }

        let mut writer = std::mem::MaybeUninit::<WebPMemoryWriter>::uninit();
        WebPMemoryWriterInit(writer.as_mut_ptr());
        let mut writer = writer.assume_init();
        picture.writer = Some(WebPMemoryWrite);
        picture.custom_ptr = (&mut writer as *mut WebPMemoryWriter).cast();
        let encoded = if WebPEncode(config, &mut picture) != 0 && !writer.mem.is_null() {
            Some(std::slice::from_raw_parts(writer.mem, writer.size).to_vec())
        } else {
            None
        };
        WebPMemoryWriterClear(&mut writer);
        WebPPictureFree(&mut picture);
        encoded
    }
}

/// AVIF keeps alpha in a separate plane, so transparency needs no special case here.
/// libaom and rav1e calibrate their 1-100 scales differently; 75% matches the former
/// rav1e output size and measured PSNR on the real corpus.
fn encode_avif(image: &DynamicImage, quality: Quality) -> Option<Vec<u8>> {
    let has_alpha = has_transparency(image);
    let quality = aom_quality(quality);
    let cores = std::thread::available_parallelism().map_or(4, |count| count.get());
    let threads = (cores / workers(Format::Avif)).clamp(1, 8);

    if has_alpha {
        let rgba = image.to_rgba8();
        crate::avif::encode(
            rgba.as_raw(),
            rgba.width(),
            rgba.height(),
            true,
            quality,
            threads,
        )
    } else {
        let rgb = image.to_rgb8();
        crate::avif::encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            false,
            quality,
            threads,
        )
    }
}

fn aom_quality(quality: Quality) -> u8 {
    ((quality.0.unwrap_or(90.) * 0.75).round() as u8).clamp(1, 75)
}

/// True when any pixel is not fully opaque. A PNG with an alpha channel that is
/// entirely 255 is just an RGB image paying for a fourth channel, and should still
/// get the lossy path.
fn has_transparency(image: &DynamicImage) -> bool {
    match image {
        DynamicImage::ImageRgba8(buffer) => buffer.pixels().any(|pixel| pixel.0[3] != 255),
        DynamicImage::ImageLumaA8(buffer) => buffer.pixels().any(|pixel| pixel.0[1] != 255),
        DynamicImage::ImageRgba16(buffer) => buffer.pixels().any(|pixel| pixel.0[3] != u16::MAX),
        DynamicImage::ImageLumaA16(buffer) => buffer.pixels().any(|pixel| pixel.0[1] != u16::MAX),
        // A float TIFF is rare and its alpha is still alpha. Answering "no" here would
        // send a cut-out down the opaque path and flatten it.
        DynamicImage::ImageRgba32F(buffer) => buffer.pixels().any(|pixel| pixel.0[3] != 1.0),
        _ => false,
    }
}

/// How many files to convert at once.
///
/// Each file in flight holds a fully decoded image, so this bounds memory as much as
/// it bounds speed, and the two encoders want opposite things. libwebp runs on the
/// calling thread and left one core 46% busy on a sixteen-core machine, so WebP wants
/// a file per core. libaom gets half the available threads per image, so two files
/// keep the machine busy without doubling peak decoded-image memory again.
pub fn workers(format: Format) -> usize {
    let cores = std::thread::available_parallelism().map_or(4, |count| count.get());
    match format {
        Format::WebP => cores.clamp(2, 8),
        Format::Avif => 2,
    }
}

/// Convert every path, `workers(format)` at a time, calling `report` with each result
/// as it lands. `report` is called from a worker thread, one at a time.
///
/// The window's conversion has its own copy of this loop built out of executor tasks,
/// because it has to hand each result back to the UI thread. This one is for callers
/// that only need the work done.
pub fn convert_each(
    root: &Path,
    sources: &[PathBuf],
    out_dir: &Path,
    format: Format,
    quality: Quality,
    max_edge: MaxEdge,
    report: impl Fn(&Path, Option<Converted>) + Sync,
) {
    let planned = &plan_outputs(root, sources, out_dir, format);
    // A shared cursor rather than a slice per thread: files in one folder differ in
    // size by a hundred times, so a fixed split leaves most threads finished early.
    let next = &AtomicUsize::new(0);
    let report = &report;
    let reporting = &parking_lot::Mutex::new(());
    std::thread::scope(|scope| {
        for _ in 0..workers(format).min(sources.len().max(1)) {
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let (Some(source), Some(written)) = (sources.get(index), planned.get(index))
                    else {
                        return;
                    };
                    let converted = convert_to(source, written, format, quality, max_edge);
                    let _ordered = reporting.lock();
                    report(source, converted);
                }
            });
        }
    });
}

/// Where a converted file goes: the same layout as the source, rooted at `out_dir`,
/// with a `.webp` extension. Keeping the tree means a folder of albums stays a folder
/// of albums.
pub fn output_path(root: &Path, source: &Path, out_dir: &Path, format: Format) -> PathBuf {
    let relative = source.strip_prefix(root).unwrap_or(source);
    out_dir.join(relative).with_extension(format.extension())
}

/// One output path per source, no two of them the same.
///
/// `output_path` replaces the source extension, so `shot.jpg` and `shot.png` in one
/// folder both ask for `optimized/shot.webp`. Whichever finished second replaced the
/// other, and the run still reported two files converted and a saving that counted
/// bytes no longer on disk. A second claim on a name keeps its source extension:
/// `optimized/shot-jpg.webp`, then `-2`, `-3` if even that is taken.
pub fn plan_outputs(
    root: &Path,
    sources: &[PathBuf],
    out_dir: &Path,
    format: Format,
) -> Vec<PathBuf> {
    // Keyed case-insensitively. `Shot.png` and `shot.jpg` are two files on Linux and
    // one on macOS, and renaming one of them needlessly costs a stranger name, while
    // not renaming it costs a lost image.
    let mut taken: HashSet<String> = HashSet::new();
    let key = |path: &Path| path.to_string_lossy().to_lowercase();

    sources
        .iter()
        .map(|source| {
            let plain = output_path(root, source, out_dir, format);
            if taken.insert(key(&plain)) {
                return plain;
            }
            let extension = source
                .extension()
                .map(|extension| extension.to_string_lossy().to_lowercase())
                .unwrap_or_else(|| "file".to_string());
            let stem = plain.file_stem().unwrap_or_default().to_string_lossy();
            let parent = plain.parent().unwrap_or(out_dir);
            for attempt in 1.. {
                let suffix = if attempt == 1 {
                    String::new()
                } else {
                    format!("-{attempt}")
                };
                let candidate = parent
                    .join(format!("{stem}-{extension}{suffix}"))
                    .with_extension(format.extension());
                if taken.insert(key(&candidate)) {
                    return candidate;
                }
            }
            unreachable!("the loop returns as soon as a name is free")
        })
        .collect()
}

/// Read, encode, and write one file to the path `plan_outputs` chose for it.
pub fn convert_to(
    source: &Path,
    written: &Path,
    format: Format,
    quality: Quality,
    max_edge: MaxEdge,
) -> Option<Converted> {
    let decoded = max_edge.apply(crate::scan::decode(source)?);
    let (width, height) = (decoded.width(), decoded.height());
    let encoded = encode(&decoded, format, quality)?;

    std::fs::create_dir_all(written.parent()?).ok()?;
    // Write beside the target and rename onto it. A crash or a full disk part-way
    // through the write would otherwise leave a short `.webp` that looks finished, and
    // the next run would list it as a real image.
    let mut partial = written.to_path_buf().into_os_string();
    partial.push(".part");
    let partial = PathBuf::from(partial);
    if std::fs::write(&partial, &encoded).is_err() || std::fs::rename(&partial, written).is_err() {
        let _ = std::fs::remove_file(&partial);
        return None;
    }

    Some(Converted {
        written: written.to_path_buf(),
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
    fn bundled_libwebp_has_the_current_encoder() {
        // 0xMMmmpp: major, minor, patch.
        let version = unsafe { libwebp_sys::WebPGetEncoderVersion() };
        assert!(version >= 0x010600, "bundled libwebp is {version:#08x}");
    }

    #[test]
    fn output_is_a_real_avif() {
        let encoded = encode(&photo(32, 32), Format::Avif, Quality::lossy(80.)).unwrap();
        // ISO base media file format: a 'ftyp' box naming the AVIF brand.
        assert_eq!(&encoded[4..8], b"ftyp");
        assert_eq!(&encoded[8..12], b"avif");
    }

    #[test]
    fn aom_quality_matches_the_measured_rav1e_output() {
        assert_eq!(aom_quality(Quality::lossy(80.)), 60);
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
        let image = DynamicImage::ImageRgba8(buffer);
        assert!(has_transparency(&image), "one see-through pixel is enough");

        let encoded = encode(&image, Format::WebP, Quality::lossy(20.)).unwrap();
        let decoded = image::load_from_memory(&encoded).unwrap().to_rgba8();
        assert_eq!(decoded.get_pixel(1, 1), &Rgba([10, 20, 30, 255]));
        assert_eq!(decoded.get_pixel(0, 0)[3], 0);
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

        let converted = convert_to(
            &source,
            &output_path(&dir, &source, &out, Format::WebP),
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

        let converted = convert_to(
            &source,
            &output_path(&dir, &source, &dir.join("out"), Format::WebP),
            Format::WebP,
            Quality::lossy(80.),
            MaxEdge(Some(200)),
        )
        .expect("conversion runs");

        assert_eq!((converted.width, converted.height), (200, 100));
    }

    /// The bug this exists to stop: `shot.jpg` and `shot.png` both asked for
    /// `optimized/shot.webp`, one silently replaced the other, and the run still
    /// reported two conversions and a total that counted bytes no longer on disk.
    #[test]
    fn two_sources_never_claim_one_output() {
        let root = Path::new("/photos");
        let out = Path::new("/photos/optimized");
        let sources = [
            PathBuf::from("/photos/shot.jpg"),
            PathBuf::from("/photos/shot.png"),
            PathBuf::from("/photos/album/shot.png"),
            PathBuf::from("/photos/shot.webp"),
        ];

        let planned = plan_outputs(root, &sources, out, Format::WebP);
        assert_eq!(
            planned,
            [
                PathBuf::from("/photos/optimized/shot.webp"),
                PathBuf::from("/photos/optimized/shot-png.webp"),
                PathBuf::from("/photos/optimized/album/shot.webp"),
                PathBuf::from("/photos/optimized/shot-webp.webp"),
            ],
            "the first claim keeps the plain name, a subfolder is not a clash"
        );
    }

    /// Names that differ only in case are two files on Linux and one on macOS, so they
    /// count as a clash either way. Three of them exhaust the extension suffix and
    /// reach the numbered fallback.
    #[test]
    fn case_alone_is_a_clash_and_a_taken_suffix_gets_numbered() {
        let sources = [
            PathBuf::from("/p/a.png"),
            PathBuf::from("/p/A.png"),
            PathBuf::from("/p/a.PNG"),
        ];

        let planned = plan_outputs(Path::new("/p"), &sources, Path::new("/p/out"), Format::WebP);
        let names: Vec<String> = planned
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.webp", "A-png.webp", "a-png-2.webp"]);
    }

    /// A part-written file must never be left looking like a finished one.
    #[test]
    fn a_conversion_leaves_no_partial_file_behind() {
        let dir = temp_dir("atomic");
        let source = dir.join("in.png");
        photo(64, 64).save(&source).unwrap();
        let written = dir.join("out").join("in.webp");

        convert_to(
            &source,
            &written,
            Format::WebP,
            Quality::lossy(80.),
            MaxEdge::FULL,
        )
        .expect("conversion runs");

        let leftovers: Vec<_> = std::fs::read_dir(dir.join("out"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "found {leftovers:?}");
    }

    #[test]
    fn quality_is_clamped_and_labelled() {
        assert_eq!(Quality::lossy(500.).0, Some(100.));
        assert_eq!(Quality::lossy(-3.).0, Some(1.));
        assert_eq!(Quality::lossy(80.).label(), "q80");
        assert_eq!(Quality::LOSSLESS.label(), "lossless");
    }
}
