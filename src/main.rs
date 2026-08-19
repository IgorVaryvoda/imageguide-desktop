//! ImageGuide Desktop — audit a folder of images without uploading them anywhere.
//!
//! The browser tools on imageguide.dev post files to a worker to convert them. This
//! does the same work locally, so nothing leaves the machine and the folder size is
//! bounded by the disk rather than by a tab.

mod scan;
mod thumbs;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    App, Bounds, Context, FontWeight, RenderImage, Window, WindowBounds, WindowOptions, div, img,
    prelude::*, px, rgb, rgba, size, uniform_list, white,
};
use gpui_platform::application;
use scan::{Entry, format_bytes, format_name};

const BACKGROUND: u32 = 0x14161b;
const ROW: u32 = 0x1b1e25;
const MUTED: u32 = 0xffffff77;
const ROW_HEIGHT: f32 = 60.;

struct Audit {
    root: PathBuf,
    entries: Vec<Entry>,
    skipped_raw: usize,
    /// Decoded thumbnails, keyed by their row. Only rows that have been on screen are
    /// in here; a folder of 5,000 images never decodes 5,000 files.
    thumbs: HashMap<usize, Arc<RenderImage>>,
    /// Rows already handed to a background thread, so scrolling past one twice does
    /// not decode it twice.
    requested: HashSet<usize>,
}

impl Audit {
    fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.bytes).sum()
    }

    /// Kick off decoding for a row, unless it is already loaded or in flight.
    fn request_thumb(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.thumbs.contains_key(&index) || !self.requested.insert(index) {
            return;
        }
        let Some(path) = self.entries.get(index).map(|entry| entry.path.clone()) else {
            return;
        };

        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move { thumbs::load(&path, thumbs::THUMB_EDGE) })
                .await;

            if let Some(image) = loaded {
                let _ = this.update(cx, |audit, cx| {
                    audit.thumbs.insert(index, image);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn row(&self, index: usize) -> gpui::Stateful<gpui::Div> {
        let Some(entry) = self.entries.get(index) else {
            return div().id(index);
        };
        let thumb = self.thumbs.get(&index).cloned();

        div()
            .id(index)
            .flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_3()
            .h(px(ROW_HEIGHT - 4.))
            .rounded_md()
            .bg(rgb(ROW))
            .text_size(px(12.))
            .child(
                // A fixed slot, so rows do not jump as thumbnails arrive.
                div()
                    .w(px(52.))
                    .h(px(48.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .bg(rgba(0xffffff0d))
                    .when_some(thumb, |slot, image| {
                        slot.child(img(image).max_w(px(52.)).max_h(px(48.)))
                    }),
            )
            .child(
                // min_w_0 lets a long filename shrink instead of shoving the
                // number columns off the right edge.
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_color(white())
                    .child(entry.name()),
            )
            .child(
                div()
                    .w(px(52.))
                    .text_color(rgb(0x8ab4ff))
                    .child(format_name(entry.format)),
            )
            .child(
                div()
                    .w(px(96.))
                    .text_color(rgba(MUTED))
                    .child(format!("{}×{}", entry.width, entry.height)),
            )
            .child(
                div()
                    .w(px(76.))
                    .text_color(rgba(MUTED))
                    .child(format!("{:.2} bpp", entry.bytes_per_pixel())),
            )
            .child(
                div()
                    .w(px(76.))
                    .text_color(white())
                    .child(format_bytes(entry.bytes)),
            )
    }
}

impl Render for Audit {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.entries.len();

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .bg(rgb(BACKGROUND))
            .font_family("sans-serif")
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_3()
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(white())
                            .child(self.root.display().to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgba(MUTED))
                            .child(match self.skipped_raw {
                                0 => format!(
                                    "{count} images · {}",
                                    format_bytes(self.total_bytes())
                                ),
                                skipped => format!(
                                    "{count} images · {} · {skipped} camera raw skipped",
                                    format_bytes(self.total_bytes())
                                ),
                            }),
                    ),
            )
            .child(
                uniform_list(
                    "images",
                    count,
                    cx.processor(|audit, range: std::ops::Range<usize>, _window, cx| {
                        // Decode only what the viewport asked for.
                        range
                            .map(|index| {
                                audit.request_thumb(index, cx);
                                audit.row(index)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .h_full(),
            )
    }
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    if !root.is_dir() {
        eprintln!("imageguide: {} is not a folder", root.display());
        std::process::exit(2);
    }

    let scanned = scan::scan(&root);
    let entries = scanned.entries;
    println!(
        "{} images, {} on disk, {} camera raw skipped",
        entries.len(),
        format_bytes(entries.iter().map(|entry| entry.bytes).sum()),
        scanned.skipped_raw
    );

    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("imageguide".to_string()),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| Audit {
                    root,
                    entries,
                    skipped_raw: scanned.skipped_raw,
                    thumbs: HashMap::new(),
                    requested: HashSet::new(),
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
