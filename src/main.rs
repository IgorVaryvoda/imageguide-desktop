//! ImageGuide Desktop — audit a folder of images without uploading them anywhere.
//!
//! The browser tools on imageguide.dev post files to a worker to convert them. This
//! does the same work locally, so nothing leaves the machine and the folder size is
//! bounded by the disk rather than by a tab.

mod compare;
mod convert;
mod scan;
mod settings;
mod thumbs;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use compare::Pair;
use convert::{Format, MaxEdge, Quality};
use gpui::{
    App, Bounds, Context, FocusHandle, FontWeight, RenderImage, Window, WindowBounds,
    WindowOptions, div, img, prelude::*, px, rgb, rgba, size, uniform_list, white,
};
use gpui_component::button::{Button, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::progress::Progress;
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::{ActiveTheme, Root, Selectable, Sizable};
use gpui_platform::application;
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
/// Gallery tile size, and how many fit a row. Fixed rather than responsive because
/// `uniform_list` needs every row the same height to virtualise at all.
const TILE: f32 = 168.;
const TILE_COLUMNS: usize = 5;
/// Files encoded to project a total. More is more accurate and much slower — an AVIF
/// sample is a second or two each.
const SAMPLE_SIZE: usize = 4;
/// Settling time before sampling, so dragging the slider does not start a run per pixel.
const ESTIMATE_DELAY: Duration = Duration::from_millis(400);

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
    /// Drives the quality slider. Its own entity, because that is how the component
    /// reports drags.
    quality_slider: gpui::Entity<SliderState>,
    /// Rows ticked for conversion. Empty means "all of them".
    selected: HashSet<usize>,
    /// Encoded size per row, filled in as conversion progresses.
    results: HashMap<usize, u64>,
    converting: bool,
    /// Names of files a conversion could not read or write. Kept rather than counted,
    /// because "3 failed" without saying which is not a report.
    failures: Vec<String>,
    /// Files in the folder that claim to be images and will not decode.
    unreadable: usize,
    /// A drag is hovering over the window.
    drag_over: bool,
    /// The open side-by-side view, if any.
    compare: Option<Comparison>,
    /// How the list is ordered.
    sort: Sort,
    /// Indices into `entries`, filtered and sorted. `entries` itself never moves, so
    /// thumbnails, ticks and results stay attached to their file through both.
    visible: Vec<usize>,
    /// Substring the name must contain, lowercased. Empty shows everything.
    filter: String,
    /// Backs the filter box.
    filter_input: gpui::Entity<InputState>,
    /// Row the keyboard is on, as a position in `visible`.
    cursor: usize,
    /// List or gallery.
    grid: bool,
    /// Projected output size for the current settings, and how many files were
    /// actually encoded to get it.
    estimate: Option<(u64, usize)>,
    /// Bumped on every settings change so a slow sample can tell it is stale. Dragging
    /// the quality slider fires dozens of these.
    estimate_generation: u64,
    /// Keyboard target. Without one the window gets no key events at all.
    focus: FocusHandle,
    /// Last title pushed to the compositor, so render does not set it every frame.
    titled: String,
    /// Last state written to disk, so render only writes when it changes.
    settings: settings::Settings,
    /// The last pair built, kept so closing and reopening the same image is instant.
    // ponytail: one entry. A pair holds two full-size RGBA buffers — 165 MB for a
    // 5568x3712 photo — so a bigger cache would need a byte budget, not a count.
    cached: Option<(compare::Key, Arc<Pair>)>,
}

/// List order. Every column is sortable, and clicking the active one reverses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    column: Column,
    descending: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Column {
    Name,
    Format,
    Pixels,
    Density,
    Weight,
}

impl Column {
    fn title(&self) -> &'static str {
        match self {
            Column::Name => "Name",
            Column::Format => "Format",
            Column::Pixels => "Size",
            Column::Density => "bpp",
            Column::Weight => "Weight",
        }
    }
}

/// Ties fall back to the filename so the order is stable between runs — a list that
/// reshuffles itself is worse than one sorted badly.
fn compare_entries(a: &Entry, b: &Entry, sort: Sort) -> std::cmp::Ordering {
    {
        let ordering = match sort.column {
            Column::Name => a.name().to_lowercase().cmp(&b.name().to_lowercase()),
            Column::Format => format_name(a.format).cmp(format_name(b.format)),
            Column::Pixels => {
                (a.width as u64 * a.height as u64).cmp(&(b.width as u64 * b.height as u64))
            }
            Column::Density => a
                .bytes_per_pixel()
                .partial_cmp(&b.bytes_per_pixel())
                .unwrap_or(std::cmp::Ordering::Equal),
            Column::Weight => a.bytes.cmp(&b.bytes),
        }
        .then_with(|| a.name().cmp(&b.name()));

        if sort.descending {
            ordering.reverse()
        } else {
            ordering
        }
    }
}

struct Comparison {
    index: usize,
    /// `None` while the two sides are still decoding.
    pair: Option<Arc<Pair>>,
    /// Where the divider sits, 0 to 1 across the viewport.
    split: f32,
    /// How far the image is dragged from centre, in pixels.
    pan: (f32, f32),
    /// Display scale. `None` means fit the window; `Some(1.0)` is one image pixel per
    /// screen pixel. Kept separate so resizing the window keeps "fit" fitting.
    zoom: Option<f32>,
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
            // Everything currently visible, so a filter narrows the job as well as
            // the list. Converting hidden files would be a nasty surprise.
            self.visible.clone()
        } else {
            let mut rows: Vec<usize> = self.selected.iter().copied().collect();
            rows.sort_unstable();
            rows
        }
    }

    /// Bytes before and after, counting only the files actually converted. Comparing
    /// against the whole folder mid-run would report a fake saving.
    fn converted_totals(&self) -> (u64, u64) {
        self.results
            .iter()
            .fold((0, 0), |(before, after), (index, bytes)| {
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
        self.failures.clear();
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
                                None => {
                                    let name = audit
                                        .entries
                                        .get(index)
                                        .map(|entry| entry.name())
                                        .unwrap_or_default();
                                    audit.failures.push(name);
                                }
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

    /// Rebuild the filtered, sorted view. Nothing keyed by entry index is touched:
    /// a file keeps its thumbnail, its tick and its result through any re-ordering.
    fn refresh_visible(&mut self) {
        let needle = self.filter.to_lowercase();
        let mut visible: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| needle.is_empty() || entry.name().to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect();

        let entries = &self.entries;
        let sort = self.sort;
        visible.sort_by(|a, b| compare_entries(&entries[*a], &entries[*b], sort));

        self.cursor = self.cursor.min(visible.len().saturating_sub(1));
        self.visible = visible;
    }

    fn set_sort(&mut self, column: Column, cx: &mut Context<Self>) {
        self.sort = if self.sort.column == column {
            Sort {
                column,
                descending: !self.sort.descending,
            }
        } else {
            // Numbers open largest-first; names open A to Z.
            Sort {
                column,
                descending: !matches!(column, Column::Name | Column::Format),
            }
        };
        self.refresh_visible();
        cx.notify();
    }

    fn set_filter(&mut self, filter: String, cx: &mut Context<Self>) {
        self.filter = filter;
        self.refresh_visible();
        self.schedule_estimate(cx);
        cx.notify();
    }

    /// Encode a handful of files in memory to project what a full run would produce.
    /// Nothing is written; this only exists so the quality slider means something
    /// before you commit to it.
    fn schedule_estimate(&mut self, cx: &mut Context<Self>) {
        self.estimate_generation += 1;
        self.estimate = None;
        let generation = self.estimate_generation;

        let targets = self.targets();
        if targets.is_empty() {
            return;
        }

        // Spread the sample across the list rather than taking the heaviest few: the
        // top of a folder is often one outlier.
        let stride = targets.len().div_ceil(SAMPLE_SIZE).max(1);
        let sample: Vec<(PathBuf, u64)> = targets
            .iter()
            .step_by(stride)
            .take(SAMPLE_SIZE)
            .filter_map(|index| {
                let entry = self.entries.get(*index)?;
                Some((entry.path.clone(), entry.bytes))
            })
            .collect();
        let total: u64 = targets
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .map(|entry| entry.bytes)
            .sum();

        let (format, quality, max_edge) = (self.format, self.quality, self.max_edge);

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ESTIMATE_DELAY).await;
            if this
                .read_with(cx, |audit, _| audit.estimate_generation != generation)
                .unwrap_or(true)
            {
                return;
            }

            let sampled = cx
                .background_executor()
                .spawn(async move {
                    let mut source = 0u64;
                    let mut encoded = 0u64;
                    let mut counted = 0usize;
                    for (path, bytes) in sample {
                        let Some(image) = image::open(&path).ok().map(|i| max_edge.apply(i)) else {
                            continue;
                        };
                        let Some(output) = convert::encode(&image, format, quality) else {
                            continue;
                        };
                        source += bytes;
                        encoded += output.len() as u64;
                        counted += 1;
                    }
                    (source, encoded, counted)
                })
                .await;

            let (source, encoded, counted) = sampled;
            if counted == 0 || source == 0 {
                return;
            }

            let projected = (total as f64 * (encoded as f64 / source as f64)) as u64;
            let _ = this.update(cx, |audit, cx| {
                // A newer change started while this was encoding.
                if audit.estimate_generation == generation {
                    audit.estimate = Some((projected, counted));
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Move the keyboard cursor, clamped to the list.
    fn move_cursor(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.visible.is_empty() {
            return;
        }
        let last = self.visible.len() - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last as isize) as usize;
        cx.notify();
    }

    fn toggle_cursor_selection(&mut self, cx: &mut Context<Self>) {
        if let Some(entry) = self.entry_at(self.cursor)
            && !self.selected.remove(&entry)
        {
            self.selected.insert(entry);
        }
        cx.notify();
    }

    /// The entry a visible row points at.
    fn entry_at(&self, row: usize) -> Option<usize> {
        self.visible.get(row).copied()
    }

    /// Where an entry currently sits in the view, if the filter has not hidden it.
    fn row_of(&self, entry: usize) -> Option<usize> {
        self.visible.iter().position(|index| *index == entry)
    }

    /// Step to the next or previous image while the comparison is open.
    fn step_compare(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(entry) = self.compare.as_ref().map(|comparison| comparison.index) else {
            return;
        };
        // Step through what is on screen, not through the underlying scan order.
        let Some(row) = self.row_of(entry) else {
            return;
        };
        let next = row as isize + delta;
        if next >= 0
            && let Some(entry) = self.entry_at(next as usize)
        {
            self.cursor = next as usize;
            self.open_compare(entry, cx);
        }
    }

    /// One gallery tile: the picture, with its name and weight under it.
    fn tile(&self, row: usize, index: usize, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let Some(entry) = self.entries.get(index) else {
            return div().id(("tile", row));
        };
        let thumb = self.thumbs.get(&index).cloned();
        let ticked = self.selected.contains(&index);

        div()
            .id(("tile", row))
            .w(px(TILE))
            .flex()
            .flex_col()
            .gap_1()
            .p_1()
            .rounded_md()
            .cursor_pointer()
            .when(row == self.cursor, |tile| {
                tile.border_1().border_color(rgb(ACCENT))
            })
            .when(ticked, |tile| tile.bg(rgba(0xffffff1f)))
            .hover(|style| style.bg(rgba(0xffffff14)))
            .on_click(cx.listener(move |audit, _, _, cx| {
                if let Some(position) = audit.row_of(index) {
                    audit.cursor = position;
                }
                audit.open_compare(index, cx)
            }))
            .child(
                div()
                    .w_full()
                    .h(px(TILE - 44.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .bg(rgba(0xffffff0d))
                    .when_some(thumb, |slot, image| {
                        slot.child(img(image).max_w(px(TILE - 12.)).max_h(px(TILE - 48.)))
                    }),
            )
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(px(10.))
                    .text_color(white())
                    .child(entry.name()),
            )
            .child(div().text_size(px(10.)).text_color(rgba(MUTED)).child(
                match self.results.get(&index) {
                    Some(bytes) => {
                        format!("{} → {}", format_bytes(entry.bytes), format_bytes(*bytes))
                    }
                    None => format_bytes(entry.bytes),
                },
            ))
    }

    fn column_header(
        &self,
        column: Column,
        width: Option<f32>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.sort.column == column;
        let arrow = if !active {
            ""
        } else if self.sort.descending {
            " ↓"
        } else {
            " ↑"
        };

        let header = div()
            .id(gpui::SharedString::from(format!("col-{}", column.title())))
            .cursor_pointer()
            .text_size(px(11.))
            .text_color(if active { rgb(ACCENT) } else { rgba(MUTED) })
            .hover(|style| style.text_color(white()))
            .child(format!("{}{arrow}", column.title()))
            .on_click(cx.listener(move |audit, _, _, cx| audit.set_sort(column, cx)));

        match width {
            Some(width) => header.w(px(width)),
            None => header.flex_1().min_w_0(),
        }
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
                    unreadable: 0,
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
        self.unreadable = scanned.unreadable;
        self.thumbs.clear();
        self.requested.clear();
        self.selected.clear();
        self.results.clear();
        self.failures.clear();
        self.compare = None;
        self.cached = None;
        self.filter.clear();
        self.cursor = 0;
        self.refresh_visible();
        cx.notify();

        if single {
            self.open_compare(0, cx);
        }
    }

    /// Hand the output folder to the desktop's file manager.
    // ponytail: three names for one idea, and no crate needed for it.
    fn reveal_output(&self) {
        let path = self.root.join(scan::OUTPUT_DIR);
        if !path.exists() {
            return;
        }
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "explorer"
        } else {
            "xdg-open"
        };
        let _ = std::process::Command::new(opener).arg(path).spawn();
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
        Button::new(id)
            .ghost()
            .xsmall()
            .label(text)
            .on_click(cx.listener(move |audit, _, _, cx| on_click(audit, cx)))
    }

    /// A button that stays lit while its value is the active one.
    fn choice_button(
        &self,
        id: impl Into<gpui::ElementId>,
        label: String,
        selected: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> impl IntoElement {
        Button::new(id)
            .ghost()
            .xsmall()
            .selected(selected)
            .label(label)
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
            // Open fitted: you cannot judge a crop of an image you have not seen.
            zoom: None,
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

    /// Full-window comparison. It opens fitted, because you cannot judge a crop of an
    /// image you have not seen yet, and zooms to 1:1 and beyond — fitting a 5568px
    /// photo into a 900px window hides exactly the artefacts this view exists to show.
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
            .on_scroll_wheel(
                cx.listener(move |audit, event: &gpui::ScrollWheelEvent, _, cx| {
                    let Some(comparison) = audit.compare.as_mut() else {
                        return;
                    };
                    let Some(pair) = comparison.pair.as_ref() else {
                        return;
                    };

                    let ticks = match event.delta {
                        gpui::ScrollDelta::Lines(delta) => delta.y,
                        gpui::ScrollDelta::Pixels(delta) => f32::from(delta.y) / 40.,
                    };
                    if ticks == 0. {
                        return;
                    }

                    let fit = (view_w / pair.width as f32)
                        .min(view_h / pair.height as f32)
                        .min(1.);
                    let before = comparison.zoom.unwrap_or(fit);
                    let after = (before * 1.2f32.powf(ticks)).clamp(0.02, 16.);

                    // Keep whatever is under the pointer under the pointer. Without this
                    // zooming walks the image off screen.
                    let pointer = (
                        f32::from(event.position.x) - view_w / 2.,
                        f32::from(event.position.y) - view_h / 2.,
                    );
                    let ratio = after / before;
                    comparison.pan = (
                        pointer.0 - (pointer.0 - comparison.pan.0) * ratio,
                        pointer.1 - (pointer.1 - comparison.pan.1) * ratio,
                    );
                    comparison.zoom = Some(after);
                    cx.notify();
                }),
            )
            .on_mouse_move(
                cx.listener(move |audit, event: &gpui::MouseMoveEvent, _, cx| {
                    let Some(comparison) = audit.compare.as_mut() else {
                        return;
                    };
                    let at = (f32::from(event.position.x), f32::from(event.position.y));

                    match comparison.drag {
                        // Held: pan both sides together, so they stay in register.
                        Some((from, start_pan)) => {
                            comparison.pan =
                                (start_pan.0 + at.0 - from.0, start_pan.1 + at.1 - from.1);
                        }
                        // Free: the divider tracks the pointer.
                        None => comparison.split = (at.0 / view_w).clamp(0., 1.),
                    }
                    cx.notify();
                }),
            );

        if let Some(pair) = comparison.pair.as_ref() {
            let natural = (pair.width as f32, pair.height as f32);
            // Fit never scales up: a 400px thumbnail blown across a 4K window is just
            // a blurry 400px thumbnail.
            let fit = (view_w / natural.0).min(view_h / natural.1).min(1.);
            let scale = comparison.zoom.unwrap_or(fit);
            let (image_w, image_h) = (natural.0 * scale, natural.1 * scale);
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
                    format!(
                        "{name} · {}×{} · {:.0}%  ·  scroll to zoom · drag to pan · F fit · 1 actual · ← → next",
                        pair.width,
                        pair.height,
                        scale * 100.
                    ),
                    rgba(MUTED),
                ));
        } else {
            stage = stage.child(label(
                px(12.),
                px(12.),
                "decoding…".to_string(),
                rgba(MUTED),
            ));
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
        self.choice_button(
            gpui::SharedString::from(format!("size-{}", max_edge.label())),
            max_edge.label(),
            self.max_edge == max_edge,
            cx,
            move |audit, cx| {
                audit.max_edge = max_edge;
                audit.results.clear();
                audit.schedule_estimate(cx);
                cx.notify();
            },
        )
    }

    fn format_button(&self, format: Format, cx: &mut Context<Self>) -> impl IntoElement {
        self.choice_button(
            gpui::SharedString::from(format.label()),
            format.label().to_string(),
            self.format == format,
            cx,
            move |audit, cx| {
                audit.format = format;
                // Results describe the old format; keeping them would mislabel them.
                audit.results.clear();
                audit.schedule_estimate(cx);
                cx.notify();
            },
        )
    }

    fn quality_button(&self, quality: Quality, cx: &mut Context<Self>) -> impl IntoElement {
        self.choice_button(
            gpui::SharedString::from(quality.label()),
            quality.label(),
            self.quality == quality,
            cx,
            move |audit, cx| {
                audit.quality = quality;
                audit.results.clear();
                audit.schedule_estimate(cx);
                cx.notify();
            },
        )
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

    fn row(&self, row: usize, index: usize, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let Some(entry) = self.entries.get(index) else {
            return div().id(row);
        };
        let thumb = self.thumbs.get(&index).cloned();
        let on_cursor = row == self.cursor;

        div()
            .id(row)
            .flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_3()
            .h(px(ROW_HEIGHT - 4.))
            .rounded_md()
            .bg(rgb(ROW))
            .when(on_cursor, |row| {
                row.border_1()
                    .border_color(rgb(ACCENT))
                    .bg(rgba(0xffffff0f))
            })
            .cursor_pointer()
            .hover(|style| style.bg(rgba(0xffffff14)))
            .on_click(cx.listener(move |audit, _, _, cx| {
                if let Some(position) = audit.row_of(index) {
                    audit.cursor = position;
                }
                audit.open_compare(index, cx)
            }))
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
                    .border_color(if ticked {
                        rgb(ACCENT)
                    } else {
                        rgba(0xffffff33)
                    })
                    .bg(if ticked {
                        rgb(ACCENT)
                    } else {
                        rgba(0x00000000)
                    })
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
fn label(
    left: gpui::Pixels,
    top: gpui::Pixels,
    text: String,
    colour: impl Into<gpui::Hsla>,
) -> impl IntoElement {
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
        let count = self.visible.len();

        let title = match self.root.file_name() {
            Some(name) => format!("{} — ImageGuide", name.to_string_lossy()),
            None => "ImageGuide".to_string(),
        };
        if title != self.titled {
            window.set_window_title(&title);
            self.titled = title;
        }

        // Cheap enough to check every frame, and it means a crash still leaves the
        // last good size and folder on disk.
        let viewport = window.viewport_size();
        let current = settings::Settings {
            width: Some(f32::from(viewport.width)),
            height: Some(f32::from(viewport.height)),
            folder: self.root.is_dir().then(|| self.root.clone()),
        };
        if current != self.settings {
            settings::save(&current);
            self.settings = current;
        }

        if self.entries.is_empty() && !self.root.is_dir() {
            return div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_4()
                .bg(cx.theme().background)
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
                        .child(self.toolbar_button(
                            "empty-folder",
                            "Open folder…",
                            cx,
                            |audit, cx| audit.pick(true, cx),
                        ))
                        .child(self.toolbar_button(
                            "empty-file",
                            "Open image…",
                            cx,
                            |audit, cx| audit.pick(false, cx),
                        )),
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
            return div()
                .size_full()
                .relative()
                .track_focus(&self.focus)
                .on_key_down(cx.listener(|audit, event: &gpui::KeyDownEvent, _, cx| {
                    match event.keystroke.key.as_str() {
                        "escape" => {
                            audit.compare = None;
                            cx.notify();
                        }
                        "right" | "down" => audit.step_compare(1, cx),
                        "left" | "up" => audit.step_compare(-1, cx),
                        "f" => {
                            if let Some(comparison) = audit.compare.as_mut() {
                                comparison.zoom = None;
                                comparison.pan = (0., 0.);
                                cx.notify();
                            }
                        }
                        "1" => {
                            if let Some(comparison) = audit.compare.as_mut() {
                                comparison.zoom = Some(1.);
                                comparison.pan = (0., 0.);
                                cx.notify();
                            }
                        }
                        _ => {}
                    }
                }))
                .child(view)
                .into_any_element();
        }

        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_4()
            .bg(cx.theme().background)
            .font_family("sans-serif")
            .track_focus(&self.focus)
            .when(self.drag_over, |shell| {
                shell.border_2().border_color(rgb(ACCENT))
            })
            .on_drag_move(cx.listener(
                |audit, _: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                    if !audit.drag_over {
                        audit.drag_over = true;
                        cx.notify();
                    }
                },
            ))
            .on_key_down(cx.listener(|audit, event: &gpui::KeyDownEvent, _, cx| {
                // The filter box swallows its own keys, so these only fire when the
                // list itself has focus.
                match event.keystroke.key.as_str() {
                    "down" => audit.move_cursor(1, cx),
                    "up" => audit.move_cursor(-1, cx),
                    "pagedown" => audit.move_cursor(10, cx),
                    "pageup" => audit.move_cursor(-10, cx),
                    "home" => audit.move_cursor(isize::MIN / 2, cx),
                    "end" => audit.move_cursor(isize::MAX / 2, cx),
                    "space" => audit.toggle_cursor_selection(cx),
                    "enter" => {
                        if let Some(entry) = audit.entry_at(audit.cursor) {
                            audit.open_compare(entry, cx);
                        }
                    }
                    _ => {}
                }
            }))
            .on_drop(cx.listener(|audit, paths: &gpui::ExternalPaths, _, cx| {
                audit.drag_over = false;
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
                    .child(div().text_size(px(12.)).text_color(rgba(MUTED)).child(
                        match self.skipped_raw {
                            0 => format!("{count} images · {}", format_bytes(self.total_bytes())),
                            skipped => format!(
                                "{count} images · {} · {skipped} camera raw skipped",
                                format_bytes(self.total_bytes())
                            ),
                        },
                    ))
                    .child(div().flex_1())
                    .child(
                        div()
                            .w(px(170.))
                            .child(Input::new(&self.filter_input).xsmall()),
                    )
                    .child(self.choice_button(
                        "view-grid",
                        if self.grid { "List" } else { "Grid" }.to_string(),
                        self.grid,
                        cx,
                        |audit, cx| {
                            audit.grid = !audit.grid;
                            cx.notify();
                        },
                    ))
                    .child(
                        self.toolbar_button("open-folder", "Folder…", cx, |audit, cx| {
                            audit.pick(true, cx)
                        }),
                    )
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
                    .child(
                        div()
                            .w(px(150.))
                            .child(Slider::new(&self.quality_slider).horizontal()),
                    )
                    .child(
                        div()
                            .w(px(64.))
                            .text_size(px(12.))
                            .text_color(rgba(MUTED))
                            .child(self.quality.label()),
                    )
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
                                format!(
                                    "Converting {}/{}",
                                    self.results.len() + self.failures.len(),
                                    self.targets().len()
                                )
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
                    .when(!self.converting && self.results.is_empty(), |row| {
                        let source: u64 = self
                            .targets()
                            .iter()
                            .filter_map(|index| self.entries.get(*index))
                            .map(|entry| entry.bytes)
                            .sum();
                        row.child(div().text_size(px(12.)).text_color(rgba(MUTED)).child(
                            match self.estimate {
                                None => "estimating…".to_string(),
                                Some((projected, sampled)) => format!(
                                    "≈ {} · −{:.0}% (from {sampled})",
                                    format_bytes(projected),
                                    (source.saturating_sub(projected)) as f32
                                        / source.max(1) as f32
                                        * 100.
                                ),
                            },
                        ))
                    })
                    .when(!self.selected.is_empty(), |row| {
                        row.child(
                            self.toolbar_button("select-none", "Clear", cx, |audit, cx| {
                                audit.selected.clear();
                                cx.notify();
                            }),
                        )
                    })
                    .when(!self.results.is_empty() && !self.converting, |row| {
                        row.child(
                            self.toolbar_button("reveal", "Show output", cx, |audit, _| {
                                audit.reveal_output()
                            }),
                        )
                    }),
            )
            .when(!self.failures.is_empty() || self.unreadable > 0, |shell| {
                let mut parts = Vec::new();
                if self.unreadable > 0 {
                    parts.push(format!("{} would not decode", self.unreadable));
                }
                if !self.failures.is_empty() {
                    // Name a few. A bare count is not a report.
                    let named: Vec<&str> = self
                        .failures
                        .iter()
                        .take(3)
                        .map(|name| name.as_str())
                        .collect();
                    let rest = self.failures.len().saturating_sub(named.len());
                    parts.push(match rest {
                        0 => format!("failed: {}", named.join(", ")),
                        rest => format!("failed: {} and {rest} more", named.join(", ")),
                    });
                }
                shell.child(
                    div()
                        .text_size(px(12.))
                        .text_color(rgb(0xe0a34a))
                        .child(parts.join(" · ")),
                )
            })
            .when(!self.results.is_empty(), |shell| {
                let (before, after) = self.converted_totals();
                let saved = before.saturating_sub(after);
                let percent = if before == 0 {
                    0.
                } else {
                    saved as f32 / before as f32 * 100.
                };
                shell.child(div().text_size(px(12.)).text_color(rgb(GOOD)).child(
                    match self.failures.len() {
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
                    },
                ))
            })
            .when(self.converting, |shell| {
                let done = (self.results.len() + self.failures.len()) as f32;
                let total = self.targets().len().max(1) as f32;
                shell.child(Progress::new("convert-progress").value(done / total * 100.))
            })
            .when(!self.grid, |shell| {
                shell.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .px_3()
                        .pb_1()
                        .child(
                            // Tick-all sits where the per-row ticks are.
                            Checkbox::new("select-all")
                                .checked(!self.selected.is_empty())
                                .on_click(cx.listener(|audit, _: &bool, _, cx| {
                                    if audit.selected.is_empty() {
                                        audit.selected = (0..audit.entries.len()).collect();
                                    } else {
                                        audit.selected.clear();
                                    }
                                    cx.notify();
                                })),
                        )
                        .child(div().w(px(52.)))
                        .child(self.column_header(Column::Name, None, cx))
                        .child(self.column_header(Column::Format, Some(52.), cx))
                        .child(self.column_header(Column::Pixels, Some(96.), cx))
                        .child(self.column_header(Column::Density, Some(76.), cx))
                        .child(self.column_header(Column::Weight, Some(76.), cx))
                        .child(div().w(px(132.))),
                )
            })
            .child(if self.grid {
                // Same virtualisation, one row of tiles per list row.
                let rows = count.div_ceil(TILE_COLUMNS);
                uniform_list(
                    "gallery",
                    rows,
                    cx.processor(|audit, range: std::ops::Range<usize>, _window, cx| {
                        range
                            .map(|band| {
                                // A plain loop: the closure form borrows `audit`
                                // mutably for `request_thumb` and immutably for
                                // `tile`, which nested closures cannot express.
                                let first = band * TILE_COLUMNS;
                                let last = (first + TILE_COLUMNS).min(audit.visible.len());
                                let mut tiles = Vec::new();
                                for row in first..last {
                                    let Some(entry) = audit.entry_at(row) else {
                                        continue;
                                    };
                                    audit.request_thumb(entry, cx);
                                    tiles.push(audit.tile(row, entry, cx));
                                }
                                div().flex().gap_2().children(tiles)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .into_any_element()
            } else {
                uniform_list(
                    "images",
                    count,
                    cx.processor(|audit, range: std::ops::Range<usize>, _window, cx| {
                        // Decode only what the viewport asked for.
                        range
                            .filter_map(|row| {
                                let entry = audit.entry_at(row)?;
                                audit.request_thumb(entry, cx);
                                Some(audit.row(row, entry, cx))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .flex_1()
                .into_any_element()
            })
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
    grid: bool,
}

fn parse_args() -> Args {
    let mut root = None;
    let mut convert = false;
    let mut format = Format::WebP;
    let mut quality = Quality::lossy(80.);
    let mut max_edge = MaxEdge::FULL;
    let mut grid = false;
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
            "--grid" => grid = true,
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
        grid,
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

    let remembered = settings::load();
    let target = args.root.clone().or_else(|| {
        remembered
            .folder
            .clone()
            .filter(|folder| folder.is_dir() && !args.convert)
    });

    let Some(target) = target else {
        if args.convert {
            eprintln!("imageguide: --convert needs a folder");
            std::process::exit(2);
        }
        // No path given: open the window on its empty state and let the user pick.
        return run_window(Launch {
            root: PathBuf::new(),
            entries: Vec::new(),
            skipped_raw: 0,
            unreadable: 0,
            open_single: false,
            format: args.format,
            quality: args.quality,
            max_edge: args.max_edge,
            grid: args.grid,
        });
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
                unreadable: 0,
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

    run_window(Launch {
        root,
        entries,
        skipped_raw: scanned.skipped_raw,
        unreadable: scanned.unreadable,
        open_single,
        format: args.format,
        quality: args.quality,
        max_edge: args.max_edge,
        grid: args.grid,
    });
}

/// Everything the window needs to open. A struct rather than nine positional
/// arguments, three of which are `usize` and two of which are `bool`.
struct Launch {
    root: PathBuf,
    entries: Vec<Entry>,
    skipped_raw: usize,
    unreadable: usize,
    open_single: bool,
    format: Format,
    quality: Quality,
    max_edge: MaxEdge,
    grid: bool,
}

fn run_window(launch: Launch) {
    let Launch {
        root,
        entries,
        skipped_raw,
        unreadable,
        open_single,
        format,
        quality,
        max_edge,
        grid,
    } = launch;

    application().run(move |cx: &mut App| {
        // Must run before any gpui-component type is constructed.
        gpui_component::init(cx);
        // Dark by default. Judging compression against a bright chrome is a bad idea,
        // and the comparison view is full-bleed imagery either way.
        gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);

        let remembered = settings::load();
        let bounds = Bounds::centered(
            None,
            size(
                px(remembered.width.unwrap_or(900.)),
                px(remembered.height.unwrap_or(640.)),
            ),
            cx,
        );
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                app_id: Some("imageguide".to_string()),
                ..Default::default()
            },
            |window, cx| {
                let audit = cx.new(|cx| {
                    let focus = cx.focus_handle();
                    focus.focus(window, cx);

                    let filter_input =
                        cx.new(|cx| InputState::new(window, cx).placeholder("Filter by name"));
                    cx.subscribe(
                        &filter_input,
                        |audit: &mut Audit, input, event: &InputEvent, cx| {
                            if matches!(event, InputEvent::Change) {
                                let value = input.read(cx).value().to_string();
                                audit.set_filter(value, cx);
                            }
                        },
                    )
                    .detach();

                    let quality_slider = cx.new(|_| {
                        SliderState::new()
                            .min(1.)
                            .max(100.)
                            .step(1.)
                            .default_value(quality.0.unwrap_or(80.))
                    });
                    // Dragging the slider is the only thing that changes quality now,
                    // so results from the old value stop being true the moment it moves.
                    cx.subscribe(
                        &quality_slider,
                        |audit: &mut Audit, _, event: &SliderEvent, cx| {
                            let SliderEvent::Change(value) = event else {
                                return;
                            };
                            audit.quality = Quality::lossy(value.start());
                            audit.results.clear();
                            audit.schedule_estimate(cx);
                            cx.notify();
                        },
                    )
                    .detach();
                    let mut audit = Audit {
                        root,
                        entries,
                        skipped_raw,
                        thumbs: HashMap::new(),
                        requested: HashSet::new(),
                        format,
                        quality,
                        max_edge,
                        quality_slider,
                        selected: HashSet::new(),
                        sort: Sort {
                            column: Column::Weight,
                            descending: true,
                        },
                        visible: Vec::new(),
                        filter: String::new(),
                        filter_input,
                        cursor: 0,
                        grid,
                        estimate: None,
                        estimate_generation: 0,
                        focus,
                        titled: String::new(),
                        settings: settings::Settings::default(),
                        cached: None,
                        results: HashMap::new(),
                        converting: false,
                        failures: Vec::new(),
                        unreadable,
                        drag_over: false,
                        compare: None,
                    };
                    audit.refresh_visible();
                    audit.schedule_estimate(cx);
                    if open_single {
                        audit.open_compare(0, cx);
                    }
                    audit
                });

                // Dialogs, notifications and tooltips are drawn by the Root, so the
                // window's first level has to be one.
                cx.new(|cx| Root::new(audit, window, cx).bg(cx.theme().background))
            },
        )
        .unwrap();
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::ImageFormat;
    use std::path::PathBuf;

    fn entry(name: &str, width: u32, height: u32, bytes: u64, format: ImageFormat) -> Entry {
        Entry {
            path: PathBuf::from(name),
            format,
            width,
            height,
            bytes,
        }
    }

    fn names(entries: &[Entry]) -> Vec<String> {
        entries.iter().map(|entry| entry.name()).collect()
    }

    /// The app sorts indices into an unmoved `entries`; these tests sort the data
    /// directly, which is the same comparator either way.
    fn sort_entries(entries: &mut [Entry], sort: Sort) {
        entries.sort_by(|a, b| compare_entries(a, b, sort));
    }

    #[test]
    fn weight_sorts_heaviest_first_when_descending() {
        let mut entries = vec![
            entry("small.png", 10, 10, 100, ImageFormat::Png),
            entry("big.png", 10, 10, 900, ImageFormat::Png),
            entry("mid.png", 10, 10, 500, ImageFormat::Png),
        ];
        sort_entries(
            &mut entries,
            Sort {
                column: Column::Weight,
                descending: true,
            },
        );
        assert_eq!(names(&entries), ["big.png", "mid.png", "small.png"]);
    }

    #[test]
    fn name_sorting_ignores_case() {
        let mut entries = vec![
            entry("Zebra.png", 1, 1, 1, ImageFormat::Png),
            entry("apple.png", 1, 1, 1, ImageFormat::Png),
        ];
        sort_entries(
            &mut entries,
            Sort {
                column: Column::Name,
                descending: false,
            },
        );
        assert_eq!(names(&entries), ["apple.png", "Zebra.png"]);
    }

    /// Equal values must not reshuffle between sorts. A list that reorders itself for
    /// no visible reason is worse than one sorted badly.
    #[test]
    fn ties_fall_back_to_the_filename() {
        let mut entries = vec![
            entry("c.png", 4, 4, 200, ImageFormat::Png),
            entry("a.png", 4, 4, 200, ImageFormat::Png),
            entry("b.png", 4, 4, 200, ImageFormat::Png),
        ];
        let sort = Sort {
            column: Column::Density,
            descending: false,
        };
        sort_entries(&mut entries, sort);
        assert_eq!(names(&entries), ["a.png", "b.png", "c.png"]);
    }

    #[test]
    fn pixels_sorts_on_area_not_width() {
        let mut entries = vec![
            entry("wide.png", 1000, 10, 1, ImageFormat::Png),
            entry("square.png", 200, 200, 1, ImageFormat::Png),
        ];
        sort_entries(
            &mut entries,
            Sort {
                column: Column::Pixels,
                descending: true,
            },
        );
        assert_eq!(names(&entries), ["square.png", "wide.png"]);
    }
}
