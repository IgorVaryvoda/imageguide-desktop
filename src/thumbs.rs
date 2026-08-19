//! Turn a file on disk into something the GPU can draw.
//!
//! Thumbnails are decoded at full size and scaled down once. There is no cheaper way
//! for JPEG and PNG, which is exactly why this happens off the main thread and only
//! for rows that are actually on screen.

use std::path::Path;
use std::sync::Arc;

use gpui::RenderImage;
use image::{Frame, RgbaImage};

/// Longest edge of a generated thumbnail, in pixels.
pub const THUMB_EDGE: u32 = 96;

/// Decode `path`, scale it to fit `edge`, and hand back something `img()` can draw.
/// Returns `None` for anything that fails to decode, which the caller shows as a gap
/// rather than an error — a folder of holiday photos will contain a broken file.
pub fn load(path: &Path, edge: u32) -> Option<Arc<RenderImage>> {
    let decoded = crate::scan::decode(path)?;
    // `thumbnail` preserves the aspect ratio and fits inside the box.
    let scaled = decoded.thumbnail(edge, edge).into_rgba8();
    Some(Arc::new(RenderImage::new(vec![Frame::new(to_bgra(
        scaled,
    ))])))
}

/// `RenderImage` wants BGRA. The `image` crate gives RGBA. Swap in place rather than
/// allocating a second buffer per thumbnail.
pub(crate) fn to_bgra(mut image: RgbaImage) -> RgbaImage {
    for pixel in image.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    image
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    #[test]
    fn swaps_red_and_blue_and_leaves_alpha_alone() {
        let red = ImageBuffer::from_pixel(1, 1, Rgba([255u8, 10, 0, 200]));
        let swapped = to_bgra(red);
        assert_eq!(swapped.into_raw(), vec![0, 10, 255, 200]);
    }

    #[test]
    fn scales_to_fit_the_box_and_keeps_the_aspect_ratio() {
        let dir = std::env::temp_dir().join("imageguide-test-thumb");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("wide.png");
        ImageBuffer::from_pixel(400, 100, Rgba([1u8, 2, 3, 255]))
            .save(&path)
            .unwrap();

        let thumb = load(&path, 96).expect("png decodes");
        let size = thumb.size(0);
        assert_eq!(u32::from(size.width), 96, "long edge fills the box");
        assert_eq!(u32::from(size.height), 24, "4:1 aspect ratio is preserved");
    }

    #[test]
    fn a_file_that_is_not_an_image_is_skipped_rather_than_fatal() {
        let dir = std::env::temp_dir().join("imageguide-test-thumb-bad");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.png");
        std::fs::write(&path, b"this is not a png").unwrap();

        assert!(load(&path, 96).is_none());
    }
}
