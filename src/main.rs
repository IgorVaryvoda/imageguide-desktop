//! ImageGuide Desktop — audit a folder of images without uploading them anywhere.
//!
//! The browser tools on imageguide.dev post files to a worker to convert them. This
//! does the same work locally, so nothing leaves the machine and the folder size is
//! bounded by the disk rather than by a tab.

mod compare;
mod convert;
mod scan;
mod thumbs;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    App, Bounds, Context, FontWeight, RenderImage, Window, WindowBounds, WindowOptions, div, img,
    prelude::*, px, rgb, rgba, size, uniform_list, white,
};
use gpui_platform::application;
use compare::Pair;
use convert::{Format, MaxEdge, Quality};
use scan::{Entry, format_bytes, format_name};

const BACKGROUND: u32 = 0x14161b;
const ROW: u32 = 0x1b1e25;
const MUTED: u32 = 0xffffff77;
const ROW_HEIGHT: f32 = 60.;
const ACCENT: u32 = 0x8ab4ff;
const GOOD: u32 = 0x5ec27a;
/// How many files encode at once. Each one holds a fully decoded image in memory, so
/// this is a memory bound as much as a CPU one.
const WORKERS: usize = 8;

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
    format: Format,
    quality: Quality,
    max_edge: MaxEdge,
    /// Rows ticked for conversion. Empty means "all of them".
    selected: HashSet<usize>,
    /// Encoded size per row, filled in as conversion progresses.
    results: HashMap<usize, u64>,
    converting: bool,
    /// Files that could not be decoded or written, so the count is honest.
    failed: usize,
    /// The open side-by-side view, if any.
    compare: Option<Comparison>,
    /// The last pair built, kept so closing and reopening the same image is instant.
    // ponytail: one entry. A pair holds two full-size RGBA buffers — 165 MB for a
    // 5568x3712 photo — so a bigger cache would need a byte budget, not a count.
    cached: Option<(compare::Key, Arc<Pair>)>,
}

struct Comparison {
    index: usize,
    /// `None` while the two sides are still decoding.
    pair: Option<Arc<Pair>>,
    /// Where the divider sits, 0 to 1 across the viewport.
    split: f32,
    /// How far the image is dragged from centre, in pixels.
    pan: (f32, f32),
    /// Pointer position when the current drag began, and the pan it started from.
    drag: Option<((f32, f32), (f32, f32))>,
}

impl Audit {
    fn total_bytes(&self) -> u64 {
        self.entries.iter().map(|entry| entry.bytes).sum()
    }

    /// The rows a conversion would touch. An empty selection means the whole folder,
    /// so the common case needs no ticking.
    fn targets(&self) -> Vec<usize> {
        if self.selected.is_empty() {
            (0..self.entries.len()).collect()
        } else {
            let mut rows: Vec<usize> = self.selected.iter().copied().collect();
            rows.sort_unstable();
            rows
        }
    }

    /// Bytes before and after, counting only the files actually converted. Comparing
    /// against the whole folder mid-run would report a fake saving.
    fn converted_totals(&self) -> (u64, u64) {
        self.results.iter().fold((0, 0), |(before, after), (index, bytes)| {
            let source = self.entries.get(*index).map_or(0, |entry| entry.bytes);
            (before + source, after + bytes)
        })
    }

    fn start_conversion(&mut self, cx: &mut Context<Self>) {
        if self.converting {
            return;
        }
        self.converting = true;
        self.results.clear();
        self.failed = 0;
        cx.notify();

        let root = self.root.clone();
        let out_dir = self.root.join(scan::OUTPUT_DIR);
        let quality = self.quality;
        let format = self.format;
        let max_edge = self.max_edge;
        let sources: Vec<(usize, PathBuf)> = self
            .targets()
            .into_iter()
            .filter_map(|index| Some((index, self.entries.get(index)?.path.clone())))
            .collect();

        cx.spawn(async move |this, cx| {
            for chunk in sources.chunks(WORKERS) {
                // Spawn a bounded batch, then wait for it. Queueing all 5,000 at once
                // would be fine for the executor and terrible for memory.
                let batch: Vec<_> = chunk
                    .iter()
                    .map(|(index, path)| {
                        let (index, path) = (*index, path.clone());
                        let (root, out_dir) = (root.clone(), out_dir.clone());
                        cx.background_executor().spawn(async move {
                            (
                                index,
                                convert::convert_file(
                                    &root, &path, &out_dir, format, quality, max_edge,
                                ),
                            )
                        })
                    })
                    .collect();

                let mut done = Vec::with_capacity(batch.len());
                for task in batch {
                    done.push(task.await);
                }

                if this
                    .update(cx, |audit, cx| {
                        for (index, result) in done {
                            match result {
                                Some(converted) => {
                                    audit.results.insert(index, converted.bytes);
                                }
                                None => audit.failed += 1,
                            }
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    return;
                }
            }

            let _ = this.update(cx, |audit, cx| {
                audit.converting = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// Point the audit at a new folder, or a single file. Everything derived from the
    /// old one is dropped: stale thumbnails and conversion results would be lies.
    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let single = path.is_file();
        let (scanned, root) = if single {
            let Some(entry) = scan::probe(&path) else {
                return;
            };
            let parent = path.parent().unwrap_or(Path::new(".")).to_path_buf();
            (
                scan::Scan {
                    entries: vec![entry],
                    skipped_raw: 0,
                },
                parent,
            )
        } else if path.is_dir() {
            (scan::scan(&path), path)
        } else {
            return;
        };

        self.root = root;
        self.entries = scanned.entries;
        self.skipped_raw = scanned.skipped_raw;
        self.thumbs.clear();
        self.requested.clear();
        self.selected.clear();
        self.results.clear();
        self.failed = 0;
        self.compare = None;
        self.cached = None;
        cx.notify();

        if single {
            self.open_compare(0, cx);
        }
    }

    /// Ask the desktop for a folder or a file. The dialog runs off the main thread so
    /// the window keeps drawing while it is open.
    fn pick(&mut self, folders: bool, cx: &mut Context<Self>) {
        let start = self.root.clone();
        cx.spawn(async move |this, cx| {
            let chosen = cx
                .background_executor()
                .spawn(async move {
                    let dialog = rfd::FileDialog::new().set_directory(&start);
                    if folders {
                        dialog.pick_folder()
                    } else {
                        dialog.pick_file()
                    }
                })
                .await;

            if let Some(path) = chosen {
                let _ = this.update(cx, |audit, cx| audit.open_path(path, cx));
            }
        })
        .detach();
    }

    fn toolbar_button(
        &self,
        id: &'static str,
        text: &'static str,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        div()
            .id(id)
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(12.))
            .bg(rgba(0xffffff08))
            .text_color(rgba(MUTED))
            .hover(|style| style.bg(rgba(0xffffff1f)).text_color(white()))
            .child(text)
            .on_click(cx.listener(move |audit, _, _, cx| on_click(audit, cx)))
    }

    /// Open the side-by-side view for a row and start building both sides.
    fn open_compare(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(path) = self.entries.get(index).map(|entry| entry.path.clone()) else {
            return;
        };
        self.compare = Some(Comparison {
            index,
            pair: None,
            split: 0.5,
            pan: (0., 0.),
            drag: None,
        });
        cx.notify();

        let quality = self.quality;
        let format = self.format;
        let max_edge = self.max_edge;
        let key = compare::Key::new(&path, format, quality, max_edge);

        // Same image, same settings: skip the encoder entirely.
        if let Some((cached_key, pair)) = self.cached.as_ref()
            && *cached_key == key
        {
            if let Some(comparison) = self.compare.as_mut() {
                comparison.pair = Some(pair.clone());
            }
            cx.notify();
            return;
        }

        cx.spawn(async move |this, cx| {
            let built = cx
                .background_executor()
                .spawn(async move { compare::build(&path, format, quality, max_edge) })
                .await
                .map(Arc::new);

            let _ = this.update(cx, |audit, cx| {
                if let Some(pair) = built.as_ref() {
                    audit.cached = Some((key, pair.clone()));
                }
                // Ignore a result the user already navigated away from.
                if let Some(comparison) = audit.compare.as_mut()
                    && comparison.index == index
                {
                    comparison.pair = built;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Full-window, 1:1 pixels. Fitting a 5568px photo into a 900px window would hide
    /// exactly the artefacts this view exists to show, so the image is drawn at native
    /// size and centred, and the divider follows the pointer.
    fn compare_view(
        &self,
        comparison: &Comparison,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let viewport = window.viewport_size();
        let (view_w, view_h) = (f32::from(viewport.width), f32::from(viewport.height));
        let entry = self.entries.get(comparison.index);
        let source_bytes = entry.map_or(0, |entry| entry.bytes);
        let name = entry.map(|entry| entry.name()).unwrap_or_default();

        let mut stage = div()
            .id("compare-stage")
            .absolute()
            .inset_0()
            .overflow_hidden()
            .bg(rgb(0x0b0d10))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|audit, event: &gpui::MouseDownEvent, _, cx| {
                    if let Some(comparison) = audit.compare.as_mut() {
                        let at = (f32::from(event.position.x), f32::from(event.position.y));
                        comparison.drag = Some((at, comparison.pan));
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|audit, _: &gpui::MouseUpEvent, _, cx| {
                    if let Some(comparison) = audit.compare.as_mut() {
                        comparison.drag = None;
                        cx.notify();
                    }
                }),
            )
            .on_mouse_move(cx.listener(move |audit, event: &gpui::MouseMoveEvent, _, cx| {
                let Some(comparison) = audit.compare.as_mut() else {
                    return;
                };
                let at = (f32::from(event.position.x), f32::from(event.position.y));

                match comparison.drag {
                    // Held: pan both sides together, so they stay in register.
                    Some((from, start_pan)) => {
                        comparison.pan = (
                            start_pan.0 + at.0 - from.0,
                            start_pan.1 + at.1 - from.1,
                        );
                    }
                    // Free: the divider tracks the pointer.
                    None => comparison.split = (at.0 / view_w).clamp(0., 1.),
                }
                cx.notify();
            }));

        if let Some(pair) = comparison.pair.as_ref() {
            let (image_w, image_h) = (pair.width as f32, pair.height as f32);
            // Negative when the image is larger than the window: that is the crop.
            let left = (view_w - image_w) / 2. + comparison.pan.0;
            let top = (view_h - image_h) / 2. + comparison.pan.1;
            let divider = view_w * comparison.split;

            let placed = |image: &Arc<gpui::RenderImage>| {
                div()
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(image_w))
                    .h(px(image_h))
                    .child(img(image.clone()).w(px(image_w)).h(px(image_h)))
            };

            stage = stage
                .child(placed(&pair.converted))
                .child(
                    // The original, clipped to everything left of the divider. Its
                    // child keeps full width so both sides stay in register.
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .h_full()
                        .w(px(divider))
                        .overflow_hidden()
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .w(px(view_w))
                                .h(px(view_h))
                                .child(placed(&pair.original)),
                        ),
                )
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .left(px(divider - 1.))
                        .w(px(2.))
                        .h_full()
                        .bg(rgba(0xffffffcc)),
                )
                .child(label(
                    px(12.),
                    px(12.),
                    format!("original · {}", format_bytes(source_bytes)),
                    white(),
                ))
                .child(label(
                    px(view_w - 240.),
                    px(12.),
                    format!(
                        "{} {} · {} · {:+.0}%",
                        self.format.label(),
                        self.quality.label(),
                        format_bytes(pair.converted_bytes),
                        pair.saving_percent(source_bytes)
                    ),
                    rgb(GOOD),
                ))
                .child(label(
                    px(12.),
                    px(view_h - 32.),
                    format!("{name} · {image_w}×{image_h} at 1:1 · drag to pan"),
                    rgba(MUTED),
                ));
        } else {
            stage = stage.child(label(px(12.), px(12.), "decoding…".to_string(), rgba(MUTED)));
        }

        stage
            .child(
            div()
                .id("compare-close")
                .absolute()
                .top(px(8.))
                .right(px(8.))
                .px_3()
                .py_1()
                .rounded_md()
                .cursor_pointer()
                .text_size(px(12.))
                .bg(rgba(0x000000aa))
                .text_color(white())
                .child("close")
                .on_click(cx.listener(|audit, _, _, cx| {
                    audit.compare = None;
                    cx.notify();
                })),
            )
            .into_any_element()
    }

    fn size_button(&self, max_edge: MaxEdge, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.max_edge == max_edge;
        div()
            .id(gpui::SharedString::from(format!("size-{}", max_edge.label())))
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(12.))
            .bg(if selected { rgba(0xffffff1f) } else { rgba(0xffffff08) })
            .text_color(if selected { rgb(ACCENT) } else { rgba(MUTED) })
            .child(max_edge.label())
            .on_click(cx.listener(move |audit, _, _, cx| {
                audit.max_edge = max_edge;
                audit.results.clear();
                cx.notify();
            }))
    }

    fn format_button(&self, format: Format, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.format == format;
        div()
            .id(gpui::SharedString::from(format.label()))
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(12.))
            .bg(if selected { rgba(0xffffff1f) } else { rgba(0xffffff08) })
            .text_color(if selected { rgb(ACCENT) } else { rgba(MUTED) })
            .child(format.label())
            .on_click(cx.listener(move |audit, _, _, cx| {
                audit.format = format;
                // Results describe the old format; keeping them would mislabel them.
                audit.results.clear();
                cx.notify();
            }))
    }

    fn quality_button(&self, quality: Quality, cx: &mut Context<Self>) -> impl IntoElement {
        let selected = self.quality == quality;
        div()
            .id(gpui::SharedString::from(quality.label()))
            .px_2()
            .py_1()
            .rounded_md()
            .cursor_pointer()
            .text_size(px(12.))
            .bg(if selected { rgba(0xffffff1f) } else { rgba(0xffffff08) })
            .text_color(if selected { rgb(ACCENT) } else { rgba(MUTED) })
            .child(quality.label())
            .on_click(cx.listener(move |audit, _, _, cx| {
                audit.quality = quality;
                cx.notify();
            }))
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

    fn row(&self, index: usize, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
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
            .cursor_pointer()
            .hover(|style| style.bg(rgba(0xffffff14)))
            .on_click(cx.listener(move |audit, _, _, cx| audit.open_compare(index, cx)))
            .text_size(px(12.))
            .child({
                let ticked = self.selected.contains(&index);
                div()
                    .id(("tick", index))
                    .w(px(16.))
                    .h(px(16.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .border_1()
                    .border_color(if ticked { rgb(ACCENT) } else { rgba(0xffffff33) })
                    .bg(if ticked { rgb(ACCENT) } else { rgba(0x00000000) })
                    .text_size(px(10.))
                    .text_color(rgb(BACKGROUND))
                    .child(if ticked { "✓" } else { "" })
                    .on_click(cx.listener(move |audit, _, _, cx| {
                        // Without this the click also opens the comparison behind it.
                        cx.stop_propagation();
                        if !audit.selected.remove(&index) {
                            audit.selected.insert(index);
                        }
                        cx.notify();
                    }))
            })
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
            .child(
                div()
                    .w(px(132.))
                    .when_some(self.results.get(&index), |slot, converted| {
                        let saved = entry.bytes.saturating_sub(*converted);
                        let percent = if entry.bytes == 0 {
                            0.
                        } else {
                            saved as f32 / entry.bytes as f32 * 100.
                        };
                        // A file that grew is a real outcome, not a rounding error:
                        // re-encoding an already-optimal JPEG usually costs bytes.
                        let grew = *converted > entry.bytes;
                        slot.text_color(if grew { rgba(MUTED) } else { rgb(GOOD) })
                            .child(if grew {
                                format!("→ {} (larger)", format_bytes(*converted))
                            } else {
                                format!("→ {}  −{percent:.0}%", format_bytes(*converted))
                            })
                    }),
            )
    }
}

/// A positioned text overlay for the compare view.
fn label(left: gpui::Pixels, top: gpui::Pixels, text: String, colour: impl Into<gpui::Hsla>) -> impl IntoElement {
    div()
        .absolute()
        .left(left)
        .top(top)
        .px_2()
        .py_1()
        .rounded_md()
        .bg(rgba(0x000000aa))
        .text_size(px(12.))
        .text_color(colour)
        .child(text)
}

impl Render for Audit {
    // Three shapes share this method — empty state, comparison, and the list — so it
    // erases to one type rather than making the caller's `impl Trait` pick a winner.
    #[allow(refining_impl_trait)]
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let count = self.entries.len();

        if self.entries.is_empty() && !self.root.is_dir() {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .bg(rgb(BACKGROUND))
                .font_family("sans-serif")
                .child(
                    div()
                        .text_size(px(14.))
                        .text_color(rgba(MUTED))
                        .child("Drop a folder or an image here"),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .child(self.toolbar_button("empty-folder", "Open folder…", cx, |audit, cx| {
                            audit.pick(true, cx)
                        }))
                        .child(self.toolbar_button("empty-file", "Open image…", cx, |audit, cx| {
                            audit.pick(false, cx)
                        })),
                )
                .on_drop(cx.listener(|audit, paths: &gpui::ExternalPaths, _, cx| {
                    if let Some(path) = paths.paths().first() {
                        audit.open_path(path.clone(), cx);
                    }
                }))
                .into_any_element();
        }

        if let Some(comparison) = self.compare.take() {
            // Taken and put back so the view can borrow `self` immutably while the
            // listeners it builds hold a mutable handle to the same entity.
            let view = self.compare_view(&comparison, window, cx);
            self.compare = Some(comparison);
            return div().size_full().relative().child(view).into_any_element();
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .bg(rgb(BACKGROUND))
            .font_family("sans-serif")
            .on_drop(cx.listener(|audit, paths: &gpui::ExternalPaths, _, cx| {
                if let Some(path) = paths.paths().first() {
                    audit.open_path(path.clone(), cx);
                }
            }))
            .child(
                div()
                    .flex()
                    .items_center()
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
                    )
                    .child(div().flex_1())
                    .child(self.toolbar_button("open-folder", "Folder…", cx, |audit, cx| {
                        audit.pick(true, cx)
                    }))
                    .child(self.toolbar_button("open-file", "Image…", cx, |audit, cx| {
                        audit.pick(false, cx)
                    })),
            )
            .child(
                // Controls get their own line. Thirteen of them on the title row ran
                // the Convert button off the right edge of the window.
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.size_button(MaxEdge::FULL, cx))
                    .child(self.size_button(MaxEdge(Some(2400)), cx))
                    .child(self.size_button(MaxEdge(Some(1600)), cx))
                    .child(self.size_button(MaxEdge(Some(1000)), cx))
                    .child(div().w(px(12.)))
                    .child(self.format_button(Format::WebP, cx))
                    .child(self.format_button(Format::Avif, cx))
                    .child(div().w(px(12.)))
                    .child(self.quality_button(Quality::lossy(60.), cx))
                    .child(self.quality_button(Quality::lossy(80.), cx))
                    .child(self.quality_button(Quality::LOSSLESS, cx))
                    .child(div().flex_1())
                    .child(
                        div()
                            .id("convert")
                            .px_3()
                            .py_1()
                            .rounded_md()
                            .cursor_pointer()
                            .text_size(px(12.))
                            .bg(rgba(0xffffff1f))
                            .text_color(white())
                            .hover(|style| style.bg(rgba(0xffffff33)))
                            .child(if self.converting {
                                format!("Converting {}/{}", self.results.len() + self.failed, self.targets().len())
                            } else if self.selected.is_empty() {
                                format!("Convert all to {}", self.format.label().to_uppercase())
                            } else {
                                format!(
                                    "Convert {} to {}",
                                    self.selected.len(),
                                    self.format.label().to_uppercase()
                                )
                            })
                            .on_click(cx.listener(|audit, _, _, cx| audit.start_conversion(cx))),
                    )
                    .when(!self.selected.is_empty(), |row| {
                        row.child(self.toolbar_button("select-none", "Clear", cx, |audit, cx| {
                            audit.selected.clear();
                            cx.notify();
                        }))
                    }),
            )
            .when(!self.results.is_empty(), |shell| {
                let (before, after) = self.converted_totals();
                let saved = before.saturating_sub(after);
                let percent = if before == 0 {
                    0.
                } else {
                    saved as f32 / before as f32 * 100.
                };
                shell.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(GOOD))
                        .child(match self.failed {
                            0 => format!(
                                "{} converted · {} → {} · saved {} ({percent:.0}%)",
                                self.results.len(),
                                format_bytes(before),
                                format_bytes(after),
                                format_bytes(saved)
                            ),
                            failed => format!(
                                "{} converted · saved {} ({percent:.0}%) · {failed} failed",
                                self.results.len(),
                                format_bytes(saved)
                            ),
                        }),
                )
            })
            .child(
                uniform_list(
                    "images",
                    count,
                    cx.processor(|audit, range: std::ops::Range<usize>, _window, cx| {
                        // Decode only what the viewport asked for.
                        range
                            .map(|index| {
                                audit.request_thumb(index, cx);
                                audit.row(index, cx)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1(),
            )
            .into_any_element()
    }
}

/// `imageguide <folder> [--convert] [--quality N | --lossless]`
struct Args {
    /// `None` when launched with no path: the window opens on its empty state.
    root: Option<PathBuf>,
    convert: bool,
    format: Format,
    quality: Quality,
    max_edge: MaxEdge,
}

fn parse_args() -> Args {
    let mut root = None;
    let mut convert = false;
    let mut format = Format::WebP;
    let mut quality = Quality::lossy(80.);
    let mut max_edge = MaxEdge::FULL;
    let mut rest = std::env::args().skip(1);

    while let Some(argument) = rest.next() {
        match argument.as_str() {
            "--convert" => convert = true,
            "--avif" => format = Format::Avif,
            "--max-edge" => {
                if let Some(value) = rest.next().and_then(|value| value.parse().ok()) {
                    max_edge = MaxEdge(Some(value));
                }
            }
            "--webp" => format = Format::WebP,
            "--lossless" => quality = Quality::LOSSLESS,
            "--quality" => {
                if let Some(value) = rest.next().and_then(|value| value.parse().ok()) {
                    quality = Quality::lossy(value);
                }
            }
            _ => root = Some(PathBuf::from(argument)),
        }
    }

    Args {
        root,
        convert,
        format,
        quality,
        max_edge,
    }
}

/// Convert without opening a window, so the same work is scriptable and testable.
fn convert_headless(
    root: &std::path::Path,
    entries: &[Entry],
    format: Format,
    quality: Quality,
    max_edge: MaxEdge,
) {
    let out_dir = root.join(scan::OUTPUT_DIR);
    let (mut before, mut after, mut failed) = (0u64, 0u64, 0usize);

    for entry in entries {
        match convert::convert_file(root, &entry.path, &out_dir, format, quality, max_edge) {
            Some(converted) => {
                before += entry.bytes;
                after += converted.bytes;
                let delta = entry.bytes as i64 - converted.bytes as i64;
                let percent = delta as f64 / entry.bytes.max(1) as f64 * 100.;
                let resized = if converted.width == entry.width {
                    String::new()
                } else {
                    format!("  {}x{}", converted.width, converted.height)
                };
                println!(
                    "{:<52} {:>9} -> {:>9}  {percent:+.0}%{resized}",
                    entry.name(),
                    format_bytes(entry.bytes),
                    format_bytes(converted.bytes)
                );
            }
            None => {
                failed += 1;
                println!("{:<52} failed", entry.name());
            }
        }
    }

    let saved = before.saturating_sub(after);
    let percent = saved as f64 / before.max(1) as f64 * 100.;
    println!(
        "\n{} converted to {} at {} ({}): {} -> {}, saved {} ({percent:.0}%){}",
        entries.len() - failed,
        format.label(),
        quality.label(),
        max_edge.label(),
        format_bytes(before),
        format_bytes(after),
        format_bytes(saved),
        if failed == 0 {
            String::new()
        } else {
            format!(", {failed} failed")
        }
    );
    println!("written to {}", out_dir.display());
}

fn main() {
    let args = parse_args();

    let Some(target) = args.root.clone() else {
        if args.convert {
            eprintln!("imageguide: --convert needs a folder");
            std::process::exit(2);
        }
        // No path given: open the window on its empty state and let the user pick.
        return run_window(
            PathBuf::new(),
            Vec::new(),
            0,
            false,
            args.format,
            args.quality,
            args.max_edge,
        );
    };

    // A single file opens straight into the comparison. A folder opens the audit.
    let open_single = target.is_file();
    if !target.is_dir() && !open_single {
        eprintln!("imageguide: {} is not a file or folder", target.display());
        std::process::exit(2);
    }

    let (scanned, root) = if open_single {
        let parent = target.parent().unwrap_or(Path::new(".")).to_path_buf();
        let Some(entry) = scan::probe(&target) else {
            eprintln!("imageguide: {} is not an image", target.display());
            std::process::exit(2);
        };
        (
            scan::Scan {
                entries: vec![entry],
                skipped_raw: 0,
            },
            parent,
        )
    } else {
        (scan::scan(&target), target.clone())
    };
    let entries = scanned.entries;
    println!(
        "{} images, {} on disk, {} camera raw skipped",
        entries.len(),
        format_bytes(entries.iter().map(|entry| entry.bytes).sum()),
        scanned.skipped_raw
    );

    if args.convert {
        convert_headless(&root, &entries, args.format, args.quality, args.max_edge);
        return;
    }

    run_window(
        root,
        entries,
        scanned.skipped_raw,
        open_single,
        args.format,
        args.quality,
        args.max_edge,
    );
}

fn run_window(
    root: PathBuf,
    entries: Vec<Entry>,
    skipped_raw: usize,
    open_single: bool,
    format: Format,
    quality: Quality,
    max_edge: MaxEdge,
) {
    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("imageguide".to_string()),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|cx| {
                    let mut audit = Audit {
                        root,
                        entries,
                        skipped_raw,
                        thumbs: HashMap::new(),
                        requested: HashSet::new(),
                        format,
                        quality,
                        max_edge,
                        selected: HashSet::new(),
                        cached: None,
                        results: HashMap::new(),
                        converting: false,
                        failed: 0,
                        compare: None,
                    };
                    if open_single {
                        audit.open_compare(0, cx);
                    }
                    audit
                })
            },
        )
        .unwrap();
        cx.activate(true);
    });
}
