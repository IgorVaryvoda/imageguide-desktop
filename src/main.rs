//! ImageGuide Desktop — audit a folder of images without uploading them anywhere.
//!
//! The browser tools on imageguide.dev post files to a worker to convert them. This
//! does the same work locally, so nothing leaves the machine and the folder size is
//! bounded by the disk rather than by a tab.

mod scan;

use std::path::PathBuf;

use gpui::{
    App, Bounds, Context, FontWeight, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    rgb, rgba, size, white,
};
use gpui_platform::application;
use scan::{Entry, format_bytes, format_name};

const BACKGROUND: u32 = 0x14161b;
const ROW: u32 = 0x1b1e25;
const MUTED: u32 = 0xffffff77;

struct Audit {
    root: PathBuf,
    entries: Vec<Entry>,
}

impl Audit {
    fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.bytes).sum()
    }
}

impl Render for Audit {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self.entries.iter().take(200).map(|entry| {
            div()
                .flex()
                .items_center()
                .gap_3()
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(ROW))
                .text_size(px(12.))
                .child(
                    div()
                        .flex_1()
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
                        .w(px(64.))
                        .text_color(rgba(MUTED))
                        .child(format!("{:.2} bpp", entry.bytes_per_pixel())),
                )
                .child(
                    div()
                        .w(px(72.))
                        .text_color(white())
                        .child(format_bytes(entry.bytes)),
                )
        });

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
                            .child(format!(
                                "{} images · {}",
                                self.entries.len(),
                                format_bytes(self.total_bytes())
                            )),
                    ),
            )
            .child(div().flex().flex_col().gap_1().children(rows))
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

    let entries = scan::scan(&root);
    println!(
        "{} images, {} on disk",
        entries.len(),
        format_bytes(entries.iter().map(|entry| entry.bytes).sum())
    );

    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("imageguide".to_string()),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Audit { root, entries }),
        )
        .unwrap();
        cx.activate(true);
    });
}
