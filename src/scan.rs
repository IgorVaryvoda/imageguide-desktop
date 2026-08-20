//! Read what a folder of images actually contains.
//!
//! Everything here is header-only. Decoding a 6000px JPEG to learn it is 6000px wide
//! costs a hundred times what reading its header costs, and a shoot folder has
//! thousands of them.

use std::path::{Path, PathBuf};

use image::{ImageFormat, ImageReader};
use walkdir::WalkDir;

/// Camera raw formats. Most are TIFF containers, so a plain header read reports the
/// embedded preview — a 6000x4000 NEF comes back as a 160x120 TIFF, which makes every
/// derived number a lie. They are also not web delivery candidates. Counted, not listed.
const RAW_EXTENSIONS: [&str; 9] = [
    "nef", "cr2", "cr3", "arw", "dng", "orf", "rw2", "raf", "srw",
];

pub fn is_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| RAW_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

/// Converted files land here, inside the folder being audited. A second run would
/// otherwise list its own output and offer to convert it again.
pub const OUTPUT_DIR: &str = "optimized";

/// What a folder holds.
pub struct Scan {
    pub entries: Vec<Entry>,
    /// Camera raw files left out of the list, so the total is not silently short.
    pub skipped_raw: usize,
    /// Files that look like images by extension but would not decode. Counted rather
    /// than dropped in silence, because a folder that reports fewer files than it
    /// holds is a bug report waiting to happen.
    pub unreadable: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub format: ImageFormat,
    pub width: u32,
    pub height: u32,
    /// Bytes on disk, not decoded size.
    pub bytes: u64,
}

impl Entry {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }

    /// Bytes per pixel of output. The number that says whether a file is carrying
    /// weight it does not need — a photographic JPEG lands near 0.2, a screenshot
    /// saved as PNG can be ten times that.
    pub fn bytes_per_pixel(&self) -> f32 {
        let pixels = (self.width as u64) * (self.height as u64);
        if pixels == 0 {
            return 0.;
        }
        self.bytes as f32 / pixels as f32
    }

    /// True when the extension disagrees with the bytes inside the file. The first
    /// folder this was ever pointed at held 169 files named `.webp`, 59 of which
    /// were PNG — the sort of thing an audit should say out loud rather than leave
    /// for someone to notice in a column.
    pub fn extension_lies(&self) -> bool {
        let Some(extension) = self.path.extension().and_then(|name| name.to_str()) else {
            // No extension is not a lie, just an omission.
            return false;
        };
        // `jpg` and `jpeg` are one format under two spellings, as are `tif` and
        // `tiff`; `extensions_str` lists every name the format answers to.
        !self
            .format
            .extensions_str()
            .contains(&extension.to_ascii_lowercase().as_str())
    }
}

/// Read one file's header. `None` when it is not an image we can read.
pub fn probe(path: &Path) -> Option<Entry> {
    let bytes = std::fs::metadata(path).ok()?.len();
    let reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let format = reader.format()?;
    let (width, height) = reader.into_dimensions().ok()?;

    Some(Entry {
        path: path.to_path_buf(),
        format,
        width,
        height,
        bytes,
    })
}

/// Decode a file, choosing the decoder by what is inside it rather than by what it
/// is called.
///
/// `image::open` picks its decoder from the extension, which is the one thing this
/// app already knows it cannot trust — `probe` reads the format from the magic bytes
/// precisely because extensions lie. Using both meant the files the audit flagged as
/// mislabelled were exactly the files it then failed to convert, thumbnail or open,
/// with no error beyond a missing row.
pub fn decode(path: &Path) -> Option<image::DynamicImage> {
    ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()
}

/// Walk a folder and probe every image in it, subfolders included.
pub fn scan(root: &Path) -> Scan {
    let mut entries = Vec::new();
    let mut skipped_raw = 0;
    let mut unreadable = 0;

    for file in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        if file
            .path()
            .components()
            .any(|part| part.as_os_str() == OUTPUT_DIR)
        {
            continue;
        }
        if is_raw(file.path()) {
            skipped_raw += 1;
            continue;
        }
        match probe(file.path()) {
            Some(entry) => entries.push(entry),
            // Only count things that claimed to be images. A README is not a failure.
            None if looks_like_an_image(file.path()) => unreadable += 1,
            None => {}
        }
    }

    // Heaviest first: the top of the list is the work worth doing.
    entries.sort_by_key(|entry| std::cmp::Reverse(entry.bytes));
    Scan {
        entries,
        skipped_raw,
        unreadable,
    }
}

/// Extension-only guess, used to decide whether a decode failure is worth reporting.
fn looks_like_an_image(path: &Path) -> bool {
    const EXTENSIONS: [&str; 9] = [
        "jpg", "jpeg", "png", "webp", "avif", "gif", "tif", "tiff", "bmp",
    ];
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
}

/// Human-readable file size. Deliberately not exact: the point is comparison.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [(u64, &str); 3] = [(1 << 30, "GB"), (1 << 20, "MB"), (1 << 10, "KB")];
    for (scale, unit) in UNITS {
        if bytes >= scale {
            return format!("{:.1} {unit}", bytes as f64 / scale as f64);
        }
    }
    format!("{bytes} B")
}

/// The short name shown in the format column.
pub fn format_name(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "JPEG",
        ImageFormat::Png => "PNG",
        ImageFormat::WebP => "WebP",
        ImageFormat::Avif => "AVIF",
        ImageFormat::Gif => "GIF",
        ImageFormat::Tiff => "TIFF",
        ImageFormat::Bmp => "BMP",
        other => other.extensions_str().first().copied().unwrap_or("?"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgb};

    fn write_sample(dir: &Path, name: &str, width: u32, height: u32) -> PathBuf {
        let path = dir.join(name);
        let buffer = ImageBuffer::from_fn(width, height, |x, y| {
            Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        buffer.save(&path).unwrap();
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("imageguide-test-{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn probes_dimensions_and_format_without_decoding() {
        let dir = temp_dir("probe");
        let path = write_sample(&dir, "sample.png", 40, 25);

        let entry = probe(&path).expect("png is readable");
        assert_eq!(entry.format, ImageFormat::Png);
        assert_eq!((entry.width, entry.height), (40, 25));
        assert_eq!(entry.bytes, std::fs::metadata(&path).unwrap().len());
    }

    /// The mislabelled files are the whole point, so they have to survive every path
    /// and not just the one that counts them. Decoding by extension meant the folder
    /// this app was built for — 169 files named `.webp`, 59 of them PNG — listed
    /// those 59 and then silently failed to convert, thumbnail or preview any of them.
    #[test]
    fn a_file_decodes_by_its_contents_not_its_name() {
        let dir = temp_dir("decode-liar");
        let honest = write_sample(&dir, "honest.png", 24, 16);
        let liar = dir.join("liar.webp");
        std::fs::copy(&honest, &liar).unwrap();

        let decoded = decode(&liar).expect("a PNG named .webp still decodes");
        assert_eq!((decoded.width(), decoded.height()), (24, 16));
        // And the audit agrees about what it actually is.
        assert_eq!(probe(&liar).unwrap().format, ImageFormat::Png);
    }

    /// The audit's best finding is a file whose name disagrees with its bytes, so
    /// the check has to be right about which disagreements are real. `jpg` and
    /// `jpeg` naming the same format is not a finding.
    #[test]
    fn an_extension_only_lies_when_it_names_another_format() {
        let png = Entry {
            path: PathBuf::from("/photos/promo.png"),
            format: ImageFormat::Png,
            width: 10,
            height: 10,
            bytes: 100,
        };
        assert!(!png.extension_lies());

        let liar = Entry {
            path: PathBuf::from("/photos/promo.webp"),
            ..png.clone()
        };
        assert!(liar.extension_lies(), "a PNG named .webp is the finding");

        let shouty = Entry {
            path: PathBuf::from("/photos/PROMO.PNG"),
            ..png.clone()
        };
        assert!(!shouty.extension_lies(), "case is not a disagreement");

        let jpeg = Entry {
            path: PathBuf::from("/photos/shot.jpg"),
            format: ImageFormat::Jpeg,
            ..png.clone()
        };
        assert!(!jpeg.extension_lies(), "jpg and jpeg are one format");

        let bare = Entry {
            path: PathBuf::from("/photos/shot"),
            ..png
        };
        assert!(!bare.extension_lies(), "no extension is not a claim");
    }

    #[test]
    fn skips_files_that_are_not_images() {
        let dir = temp_dir("skip");
        std::fs::write(dir.join("notes.txt"), "not an image").unwrap();
        write_sample(&dir, "real.png", 8, 8);

        let scanned = scan(&dir);
        assert_eq!(scanned.entries.len(), 1);
        assert_eq!(scanned.entries[0].name(), "real.png");
    }

    #[test]
    fn scan_reaches_subfolders_and_sorts_heaviest_first() {
        let dir = temp_dir("walk");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        write_sample(&dir, "small.png", 8, 8);
        write_sample(&dir.join("nested"), "big.png", 300, 300);

        let scanned = scan(&dir);
        assert_eq!(scanned.entries.len(), 2);
        assert_eq!(
            scanned.entries[0].name(),
            "big.png",
            "heaviest file sorts first"
        );
        assert!(scanned.entries[0].bytes > scanned.entries[1].bytes);
    }

    #[test]
    fn the_output_folder_is_not_audited_as_input() {
        let dir = temp_dir("output");
        write_sample(&dir, "source.png", 16, 16);
        std::fs::create_dir_all(dir.join(OUTPUT_DIR)).unwrap();
        write_sample(&dir.join(OUTPUT_DIR), "source.png", 16, 16);

        let scanned = scan(&dir);
        assert_eq!(
            scanned.entries.len(),
            1,
            "a second run must not offer to convert its own output"
        );
    }

    #[test]
    fn camera_raw_is_counted_but_not_listed() {
        let dir = temp_dir("raw");
        write_sample(&dir, "keep.png", 8, 8);
        // A raw file is skipped on its name, before anything tries to decode it.
        std::fs::write(dir.join("DSC_0001.NEF"), b"not really a nef").unwrap();
        std::fs::write(dir.join("DSC_0002.cr2"), b"nor this").unwrap();

        let scanned = scan(&dir);
        assert_eq!(scanned.entries.len(), 1);
        assert_eq!(
            scanned.skipped_raw, 2,
            "raw is counted, not silently dropped"
        );
    }

    #[test]
    fn a_broken_image_is_counted_not_dropped() {
        let dir = temp_dir("unreadable");
        write_sample(&dir, "good.png", 8, 8);
        std::fs::write(dir.join("truncated.png"), b"not a png at all").unwrap();
        std::fs::write(dir.join("notes.txt"), b"plain text").unwrap();

        let scanned = scan(&dir);
        assert_eq!(scanned.entries.len(), 1);
        assert_eq!(
            scanned.unreadable, 1,
            "a broken png counts; a text file is not a failure"
        );
    }

    #[test]
    fn raw_detection_ignores_extension_case() {
        assert!(is_raw(Path::new("a/DSC_1.NEF")));
        assert!(is_raw(Path::new("a/DSC_1.nef")));
        assert!(is_raw(Path::new("a/b.ArW")));
        assert!(!is_raw(Path::new("a/b.png")));
        assert!(!is_raw(Path::new("a/b")));
    }

    #[test]
    fn bytes_per_pixel_is_zero_for_an_empty_image() {
        let entry = Entry {
            path: PathBuf::from("x.png"),
            format: ImageFormat::Png,
            width: 0,
            height: 0,
            bytes: 100,
        };
        assert_eq!(entry.bytes_per_pixel(), 0.);
    }

    #[test]
    fn sizes_read_in_the_nearest_unit() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(5 * (1 << 20)), "5.0 MB");
    }
}
