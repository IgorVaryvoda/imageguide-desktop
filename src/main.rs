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
    WindowOptions, div, img, prelude::*, px, relative, rgb, rgba, size, uniform_list, white,
};
use gpui_component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, IconName, Root, Selectable, Sizable};
use gpui_platform::application;
use image::ImageFormat;
use scan::{Entry, format_bytes, format_name};

// Colours come from `cx.theme()` rather than a private palette. The window is
// built out of this library's buttons, inputs and tags, and a hand-picked set of
// greys sitting behind them agreed with nothing — the chrome and the controls were
// two designs in one window.

/// Rows are for scanning a folder of thousands, so they are sized to fit as many
/// as possible while still showing a thumbnail you can recognise.
const ROW_HEIGHT: f32 = 40.;
const THUMB_SLOT: f32 = 34.;

// ── Column widths ───────────────────────────────────────────────────────────
// One constant per column, shared by the header and every row. They used to be
// written twice and had already drifted; a header that sits over the wrong column
// is worse than no header.
const W_TICK: f32 = 16.;
const W_FORMAT: f32 = 62.;
const W_PIXELS: f32 = 84.;
const W_DENSITY: f32 = 62.;
const W_WEIGHT: f32 = 92.;
const W_RESULT: f32 = 112.;
/// The weight bar gets a column of its own. Sharing a cell with the figure meant a
/// left-grown bar under a right-aligned number — an underline for the heaviest file
/// and a stub stranded half a column from its own number for the lightest.
const W_BAR: f32 = 140.;

/// Bytes per output pixel, banded. A photographic JPEG lands near 0.2; a
/// screenshot saved as PNG can be ten times that. The number was already in the
/// list and every row printed it in the same grey, which made the app's one
/// diagnostic something you had to read rather than see.
const DENSITY_GOOD: f32 = 0.5;
const DENSITY_HEAVY: f32 = 1.5;
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

/// Which band a file's byte density falls in. Green is carrying its weight, amber
/// is suspicious, red is a screenshot saved as a PNG.
fn density_colour(density: f32, cx: &App) -> gpui::Hsla {
    if density <= DENSITY_GOOD {
        cx.theme().green
    } else if density <= DENSITY_HEAVY {
        cx.theme().yellow
    } else {
        cx.theme().red
    }
}

/// Modern formats are the destination, JPEG is a reasonable place to be, and the
/// rest are the reason this app exists. Colouring the column turns it from a label
/// into the finding the audit is actually making.
fn format_colour(format: ImageFormat, cx: &App) -> gpui::Hsla {
    match format {
        ImageFormat::WebP | ImageFormat::Avif => cx.theme().green,
        ImageFormat::Jpeg => cx.theme().blue,
        _ => cx.theme().yellow,
    }
}

/// A label for the comparison view, which floats over the picture rather than over
/// a theme surface, so it carries its own dark backing.
fn compare_chip(text: impl Into<gpui::SharedString>, colour: gpui::Hsla, _cx: &App) -> gpui::Div {
    div()
        .h(px(18.))
        .px_2()
        .flex()
        .items_center()
        .flex_shrink_0()
        .rounded_md()
        .bg(rgba(0x000000b8))
        .text_size(px(10.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(colour)
        .child(text.into())
}

/// A right-aligned cell. Figures you are meant to compare have to share an edge —
/// left-aligned in a fixed-width column they drift apart by however long they are,
/// which is exactly the comparison the column exists to make.
fn numeric(width: f32, colour: gpui::Hsla, text: String) -> gpui::Div {
    div()
        .w(px(width))
        .flex()
        .justify_end()
        .flex_shrink_0()
        .whitespace_nowrap()
        .text_color(colour)
        .child(text)
}

/// A proportional bar. The audit is a ranking and a column of numbers does not
/// rank — 632 KB and 104 KB were set in the same size and colour, so the shape of
/// the folder was invisible in a list sorted by exactly that.
fn meter(fraction: f32, colour: gpui::Hsla, height: f32, cx: &App) -> gpui::Div {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0., 1.)
    } else {
        0.
    };
    div()
        .w_full()
        .h(px(height))
        .rounded_md()
        .bg(cx.theme().progress_bar.opacity(0.2))
        .child(
            div()
                .w(relative(fraction))
                .h(px(height))
                .rounded_md()
                .bg(colour),
        )
}

/// The faint word that says what a group of controls is for.
fn group_label(text: &'static str, cx: &App) -> gpui::Div {
    div()
        .text_size(px(10.))
        .text_color(cx.theme().muted_foreground)
        .whitespace_nowrap()
        .flex_shrink_0()
        .child(text)
}

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
    /// Where the last plain click landed, which is the fixed end of a shift-click
    /// range. Separate from `cursor` so arrowing around does not move the anchor.
    anchor: usize,
    /// The last quality the slider was set to, so turning Lossless off goes back to
    /// where you were rather than to an arbitrary default.
    slider_quality: f32,
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
    /// Bytes of the heaviest visible file, so every row's weight bar is drawn
    /// against the same scale. Cached because the alternative is a scan of the
    /// whole list once per row.
    heaviest: u64,
    /// Files whose extension disagrees with their contents. Counted once when the
    /// folder is read, because the check allocates and the filter box would
    /// otherwise redo it for every entry on every keystroke.
    mislabelled: usize,
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
        // Weight bars are drawn against the heaviest file on screen, so filtering
        // down to the small ones still spreads them across the column instead of
        // leaving every bar a stub.
        self.heaviest = visible
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .map(|entry| entry.bytes)
            .max()
            .unwrap_or(0);
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
                        let Some(image) = scan::decode(&path).map(|i| max_edge.apply(i)) else {
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

    /// What a click on a row means, by the rules every file list uses: plain click
    /// selects just that row, the platform modifier adds or removes one, shift takes
    /// the run from the last click, and a second click opens it.
    ///
    /// A plain click used to open the comparison, which made picking a few files to
    /// convert a fight with a full-screen preview.
    fn click_row(&mut self, row: usize, event: &gpui::ClickEvent, cx: &mut Context<Self>) {
        let Some(entry) = self.entry_at(row) else {
            return;
        };
        let modifiers = event.modifiers();

        if event.click_count() >= 2 {
            self.cursor = row;
            self.open_compare(entry, cx);
            return;
        }

        if modifiers.platform || modifiers.control {
            if !self.selected.remove(&entry) {
                self.selected.insert(entry);
            }
        } else if modifiers.shift {
            // From wherever the last plain click landed to here, inclusive, so a
            // run of heavy files is two clicks rather than twenty.
            let (from, to) = if self.anchor <= row {
                (self.anchor, row)
            } else {
                (row, self.anchor)
            };
            let run: Vec<usize> = (from..=to).filter_map(|row| self.entry_at(row)).collect();
            self.selected.extend(run);
        } else {
            self.selected.clear();
            self.selected.insert(entry);
            self.anchor = row;
        }

        self.cursor = row;
        self.schedule_estimate(cx);
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

        let density = entry.bytes_per_pixel();

        div()
            .id(("tile", row))
            .w(px(TILE))
            .flex()
            .flex_col()
            .gap_2()
            .p_2()
            .rounded_lg()
            .cursor_pointer()
            .bg(cx.theme().secondary)
            // Always bordered, in nothing, so arrowing onto a tile does not shunt
            // its contents a pixel down and right.
            .border_1()
            .border_color(gpui::transparent_black())
            .when(ticked, |tile| tile.bg(cx.theme().list_active))
            .when(row == self.cursor, |tile| {
                tile.border_color(cx.theme().primary)
                    .bg(cx.theme().list_active)
            })
            .hover(|style| style.bg(cx.theme().list_hover))
            .on_click(cx.listener(move |audit, event: &gpui::ClickEvent, _, cx| {
                if let Some(position) = audit.row_of(index) {
                    audit.click_row(position, event, cx);
                }
            }))
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(TILE - 68.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .overflow_hidden()
                    .bg(cx.theme().background)
                    .when_some(thumb, |slot, image| {
                        slot.child(img(image).max_w(px(TILE - 16.)).max_h(px(TILE - 68.)))
                    })
                    // The grid had no way to tick anything; the keyboard was the
                    // only route to a selection you could see in the list.
                    .child(
                        div().absolute().top(px(4.)).left(px(4.)).child(
                            Checkbox::new(("tile-tick", index))
                                .checked(ticked)
                                .on_click(cx.listener(move |audit, _: &bool, _, cx| {
                                    if !audit.selected.remove(&index) {
                                        audit.selected.insert(index);
                                    }
                                    audit.schedule_estimate(cx);
                                    cx.notify();
                                })),
                        ),
                    )
                    .child(
                        div().absolute().bottom(px(4.)).right(px(4.)).child(
                            div()
                                .px_1()
                                .rounded_sm()
                                .bg(cx.theme().background.opacity(0.8))
                                .text_size(px(9.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(format_colour(entry.format, cx))
                                .child(format_name(entry.format)),
                        ),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(11.))
                    .text_color(cx.theme().foreground)
                    .child(entry.name()),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .text_size(px(10.))
                    .child(div().text_color(cx.theme().muted_foreground).child(
                        match self.results.get(&index) {
                            Some(bytes) => {
                                format!("{} → {}", format_bytes(entry.bytes), format_bytes(*bytes))
                            }
                            None => format_bytes(entry.bytes),
                        },
                    ))
                    .child(
                        div()
                            .text_color(density_colour(density, cx))
                            .child(format!("{density:.2}")),
                    ),
            )
    }

    fn column_header(
        &self,
        column: Column,
        width: Option<f32>,
        right: bool,
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
            .flex()
            .items_center()
            // Titles sit on the same edge as the figures underneath them.
            .when(right, |header| header.justify_end())
            .cursor_pointer()
            .whitespace_nowrap()
            .text_size(px(10.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(if active {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            })
            .child(format!("{}{arrow}", column.title()))
            .on_click(cx.listener(move |audit, _, _, cx| audit.set_sort(column, cx)));

        match width {
            Some(width) => header.w(px(width)).flex_shrink_0(),
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
        self.mislabelled = scanned
            .entries
            .iter()
            .filter(|entry| entry.extension_lies())
            .count();
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

    /// A quiet button, for the things that move you around rather than commit work.
    fn toolbar_button(
        &self,
        id: &'static str,
        text: &'static str,
        icon: IconName,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Button {
        Button::new(id)
            .small()
            .icon(icon)
            .label(text)
            .on_click(cx.listener(move |audit, _, _, cx| on_click(audit, cx)))
    }

    /// Several exclusive options as one control, under the word for what they choose.
    /// The old toolbar was thirteen identical ghost buttons in a row with a 12px gap
    /// standing in for grouping, and nothing said which was which.
    fn control_group(
        &self,
        label: &'static str,
        group: ButtonGroup,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_2()
            .flex_shrink_0()
            .child(group_label(label, cx))
            .child(group.small().outline().compact())
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

        // Fit never scales up: a 400px thumbnail blown across a 4K window is just a
        // blurry 400px thumbnail. Computed before the branch because the chrome
        // reports the zoom as well as the image using it.
        let scale = comparison.pair.as_ref().map(|pair| {
            let fit = (view_w / pair.width as f32)
                .min(view_h / pair.height as f32)
                .min(1.);
            comparison.zoom.unwrap_or(fit)
        });

        if let (Some(pair), Some(scale)) = (comparison.pair.as_ref(), scale) {
            let natural = (pair.width as f32, pair.height as f32);
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

            stage =
                stage
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
                    .child(
                        // Which side is which, pinned to the divider rather than to the
                        // window, so it stays true as the divider moves.
                        div()
                            .absolute()
                            .top(px(48.))
                            .left(px(divider - 76.))
                            .w(px(64.))
                            .flex()
                            .justify_end()
                            .child(compare_chip("original", cx.theme().foreground, cx)),
                    )
                    .child(div().absolute().top(px(48.)).left(px(divider + 12.)).child(
                        compare_chip(self.format.label().to_uppercase(), cx.theme().green, cx),
                    ));
        }

        // Chrome as two full-width bars. These were black boxes pinned at
        // hand-computed offsets, the right-hand one at `view_w - 240` — a number
        // that stopped being the right edge the moment the text or window changed.
        let (saving_text, saving_colour) = match comparison.pair.as_ref() {
            Some(pair) => {
                let saving = pair.saving_percent(source_bytes);
                if saving >= 0. {
                    (format!("−{saving:.0}%"), cx.theme().green)
                } else {
                    (format!("+{:.0}%", -saving), cx.theme().yellow)
                }
            }
            None => (String::new(), cx.theme().green),
        };

        stage
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .bg(rgba(0x000000bf))
                    .text_size(px(12.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .text_color(cx.theme().foreground)
                            .font_weight(FontWeight::MEDIUM)
                            .child(name.clone()),
                    )
                    .children(comparison.pair.as_ref().map(|pair| {
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .flex_shrink_0()
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .child(format!(
                                "{} → {} {} · {}",
                                format_bytes(source_bytes),
                                self.format.label().to_uppercase(),
                                self.quality.label(),
                                format_bytes(pair.converted_bytes)
                            ))
                            .child(compare_chip(saving_text, saving_colour, cx))
                    }))
                    .child(
                        div()
                            .id("compare-close")
                            .h(px(22.))
                            .px_2()
                            .flex()
                            .items_center()
                            .flex_shrink_0()
                            .rounded_md()
                            .cursor_pointer()
                            .bg(rgba(0xffffff1f))
                            .text_color(rgba(0xffffffcc))
                            .hover(|style| style.bg(rgba(0xffffff3d)).text_color(white()))
                            .child("Close")
                            .on_click(cx.listener(|audit, _, _, cx| {
                                audit.compare = None;
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py_2()
                    .bg(rgba(0x000000bf))
                    .text_size(px(11.))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .whitespace_nowrap()
                            .text_color(rgba(0xffffffcc))
                            .child(match (comparison.pair.as_ref(), scale) {
                                (Some(pair), Some(scale)) => {
                                    format!("{}×{} · {:.0}%", pair.width, pair.height, scale * 100.)
                                }
                                _ => "decoding…".to_string(),
                            }),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .whitespace_nowrap()
                            .text_color(rgba(0xffffff8a))
                            .child(
                                "scroll zoom · drag pan · F fit · 1 actual · ←→ next · esc close",
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The resize presets, as one segmented control. `ButtonGroup` reports the index
    /// that was clicked, so the options are listed once and read back by position.
    fn resize_group(&self, cx: &mut Context<Self>) -> ButtonGroup {
        let options = MaxEdge::PRESETS;
        ButtonGroup::new("resize")
            .children(options.iter().map(|edge| {
                Button::new(gpui::SharedString::from(edge.label()))
                    .label(edge.label())
                    .selected(self.max_edge == *edge)
            }))
            .on_click(cx.listener(move |audit, clicked: &Vec<usize>, _, cx| {
                let Some(edge) = clicked.first().and_then(|index| options.get(*index)) else {
                    return;
                };
                audit.max_edge = *edge;
                audit.results.clear();
                audit.schedule_estimate(cx);
                cx.notify();
            }))
    }

    fn format_group(&self, cx: &mut Context<Self>) -> ButtonGroup {
        let options = [Format::WebP, Format::Avif];
        ButtonGroup::new("format")
            .children(options.iter().map(|format| {
                Button::new(format.label())
                    .label(format.label().to_uppercase())
                    .selected(self.format == *format)
            }))
            .on_click(cx.listener(move |audit, clicked: &Vec<usize>, _, cx| {
                let Some(format) = clicked.first().and_then(|index| options.get(*index)) else {
                    return;
                };
                audit.format = *format;
                // Results describe the old format; keeping them would mislabel them.
                audit.results.clear();
                audit.schedule_estimate(cx);
                cx.notify();
            }))
    }

    /// Bytes of what is on screen. With a filter active the folder total would be
    /// describing files the list is not showing.
    fn visible_bytes(&self) -> u64 {
        self.visible
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .map(|entry| entry.bytes)
            .sum()
    }

    /// Which folder this is, and how to get to another one.
    fn header(&self, count: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let folder = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string());

        let mut stats = if count == self.entries.len() {
            format!("{count} images · {}", format_bytes(self.visible_bytes()))
        } else {
            format!(
                "{count} of {} images · {}",
                self.entries.len(),
                format_bytes(self.visible_bytes())
            )
        };
        if self.skipped_raw > 0 {
            stats.push_str(&format!(" · {} camera raw skipped", self.skipped_raw));
        }

        div()
            .flex()
            .items_center()
            .gap_3()
            .child(
                // The folder name identifies it; the full path only locates it, so
                // it goes underneath at a size that says so.
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(folder),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(cx.theme().muted_foreground)
                                    .whitespace_nowrap()
                                    .flex_shrink_0()
                                    .child(stats),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(cx.theme().muted_foreground.opacity(0.7))
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(self.root.display().to_string()),
                    ),
            )
            .child(
                div().w(px(190.)).flex_shrink_0().child(
                    Input::new(&self.filter_input)
                        .small()
                        .cleanable(true)
                        .prefix(IconName::Search),
                ),
            )
            .child(self.toolbar_button(
                "view-grid",
                if self.grid { "List" } else { "Grid" },
                if self.grid {
                    IconName::Menu
                } else {
                    IconName::LayoutDashboard
                },
                cx,
                |audit, cx| {
                    audit.grid = !audit.grid;
                    cx.notify();
                },
            ))
            .child(self.toolbar_button(
                "open-folder",
                "Folder",
                IconName::Folder,
                cx,
                |audit, cx| audit.pick(true, cx),
            ))
            .child(
                self.toolbar_button("open-file", "Image", IconName::File, cx, |audit, cx| {
                    audit.pick(false, cx)
                }),
            )
    }

    /// The three knobs that decide what a conversion produces, each under its own
    /// name and drawn as one control rather than a run of loose buttons.
    fn controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let lossless = self.quality == Quality::LOSSLESS;
        div()
            .flex()
            .items_center()
            .gap_4()
            .child(self.control_group("Resize", self.resize_group(cx), cx))
            .child(self.control_group("Format", self.format_group(cx), cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_shrink_0()
                    .child(group_label("Quality", cx))
                    .child(
                        div()
                            .w(px(130.))
                            .child(Slider::new(&self.quality_slider).horizontal()),
                    )
                    .child(
                        div()
                            .w(px(26.))
                            .text_size(px(11.))
                            .font_weight(FontWeight::MEDIUM)
                            .whitespace_nowrap()
                            .text_color(if lossless {
                                cx.theme().muted_foreground
                            } else {
                                cx.theme().foreground
                            })
                            .child(match self.quality.0 {
                                Some(value) => format!("{}", value.round() as u32),
                                None => "—".to_string(),
                            }),
                    )
                    .child(
                        Button::new("lossless")
                            .ghost()
                            .small()
                            .label("Lossless")
                            .selected(lossless)
                            .on_click(cx.listener(|audit, _, _, cx| {
                                // A second click on a lit toggle has to turn it off,
                                // or lossless is a one-way door.
                                audit.quality = if audit.quality == Quality::LOSSLESS {
                                    Quality::lossy(audit.slider_quality)
                                } else {
                                    Quality::LOSSLESS
                                };
                                audit.results.clear();
                                audit.schedule_estimate(cx);
                                cx.notify();
                            })),
                    ),
            )
    }

    /// The payoff, said once and out loud: what the folder costs now, what it would
    /// cost converted, and the button that does it. This used to be 11px of grey
    /// wedged between the button and the window edge — the wrong volume for the only
    /// number the app exists to produce.
    fn summary(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let targets = self.targets();
        let source: u64 = targets
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .map(|entry| entry.bytes)
            .sum();

        // Four states, one shape: a headline, the share it leaves behind, and a
        // sentence of detail.
        let (headline, tone, detail, bar) = if self.converting {
            let done = self.results.len() + self.failures.len();
            (
                format!("{done} of {}", targets.len()),
                cx.theme().foreground,
                format!(
                    "Converting to {} {}…",
                    self.format.label().to_uppercase(),
                    self.quality.label()
                ),
                Some((
                    done as f32 / targets.len().max(1) as f32,
                    cx.theme().primary,
                )),
            )
        } else if !self.results.is_empty() {
            let (before, after) = self.converted_totals();
            let saved = before.saturating_sub(after);
            (
                format!("{} saved", format_bytes(saved)),
                cx.theme().green,
                format!(
                    "{} converted · {} → {}",
                    self.results.len(),
                    format_bytes(before),
                    format_bytes(after)
                ),
                Some((after as f32 / before.max(1) as f32, cx.theme().green)),
            )
        } else if let Some((projected, sampled)) = self.estimate {
            let saved = source.saturating_sub(projected);
            (
                format!("{} to save", format_bytes(saved)),
                cx.theme().green,
                format!(
                    "{} now → ≈{} as {} {} · sampled {sampled}",
                    format_bytes(source),
                    format_bytes(projected),
                    self.format.label().to_uppercase(),
                    self.quality.label()
                ),
                Some((projected as f32 / source.max(1) as f32, cx.theme().green)),
            )
        } else {
            (
                "Sizing it up…".to_string(),
                cx.theme().muted_foreground,
                format!("{} on disk", format_bytes(source)),
                None,
            )
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .px_3()
            .py_2()
            .rounded_lg()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(17.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(tone)
                            .whitespace_nowrap()
                            .flex_shrink_0()
                            .child(headline),
                    )
                    // The share saved, which is the number people actually quote.
                    .children(bar.map(|(remaining, _)| {
                        Tag::success()
                            .small()
                            .child(format!("−{:.0}%", (1. - remaining).max(0.) * 100.))
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(detail),
                    )
                    .when(!self.selected.is_empty() && !self.converting, |row| {
                        row.child(
                            Button::new("select-none")
                                .ghost()
                                .small()
                                .label(format!("Clear {}", self.selected.len()))
                                .on_click(cx.listener(|audit, _, _, cx| {
                                    audit.selected.clear();
                                    audit.schedule_estimate(cx);
                                    cx.notify();
                                })),
                        )
                    })
                    .when(!self.results.is_empty() && !self.converting, |row| {
                        row.child(
                            Button::new("reveal")
                                .outline()
                                .small()
                                .icon(IconName::FolderOpen)
                                .label("Show output")
                                .on_click(cx.listener(|audit, _, _, _| audit.reveal_output())),
                        )
                    })
                    .child(
                        Button::new("convert")
                            .primary()
                            .small()
                            .when(self.converting || targets.is_empty(), |button| {
                                button.ghost()
                            })
                            .label(if self.converting {
                                "Converting…".to_string()
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
                    ),
            )
            .children(bar.map(|(remaining, colour)| meter(1. - remaining, colour, 3., cx)))
    }

    /// Everything the scan could not take at face value, in one line rather than
    /// three scattered ones.
    fn notices(&self) -> Option<gpui::AnyElement> {
        let mut parts = Vec::new();
        if self.mislabelled > 0 {
            parts.push(match self.mislabelled {
                1 => "1 file is not the format its extension claims".to_string(),
                many => format!("{many} files are not the format their extension claims"),
            });
        }
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
        if parts.is_empty() {
            return None;
        }

        // Left-aligned and only as wide as its text. A full-bleed box for six words
        // was a bigger shape on screen than the finding it was reporting.
        Some(
            div()
                .flex()
                .child(Tag::warning().small().outline().child(parts.join("  ·  ")))
                .into_any_element(),
        )
    }

    /// The column titles, laid out from the same constants the rows use.
    fn table_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .gap_3()
            .px_2()
            .py_2()
            .bg(cx.theme().table_head)
            // The bottom rule, plus the 2px the rows spend on their cursor edge so
            // the titles sit over their own columns rather than two pixels to the
            // left of them.
            .border_b_1()
            .border_l_2()
            .border_color(cx.theme().border)
            .text_color(cx.theme().table_head_foreground)
            .child(
                div().w(px(W_TICK)).flex().child(
                    Checkbox::new("select-all")
                        .checked(!self.selected.is_empty())
                        .on_click(cx.listener(|audit, _: &bool, _, cx| {
                            if audit.selected.is_empty() {
                                // What is on screen, not what is in the folder. The
                                // old version ticked filtered-out files too, which a
                                // conversion would then have silently included.
                                audit.selected = audit.visible.iter().copied().collect();
                            } else {
                                audit.selected.clear();
                            }
                            audit.schedule_estimate(cx);
                            cx.notify();
                        })),
                ),
            )
            .child(div().w(px(THUMB_SLOT)).flex_shrink_0())
            .child(self.column_header(Column::Name, None, false, cx))
            .child(self.column_header(Column::Format, Some(W_FORMAT), false, cx))
            .child(self.column_header(Column::Pixels, Some(W_PIXELS), true, cx))
            .child(self.column_header(Column::Density, Some(W_DENSITY), true, cx))
            .child(div().w(px(W_BAR)).flex_shrink_0())
            .child(self.column_header(Column::Weight, Some(W_WEIGHT), true, cx))
            .when(!self.results.is_empty(), |header| {
                header.child(
                    div()
                        .w(px(W_RESULT))
                        .flex()
                        .justify_end()
                        .flex_shrink_0()
                        .text_size(px(10.))
                        .font_weight(FontWeight::MEDIUM)
                        .child("Result"),
                )
            })
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
        let ticked = self.selected.contains(&index);
        let density = entry.bytes_per_pixel();
        let share = entry.bytes as f32 / self.heaviest.max(1) as f32;
        let lies = entry.extension_lies();

        div()
            .id(row)
            .flex()
            .w_full()
            .items_center()
            .gap_3()
            .px_2()
            .h(px(ROW_HEIGHT))
            // Flush rows on a hairline, rather than a rounded card each with a gap
            // around it. Eight cards read as eight things; a table reads as a folder.
            //
            // The left edge is where the cursor shows itself, always two pixels wide
            // so arrowing onto a row does not shunt every cell in it sideways. gpui
            // has one border colour for all four sides, so the cursor lights the row
            // rule as well as the edge — the same thing said twice, not a wrong
            // thing said once.
            .border_b_1()
            .border_l_2()
            .border_color(if on_cursor {
                cx.theme().list_active_border
            } else {
                cx.theme().table_row_border
            })
            .when(ticked, |row| row.bg(cx.theme().list_active))
            .when(on_cursor && !ticked, |row| row.bg(cx.theme().list_hover))
            .cursor_pointer()
            .hover(|style| style.bg(cx.theme().table_hover))
            .on_click(cx.listener(move |audit, event: &gpui::ClickEvent, _, cx| {
                if let Some(position) = audit.row_of(index) {
                    audit.click_row(position, event, cx);
                }
            }))
            .text_size(px(12.))
            .child(
                div().w(px(W_TICK)).flex().child(
                    Checkbox::new(("tick", index))
                        .checked(ticked)
                        .on_click(cx.listener(move |audit, _: &bool, _, cx| {
                            if !audit.selected.remove(&index) {
                                audit.selected.insert(index);
                            }
                            audit.schedule_estimate(cx);
                            cx.notify();
                        })),
                ),
            )
            .child(
                // A fixed slot, so rows do not jump as thumbnails arrive.
                div()
                    .w(px(THUMB_SLOT))
                    .h(px(THUMB_SLOT))
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_shrink_0()
                    .rounded_sm()
                    .bg(cx.theme().background)
                    .when_some(thumb, |slot, image| {
                        slot.child(img(image).max_w(px(THUMB_SLOT)).max_h(px(THUMB_SLOT)))
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
                    .whitespace_nowrap()
                    .text_color(cx.theme().foreground)
                    .child(entry.name()),
            )
            .child(
                div()
                    .w(px(W_FORMAT))
                    .flex()
                    .items_center()
                    .gap_1()
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if lies {
                        cx.theme().yellow
                    } else {
                        format_colour(entry.format, cx)
                    })
                    .child(format_name(entry.format))
                    // The extension disagrees with the bytes. The mark is small
                    // because the count in the notice above is what raises it.
                    .when(lies, |cell| cell.child(div().text_size(px(11.)).child("≠"))),
            )
            .child(numeric(
                W_PIXELS,
                cx.theme().muted_foreground,
                format!("{}×{}", entry.width, entry.height),
            ))
            .child(
                numeric(
                    W_DENSITY,
                    density_colour(density, cx),
                    format!("{density:.2}"),
                )
                .font_weight(FontWeight::MEDIUM),
            )
            .child(
                // Against the heaviest file on screen, so the column shows the shape
                // of the folder and not just its numbers. All the bars share a left
                // edge, which is the whole point of drawing them.
                div()
                    .w(px(W_BAR))
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .child(meter(share, cx.theme().primary, 4., cx)),
            )
            .child(
                numeric(W_WEIGHT, cx.theme().foreground, format_bytes(entry.bytes))
                    .font_weight(FontWeight::MEDIUM),
            )
            // The column only exists once there is something to put in it. Reserving
            // its width up front left a fifth of the window empty in the common case.
            .when(!self.results.is_empty(), |row| {
                row.child(div().w(px(W_RESULT)).flex_shrink_0().when_some(
                    self.results.get(&index),
                    |slot, converted| {
                        let saved = entry.bytes.saturating_sub(*converted);
                        let percent = if entry.bytes == 0 {
                            0.
                        } else {
                            saved as f32 / entry.bytes as f32 * 100.
                        };
                        // A file that grew is a real outcome, not a rounding error:
                        // re-encoding an already-optimal JPEG usually costs bytes.
                        let grew = *converted > entry.bytes;
                        slot.flex()
                            .items_center()
                            .justify_end()
                            .gap_2()
                            .whitespace_nowrap()
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format_bytes(*converted)),
                            )
                            .child(if grew {
                                Tag::warning().small().child("larger")
                            } else {
                                Tag::success().small().child(format!("−{percent:.0}%"))
                            })
                    },
                ))
            })
    }
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
                .items_center()
                .justify_center()
                .p_4()
                .bg(cx.theme().background)
                .font_family("sans-serif")
                .child(
                    // A panel rather than loose text, so the window has something in
                    // it and the drop target has an edge you can see.
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .w(px(400.))
                        .px_4()
                        .py_4()
                        .rounded_lg()
                        .bg(cx.theme().secondary)
                        .border_1()
                        .border_color(if self.drag_over {
                            cx.theme().drag_border
                        } else {
                            cx.theme().border
                        })
                        .child(
                            div()
                                .text_size(px(17.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child("Audit a folder of images"),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(cx.theme().muted_foreground)
                                .text_center()
                                .child(
                                    "Nothing is uploaded. Every file is read, resized and \
                                     re-encoded on this machine.",
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_2()
                                .pt_2()
                                .child(
                                    Button::new("empty-folder")
                                        .primary()
                                        .icon(IconName::Folder)
                                        .label("Open folder…")
                                        .on_click(
                                            cx.listener(|audit, _, _, cx| audit.pick(true, cx)),
                                        ),
                                )
                                .child(
                                    Button::new("empty-file")
                                        .outline()
                                        .icon(IconName::File)
                                        .label("Open image…")
                                        .on_click(
                                            cx.listener(|audit, _, _, cx| audit.pick(false, cx)),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .pt_2()
                                .text_size(px(11.))
                                .text_color(cx.theme().muted_foreground.opacity(0.7))
                                .child("or drop one anywhere in this window"),
                        ),
                )
                .on_drag_move(cx.listener(
                    |audit, _: &gpui::DragMoveEvent<gpui::ExternalPaths>, _, cx| {
                        if !audit.drag_over {
                            audit.drag_over = true;
                            cx.notify();
                        }
                    },
                ))
                .on_drop(cx.listener(|audit, paths: &gpui::ExternalPaths, _, cx| {
                    audit.drag_over = false;
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
            .p_3()
            .bg(cx.theme().background)
            .font_family("sans-serif")
            .track_focus(&self.focus)
            // Always bordered, so a hovering drag recolours the frame instead of
            // shifting the whole window's contents inward by two pixels.
            .border_2()
            .border_color(if self.drag_over {
                cx.theme().drag_border
            } else {
                gpui::transparent_black()
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
            .child(self.header(count, cx))
            .child(self.controls(cx))
            .child(self.summary(cx))
            .children(self.notices())
            .child(
                // The table gets a surface of its own, so a folder that does not
                // fill the window reads as a short list rather than a layout that
                // ran out half way down.
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_hidden()
                    .rounded_lg()
                    .bg(cx.theme().table)
                    .border_1()
                    .border_color(cx.theme().border)
                    .when(!self.grid, |table| table.child(self.table_header(cx)))
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
                        .p_2()
                        .into_any_element()
                    }),
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

    application()
        // Every `IconName` is an SVG loaded through the app's asset source. Without
        // this the icons resolve to nothing and the toolbar renders as bare words.
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx: &mut App| {
            // Must run before any gpui-component type is constructed.
            gpui_component::init(cx);
            // Dark by default. Judging compression against a bright chrome is a bad idea,
            // and the comparison view is full-bleed imagery either way.
            gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);
            // The stock dark theme paints `primary` white, which makes the one button
            // that commits work a white slab and leaves nothing to point at anything
            // else. This app already has a blue.
            {
                let theme = gpui_component::Theme::global_mut(cx);
                theme.primary = gpui::rgb(0x2f6feb).into();
                theme.primary_hover = gpui::rgb(0x3f7dfa).into();
                theme.primary_active = gpui::rgb(0x2760d4).into();
                theme.primary_foreground = gpui::white();
            }

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
                                audit.slider_quality = value.start();
                                audit.results.clear();
                                audit.schedule_estimate(cx);
                                cx.notify();
                            },
                        )
                        .detach();
                        let mislabelled = entries
                            .iter()
                            .filter(|entry| entry.extension_lies())
                            .count();
                        let mut audit = Audit {
                            root,
                            entries,
                            skipped_raw,
                            heaviest: 0,
                            mislabelled,
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
                            anchor: 0,
                            slider_quality: quality.0.unwrap_or(80.),
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
