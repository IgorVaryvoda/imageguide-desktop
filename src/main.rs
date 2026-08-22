//! ImageGuide Desktop — audit a folder of images without uploading them anywhere.
//!
//! The browser tools on imageguide.dev post files to a worker to convert them. This
//! does the same work locally, so nothing leaves the machine and the folder size is
//! bounded by the disk rather than by a tab.

mod avif;
mod compare;
mod convert;
mod scan;
mod settings;
mod sirv;
mod thumbs;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use compare::Pair;
use convert::{Format, MaxEdge, Quality};
use futures::future::select_all;
use gpui::{
    App, Bounds, Context, Decorations, FocusHandle, Focusable as _, FontWeight, RenderImage,
    ScrollStrategy, UniformListScrollHandle, Window, WindowBounds, WindowOptions, div, img,
    prelude::*, px, rgb, rgba, size, uniform_list,
};
use gpui_component::alert::Alert;
use gpui_component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::progress::Progress;
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::switch::Switch;
use gpui_component::table::{Column as TableCol, ColumnSort, DataTable, TableDelegate, TableState};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Disableable, IconName, Root, Selectable, Sizable};
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
const W_TICK: f32 = 34.;
const W_FORMAT: f32 = 82.;
const W_PIXELS: f32 = 96.;
const W_DENSITY: f32 = 74.;
const W_WEIGHT: f32 = 100.;
const W_RESULT: f32 = 112.;
/// The Sirv diff column. Wide windows only: below 900px the name needs the
/// room more than the status does.
const W_SYNC: f32 = 86.;
const W_FORMAT_COMPACT: f32 = 70.;
const W_PIXELS_COMPACT: f32 = 88.;
const W_DENSITY_COMPACT: f32 = 60.;
const W_WEIGHT_COMPACT: f32 = 86.;
const W_NAME_MIN: f32 = 140.;

/// Bytes per output pixel, banded. A photographic JPEG lands near 0.2; a
/// screenshot saved as PNG can be ten times that. The number was already in the
/// list and every row printed it in the same grey, which made the app's one
/// diagnostic something you had to read rather than see.
const DENSITY_GOOD: f32 = 0.5;
const DENSITY_HEAVY: f32 = 1.5;
/// The smallest compositor window that supports every production view.
const WINDOW_MIN_WIDTH: f32 = 760.;
const WINDOW_MIN_HEIGHT: f32 = 560.;
const WINDOW_DEFAULT_WIDTH: f32 = 900.;
const WINDOW_DEFAULT_HEIGHT: f32 = 640.;
/// Gallery rows stay uniform for virtualisation, but the tile itself grows to use the
/// available surface instead of leaving a dead strip beside three tiny cards.
const TILE_MIN: f32 = 168.;
const TILE_MAX: f32 = 224.;
const TILE_GAP: f32 = 8.;
const GALLERY_MIN_COLUMNS: usize = 1;
const GALLERY_MAX_COLUMNS: usize = 5;
const ROOT_PADDING: f32 = 12.;
const ROOT_BORDER: f32 = 2.;
const GALLERY_PADDING: f32 = 8.;
const GALLERY_BORDER: f32 = 1.;
/// Files encoded to project a total.
///
/// Measured against a real 3.0GB folder that converts to 422.9MB, sweeping which file
/// each slice offers up: 16 slices land anywhere in −53%..+59%, and 32 slices tighten
/// that to −36%..+10%. Samples run together, so 32 of them cost 0.9s on that folder.
/// AVIF remains the expensive path even with libaom, so it settles for three and stays
/// a rough number instead of making each slider stop feel like a conversion.
fn sample_size(format: Format) -> usize {
    match format {
        Format::WebP => 32,
        Format::Avif => 3,
    }
}
/// Settling time before sampling, so dragging the slider does not start a run per pixel.
const ESTIMATE_DELAY: Duration = Duration::from_millis(400);
/// Settling time before building a comparison, so a held arrow key does not queue one
/// full decode and encode per repeat.
const COMPARE_DELAY: Duration = Duration::from_millis(120);
/// Settling time before the window state reaches disk, so a resize drag is one write.
const SETTINGS_SAVE_DELAY: Duration = Duration::from_millis(500);

/// Decoded thumbnails kept in memory at once. A viewport holds a few dozen, so this is
/// still far more than scrolling needs; without it a 5,000-image folder scrolled end to
/// end retains 5,000 decoded thumbnails and a GPU texture for each.
///
/// Lower than it was, because `THUMB_EDGE` grew to fill a gallery tile. A 3:2 thumbnail
/// is about 150KB of texture at that size against 25KB before, so 512 of them would be
/// 75MB of video memory for rows nobody is looking at.
const THUMB_CACHE: usize = 192;

fn is_checkbox_activation_key(event: &gpui::KeyDownEvent) -> bool {
    matches!(event.keystroke.key.as_str(), "space" | "enter")
        && !event.keystroke.modifiers.modified()
}

/// A persisted size can be absent or corrupted. Keep restore policy pure so native
/// startup and tests agree about the supported window.
fn restored_window_size(width: Option<f32>, height: Option<f32>) -> (f32, f32) {
    let width = width
        .filter(|value| value.is_finite())
        .unwrap_or(WINDOW_DEFAULT_WIDTH)
        .max(WINDOW_MIN_WIDTH);
    let height = height
        .filter(|value| value.is_finite())
        .unwrap_or(WINDOW_DEFAULT_HEIGHT)
        .max(WINDOW_MIN_HEIGHT);
    (width, height)
}

/// Geometry for one virtualised gallery row. Band ranges are calculated on demand,
/// so only the bands requested by `uniform_list` allocate tiles.
#[derive(Clone, Copy, Debug, PartialEq)]
struct GalleryLayout {
    columns: usize,
    rows: usize,
    entries: usize,
    tile: f32,
}

impl GalleryLayout {
    fn band_range(self, band: usize) -> std::ops::Range<usize> {
        let first = band.saturating_mul(self.columns).min(self.entries);
        let last = first.saturating_add(self.columns).min(self.entries);
        first..last
    }

    #[cfg(test)]
    fn bands(self) -> impl Iterator<Item = std::ops::Range<usize>> {
        (0..self.rows).map(move |band| self.band_range(band))
    }
}

fn gallery_layout(
    viewport_width: f32,
    root_left: f32,
    root_right: f32,
    entries: usize,
) -> GalleryLayout {
    let chrome = root_left
        + root_right
        + 2. * (ROOT_PADDING + ROOT_BORDER + GALLERY_PADDING + GALLERY_BORDER);
    let available = (viewport_width - chrome).max(0.);
    let columns = ((available + TILE_GAP) / (TILE_MIN + TILE_GAP)) as usize;
    let columns = columns.clamp(GALLERY_MIN_COLUMNS, GALLERY_MAX_COLUMNS);
    let tile = ((available - (columns.saturating_sub(1) as f32 * TILE_GAP)) / columns as f32)
        .clamp(TILE_MIN, TILE_MAX);
    GalleryLayout {
        columns,
        rows: entries.div_ceil(columns),
        entries,
        tile,
    }
}

/// Root owns a one-pixel border on every non-tiled client-decoration edge.
fn root_horizontal_chrome(window: &Window) -> (f32, f32) {
    let paddings = gpui_component::window_paddings(window);
    let (left_border, right_border) = match window.window_decorations() {
        Decorations::Client { tiling } => {
            ((!tiling.left) as u8 as f32, (!tiling.right) as u8 as f32)
        }
        Decorations::Server => (0., 0.),
    };
    (
        f32::from(paddings.left) + left_border,
        f32::from(paddings.right) + right_border,
    )
}

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
        .text_size(px(11.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(colour)
        .child(text.into())
}

/// A proportional bar. The audit is a ranking and a column of numbers does not
/// rank — 632 KB and 104 KB were set in the same size and colour, so the shape of
/// the folder was invisible in a list sorted by exactly that.
fn meter(
    id: impl Into<gpui::ElementId>,
    fraction: f32,
    colour: gpui::Hsla,
    height: f32,
) -> Progress {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0., 1.)
    } else {
        0.
    };
    Progress::new(id)
        .value(fraction * 100.)
        .color(colour)
        .h(px(height))
}

/// One option in a segmented control.
///
/// The variant is set per option rather than on the group, because a group applies
/// its own variant to every child — and an outlined group draws the selected child
/// in the *active* style, which on a near-black theme is the same colour as the
/// unselected ones. Which option was chosen has to be legible.
fn segment(
    id: impl Into<gpui::ElementId>,
    label: impl Into<gpui::SharedString>,
    selected: bool,
) -> Button {
    let button = Button::new(id).label(label).selected(selected);
    if selected {
        button.primary()
    } else {
        button.outline()
    }
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
    /// The order `thumbs` filled up in, so the oldest decode is the one that leaves
    /// when the cache reaches `THUMB_CACHE`.
    thumb_order: VecDeque<usize>,
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
    /// The immutable denominator owned by the active conversion.
    active_target_count: Option<usize>,
    /// Names of files a conversion could not read or write. Kept rather than counted,
    /// because "3 failed" without saying which is not a report.
    failures: Vec<String>,
    /// Files in the folder that claim to be images and will not decode, by name. A
    /// count alone says a folder has a problem and gives you nowhere to look.
    unreadable: Vec<PathBuf>,
    /// Files already sitting in the output folder when this one was scanned.
    existing_output: usize,
    /// A drag is hovering over the window.
    drag_over: bool,
    /// The open side-by-side view, if any.
    compare: Option<Comparison>,
    /// The paired Sirv folder, if any: the client, the remote path, and its
    /// listing keyed by the same relative keys the local rows use.
    sirv_pairing: Option<SirvPairing>,
    /// How the local dataset stands against it: files to push, files that
    /// differ, files to pull. Recomputed when the dataset or the listing
    /// changes, never per frame.
    sirv_counts: Option<(usize, usize, usize)>,
    /// A running or finished Sirv transfer, shown in the notices line.
    sirv_job: Option<SirvJob>,
    /// Bumped whenever a running transfer stops being wanted.
    sirv_generation: u64,
    /// The open remote-folder browser.
    sirv_browser: Option<SirvBrowser>,
    /// The open settings overlay.
    settings_panel: Option<SettingsPanel>,
    /// How the list is ordered.
    sort: Sort,
    /// Indices into `entries`, filtered and sorted. `entries` itself never moves, so
    /// thumbnails, ticks and results stay attached to their file through both.
    visible: Vec<usize>,
    /// Substring the name must contain, lowercased. Empty shows everything.
    filter: String,
    /// The finding the list is narrowed to, if any. Sits alongside the name filter
    /// rather than replacing it, so you can search within one.
    finding: Option<Finding>,
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
    /// The gallery scroll state survives renders so a width transition can reset it.
    gallery_scroll: UniformListScrollHandle,
    /// The column count laid out last frame. `None` deliberately leaves initial layout alone.
    gallery_columns: Option<usize>,
    /// Projected output size for the current settings, and how many files were
    /// actually encoded to get it.
    estimate: Option<(u64, usize)>,
    /// Bumped on every settings change so a slow sample can tell it is stale. Dragging
    /// the quality slider fires dozens of these.
    estimate_generation: u64,
    /// Invalidates detached work when a new folder or file is installed.
    dataset_generation: u64,
    /// Invalidates older folder-open requests while a newer scan is pending.
    scan_generation: u64,
    /// The path currently being scanned, if any.
    scanning: Option<String>,
    /// Keyboard target. Without one the window gets no key events at all.
    focus: FocusHandle,
    /// Last title pushed to the compositor, so render does not set it every frame.
    titled: String,
    /// Last state render asked to store, so render only schedules a write when it
    /// changes.
    settings: settings::Settings,
    /// A delayed save is already waiting; it reads `settings` when it fires, so a
    /// whole resize drag needs one task and one write.
    settings_save_pending: bool,
    /// The last pair built, kept so closing and reopening the same image is instant.
    // ponytail: one entry. A pair holds two full-size RGBA buffers — 165 MB for a
    // 5568x3712 photo — so a bigger cache would need a byte budget, not a count.
    cached: Option<(compare::Key, Arc<Pair>)>,
    /// Bytes of the heaviest visible file, so every row's weight bar is drawn
    /// against the same scale. Cached because the alternative is a scan of the
    /// whole list once per row.
    heaviest: u64,
    /// Cached with `heaviest`; progress and thumbnail redraws must not rescan the
    /// entire visible folder just to rebuild the header.
    visible_bytes: u64,
    /// Files whose extension disagrees with their contents. Counted once when the
    /// folder is read, because the check allocates and the filter box would
    /// otherwise redo it for every entry on every keystroke.
    mislabelled: usize,
    /// The list, which the component library owns. It holds a weak handle back to
    /// this audit and reads its rows through that, so it cannot be built until this
    /// audit is a live entity: `TableState::new` asks the delegate for its row and
    /// column counts straight away, and answering that means reading the audit.
    table: Option<gpui::Entity<TableState<AuditTable>>>,
    /// Width/result signature last handed to the component table.
    table_signature: Option<(u32, bool)>,
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

/// A paired Sirv folder. `files` maps the relative keys `sirv::relative_key`
/// produces for local rows onto the remote listing, so the diff column is a
/// lookup, never a walk. `None` while the recursive listing is in flight —
/// a pairing that just happened does not know its diff yet.
/// What the paired folder's remote listing knows.
///
/// This was an `Option<HashMap<..>>`, so `None` meant both "the walk is running" and
/// "the walk failed". The window showed the first and reported the second as a pull
/// that transferred nothing — the same confusion the comparison view had between
/// loading and failed, and the same fix.
enum Listing {
    Walking,
    Failed(String),
    Ready(HashMap<String, sirv::Node>),
}

struct SirvPairing {
    dir: String,
    files: Listing,
    client: Arc<parking_lot::Mutex<sirv::Client>>,
}

/// What a background Sirv job is doing, and how far it got. Failures keep
/// names, because "2 failed" is not a report.
#[derive(Clone, Copy, PartialEq)]
enum SirvJobKind {
    Pull,
    Push,
}

struct SirvJob {
    kind: SirvJobKind,
    done: usize,
    total: usize,
    failures: Vec<String>,
    finished: bool,
    /// The transfer generation this job belongs to. Unpairing or opening another
    /// folder bumps `sirv_generation`, and the loop stops at its next file rather
    /// than uploading the rest of a folder nobody is paired to any more.
    generation: u64,
}

/// The remote-folder browser: one path, its listing, and its own focus so
/// Escape closes it rather than the thing underneath.
struct SirvBrowser {
    client: Arc<parking_lot::Mutex<sirv::Client>>,
    path: String,
    /// `None` while the listing is in flight.
    nodes: Option<Result<Vec<sirv::Node>, String>>,
    /// Bumped per request, so a listing for a folder the user has already left
    /// cannot overwrite the one they are looking at.
    generation: u64,
    focus: gpui::FocusHandle,
}

/// The settings overlay: the CDN credentials, and nothing else. Inputs are entities
/// so the framework owns their editing state.
struct SettingsPanel {
    client_id: gpui::Entity<InputState>,
    client_secret: gpui::Entity<InputState>,
    /// (ok?, message) per section.
    cdn_status: Option<(bool, String)>,
    /// Which form field holds focus, as an index into the field list.
    focus_ix: usize,
    /// The panel has taken focus already. Without this the next render put focus back
    /// in the first field, so Tab and a click into another field both came undone the
    /// moment a save or a status message redrew the audit.
    focused: bool,
}

struct Comparison {
    index: usize,
    dataset_generation: u64,
    key: compare::Key,
    /// `None` while the two sides are still decoding.
    pair: Option<Arc<Pair>>,
    /// A completed build can fail after the initial loading frame.
    failed: bool,
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

/// Write the remembered state, except in tests, where a render must not touch the
/// user's real config file.
fn write_settings(settings: &settings::Settings) {
    #[cfg(not(test))]
    settings::save(settings);
    #[cfg(test)]
    let _ = (settings, settings::save as fn(&settings::Settings));
}

impl Audit {
    /// Remember the window state without putting a disk write inside a frame.
    /// Dragging a window edge changes the size on every frame, and the old code
    /// answered each one with `create_dir_all` plus `write` on the UI thread. One
    /// delayed save collects the whole drag and stores the size it ended at.
    fn remember_settings(&mut self, settings: settings::Settings, cx: &mut Context<Self>) {
        self.settings = settings;
        if self.settings_save_pending {
            return;
        }
        self.settings_save_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTINGS_SAVE_DELAY).await;
            let Ok(settings) = this.update(cx, |audit, _| {
                audit.settings_save_pending = false;
                audit.settings.clone()
            }) else {
                return;
            };
            cx.background_executor()
                .spawn(async move { write_settings(&settings) })
                .detach();
        })
        .detach();
    }

    /// The rows a conversion would touch. An empty selection means the whole folder,
    /// so the common case needs no ticking.
    fn targets(&self) -> Vec<usize> {
        conversion_targets(&self.visible, &self.selected)
    }

    fn target_count(&self) -> usize {
        if self.selected.is_empty() {
            self.visible.len()
        } else {
            self.visible
                .iter()
                .filter(|index| self.selected.contains(index))
                .count()
        }
    }

    fn target_bytes(&self) -> u64 {
        self.visible
            .iter()
            .filter(|index| self.selected.is_empty() || self.selected.contains(index))
            .filter_map(|index| self.entries.get(*index))
            .map(|entry| entry.bytes)
            .sum()
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
        if self.converting || self.scanning.is_some() {
            return;
        }
        let targets = self.targets();
        if targets.is_empty() {
            return;
        }
        let target_count = targets.len();
        let dataset_generation = self.dataset_generation;
        self.converting = true;
        self.active_target_count = Some(target_count);
        self.results.clear();
        self.failures.clear();
        cx.notify();

        let root = self.root.clone();
        let out_dir = self.root.join(scan::OUTPUT_DIR);
        let quality = self.quality;
        let format = self.format;
        let max_edge = self.max_edge;
        let sources: Vec<(usize, PathBuf)> = targets
            .into_iter()
            .filter_map(|index| Some((index, self.entries.get(index)?.path.clone())))
            .collect();
        // Two sources can want one output name, so the whole run picks its names
        // together before any of it writes.
        let paths: Vec<PathBuf> = sources.iter().map(|(_, path)| path.clone()).collect();
        let planned = convert::plan_outputs(&root, &paths, &out_dir, format);
        let sources: Vec<(usize, PathBuf, PathBuf)> = sources
            .into_iter()
            .zip(planned)
            .map(|((index, source), written)| (index, source, written))
            .collect();

        cx.spawn(async move |this, cx| {
            // A sliding window rather than batches. Batching waited for all eight of a
            // chunk before starting the ninth, so one 40MB photo held seven workers
            // idle; here a finished file is replaced immediately. The window is what
            // bounds memory: every file in flight holds a fully decoded image.
            let workers = convert::workers(format);
            let mut inflight: Vec<gpui::Task<(usize, Option<convert::Converted>)>> = Vec::new();
            let mut queued = sources.iter();
            let mut completed = Vec::with_capacity(workers);

            loop {
                while inflight.len() < workers {
                    let Some((index, source, written)) = queued.next() else {
                        break;
                    };
                    let (index, source, written) = (*index, source.clone(), written.clone());
                    inflight.push(cx.background_executor().spawn(async move {
                        (
                            index,
                            convert::convert_to(&source, &written, format, quality, max_edge),
                        )
                    }));
                }
                if inflight.is_empty() {
                    break;
                }
                // Take whichever file finishes first. Waiting for source order here
                // quietly turns one slow image back into a batch barrier.
                let ((index, result), _, remaining) = select_all(inflight).await;
                inflight = remaining;
                completed.push((index, result));

                // Publishing once per file made a 6,000-image conversion rebuild the
                // same window 6,000 times. One worker-window keeps progress live while
                // cutting UI invalidations by 87.5% for WebP.
                let work_remaining = !inflight.is_empty() || !queued.as_slice().is_empty();
                if !progress_batch_ready(completed.len(), workers, work_remaining) {
                    continue;
                }
                let batch = std::mem::take(&mut completed);

                if this
                    .update(cx, |audit, cx| {
                        if audit.dataset_generation != dataset_generation {
                            return;
                        }
                        for (index, result) in batch {
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
                if audit.dataset_generation == dataset_generation {
                    audit.converting = false;
                    audit.active_target_count = None;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Rebuild the filtered, sorted view. Nothing keyed by entry index is touched:
    /// a file keeps its thumbnail, its tick and its result through any re-ordering.
    fn refresh_visible(&mut self) {
        let needle = self.filter.to_lowercase();
        let finding = self.finding;
        let mut visible: Vec<usize> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| needle.is_empty() || entry.name().to_lowercase().contains(&needle))
            .filter(|(_, entry)| finding.is_none_or(|finding| finding.holds(entry)))
            .map(|(index, _)| index)
            .collect();

        let entries = &self.entries;
        let sort = self.sort;
        visible.sort_by(|a, b| compare_entries(&entries[*a], &entries[*b], sort));

        self.cursor = self.cursor.min(visible.len().saturating_sub(1));
        // Weight bars are drawn against the heaviest file on screen, so filtering
        // down to the small ones still spreads them across the column instead of
        // leaving every bar a stub.
        (self.heaviest, self.visible_bytes) = visible
            .iter()
            .filter_map(|index| self.entries.get(*index))
            .fold((0, 0), |(heaviest, total), entry| {
                (heaviest.max(entry.bytes), total + entry.bytes)
            });
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
        if self.converting {
            return;
        }
        self.filter = filter;
        self.refresh_visible();
        self.schedule_estimate(cx);
        cx.notify();
    }

    /// Narrow the list to one finding, or widen it again if that finding already holds.
    /// A second click on a lit control has to turn it off, the way Lossless does.
    fn set_finding(&mut self, finding: Finding, cx: &mut Context<Self>) {
        if self.converting {
            return;
        }
        self.finding = (self.finding != Some(finding)).then_some(finding);
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
        let dataset_generation = self.dataset_generation;

        let targets = self.targets();
        if targets.is_empty() {
            return;
        }

        let (format, quality, max_edge) = (self.format, self.quality, self.max_edge);
        let slices = sample_size(format).min(targets.len());
        // One sample per slice of the list, taken from the middle of it. The list is
        // weight-sorted, so the first file of a slice is its heaviest and the least
        // like the rest of it.
        let strata: Vec<Stratum> = (0..slices)
            .filter_map(|slice| {
                let start = slice * targets.len() / slices;
                let end = (slice + 1) * targets.len() / slices;
                let entry = self.entries.get(*targets.get((start + end) / 2)?)?;
                Some(Stratum {
                    path: entry.path.clone(),
                    bytes: entry.bytes,
                    slice_bytes: targets[start..end]
                        .iter()
                        .filter_map(|index| self.entries.get(*index))
                        .map(|entry| entry.bytes)
                        .sum(),
                })
            })
            .collect();

        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(ESTIMATE_DELAY).await;
            if this
                .read_with(cx, |audit, _| {
                    audit.estimate_generation != generation
                        || audit.dataset_generation != dataset_generation
                })
                .unwrap_or(true)
            {
                return;
            }

            // The samples are independent, so they run together, as many at once as a
            // conversion allows. That is what pays for a sample wide enough to trust:
            // 32 WebP samples of a 3.0GB folder take 0.9s, inside the wait the status
            // bar already shows as "Sizing it up…".
            let concurrency = convert::workers(format);
            let mut inflight: Vec<gpui::Task<(u64, u64, Option<u64>)>> = Vec::new();
            let mut queued = strata.iter();
            let mut sampled = Vec::with_capacity(strata.len());

            loop {
                while inflight.len() < concurrency {
                    let Some(stratum) = queued.next() else {
                        break;
                    };
                    let path = stratum.path.clone();
                    let (slice_bytes, bytes) = (stratum.slice_bytes, stratum.bytes);
                    inflight.push(cx.background_executor().spawn(async move {
                        let encoded = scan::decode(&path)
                            .map(|image| max_edge.apply(image))
                            .and_then(|image| convert::encode(&image, format, quality))
                            .map(|encoded| encoded.len() as u64);
                        (slice_bytes, bytes, encoded)
                    }));
                }
                if inflight.is_empty() {
                    break;
                }
                let ((slice_bytes, bytes, encoded), _, remaining) = select_all(inflight).await;
                inflight = remaining;
                sampled.push((slice_bytes, encoded.map(|encoded| (bytes, encoded))));
            }

            let Some((projected, counted)) = project_total(&sampled) else {
                return;
            };
            let _ = this.update(cx, |audit, cx| {
                // A newer change started while this was encoding.
                if audit.estimate_generation == generation
                    && audit.dataset_generation == dataset_generation
                {
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
        if self.grid {
            let columns = self.gallery_columns.unwrap_or(1).max(1);
            self.gallery_scroll
                .scroll_to_item_strict(self.cursor / columns, ScrollStrategy::Nearest);
        } else if let Some(table) = self.table.clone() {
            let visible = table.read(cx).visible_range().rows().clone();
            if !visible.contains(&self.cursor) {
                table.update(cx, |table, cx| table.scroll_to_row(self.cursor, cx));
            }
        }
        cx.notify();
    }

    /// One keyboard step. With shift held it is a selection drag: the run from
    /// the anchor to the new cursor joins the selection, exactly as a
    /// shift-click does.
    fn step_cursor(&mut self, delta: isize, extend: bool, cx: &mut Context<Self>) {
        self.move_cursor(delta, cx);
        if extend {
            self.select_through_cursor(cx);
        }
    }

    /// Left and right: one row in the list, one tile across in the gallery.
    fn step_cursor_lateral(&mut self, direction: isize, extend: bool, cx: &mut Context<Self>) {
        let columns = if self.grid {
            self.gallery_columns.unwrap_or(1).max(1) as isize
        } else {
            1
        };
        self.step_cursor(direction * columns, extend, cx);
    }

    fn select_through_cursor(&mut self, cx: &mut Context<Self>) {
        if self.converting {
            return;
        }
        let (from, to) = if self.anchor <= self.cursor {
            (self.anchor, self.cursor)
        } else {
            (self.cursor, self.anchor)
        };
        let run: Vec<usize> = (from..=to).filter_map(|row| self.entry_at(row)).collect();
        self.selected.extend(run);
        self.schedule_estimate(cx);
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

        if self.converting {
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
        if self.converting {
            return;
        }
        let Some(entry) = self.entry_at(self.cursor) else {
            return;
        };
        if !self.selected.remove(&entry) {
            self.selected.insert(entry);
        }
        self.schedule_estimate(cx);
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
    fn tile(
        &self,
        row: usize,
        index: usize,
        tile_size: f32,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(entry) = self.entries.get(index) else {
            return div().id(("tile", row));
        };
        let thumb = self.thumbs.get(&index).cloned();
        let ticked = self.selected.contains(&index);

        let density = entry.bytes_per_pixel();

        div()
            .id(("tile", row))
            .w(px(tile_size))
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
            .when(ticked, |tile| {
                tile.bg(cx.theme().list_active)
                    .border_color(cx.theme().list_active_border)
            })
            .when(row == self.cursor, |tile| {
                tile.border_color(cx.theme().ring)
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
                    .h(px(tile_size - 68.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .overflow_hidden()
                    .bg(cx.theme().background)
                    .when_some(thumb, |slot, image| {
                        slot.child(
                            img(image)
                                .max_w(px(tile_size - 16.))
                                .max_h(px(tile_size - 68.)),
                        )
                    })
                    // The grid had no way to tick anything; the keyboard was the
                    // only route to a selection you could see in the list.
                    .child(
                        div()
                            .absolute()
                            .top(px(4.))
                            .left(px(4.))
                            .debug_selector(move || format!("grid-checkbox-{index}"))
                            .on_key_down(cx.listener(|_, event, _, cx| {
                                if is_checkbox_activation_key(event) {
                                    cx.stop_propagation();
                                }
                            }))
                            .child(
                                Checkbox::new(("tile-tick", index))
                                    .checked(ticked)
                                    .on_click(cx.listener(move |audit, _: &bool, _, cx| {
                                        cx.stop_propagation();
                                        if audit.converting {
                                            return;
                                        }
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
                                .text_size(px(10.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(format_colour(entry.format, cx))
                                .child(format_name(entry.format)),
                        ),
                    )
                    // The same word the table's Sirv column uses. The gallery used to
                    // show nothing at all, so switching to grid lost the diff.
                    .children(self.sync_label(entry, cx).map(|(label, colour)| {
                        div().absolute().top(px(4.)).right(px(4.)).child(
                            div()
                                .px_1()
                                .rounded_sm()
                                .bg(cx.theme().background.opacity(0.8))
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(10.))
                                .text_color(colour)
                                .child(label),
                        )
                    })),
            )
            .child(
                div()
                    .w_full()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(12.))
                    .text_color(cx.theme().foreground)
                    .child(entry.name()),
            )
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .justify_between()
                    .font_family(cx.theme().mono_font_family.clone())
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

    /// Install a completed scan. This is the one state transition that replaces the
    /// dataset and invalidates every detached job derived from the old rows.
    fn install_dataset(
        &mut self,
        scanned: scan::Scan,
        root: PathBuf,
        single: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dataset_generation = self.dataset_generation.wrapping_add(1);
        self.estimate_generation = self.estimate_generation.wrapping_add(1);
        self.estimate = None;
        self.converting = false;
        self.active_target_count = None;
        self.root = root;
        self.mislabelled = scanned
            .entries
            .iter()
            .filter(|entry| entry.extension_lies())
            .count();
        self.entries = scanned.entries;
        // The scroll handle belongs to the gallery rather than its data. A new folder
        // can have the same column count, so a render-time column transition cannot
        // be relied on to bring its first image into view.
        self.gallery_scroll
            .scroll_to_item_strict(0, ScrollStrategy::Top);
        self.skipped_raw = scanned.skipped_raw;
        self.unreadable = scanned.unreadable;
        self.existing_output = scanned.existing_output;
        self.thumbs.clear();
        self.thumb_order.clear();
        self.requested.clear();
        self.selected.clear();
        self.results.clear();
        self.failures.clear();
        self.compare = None;
        self.cached = None;
        self.filter.clear();
        // A finding belongs to the folder it was found in. Carrying it over would show
        // the new folder narrowed to something nobody asked about.
        self.finding = None;
        self.filter_input.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.cursor = 0;
        self.anchor = 0;
        self.refresh_visible();
        // A new folder is a new diff: the pairing survives, the numbers do not, and a
        // transfer aimed at the old folder must not keep running against the new one.
        self.cancel_sirv_transfer();
        self.refresh_sirv_counts();
        self.schedule_estimate(cx);
        cx.notify();

        if single {
            self.open_compare(0, cx);
        }
    }

    /// Scan a requested path away from the UI thread. A newer request wins, while a
    /// failed current request leaves the last usable dataset in place.
    fn request_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.converting {
            return;
        }
        let single = path.is_file();
        if !single && !path.is_dir() {
            return;
        }
        self.scan_generation = self.scan_generation.wrapping_add(1);
        let request = self.scan_generation;
        let label = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        self.scanning = Some(label);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if single {
                        let entry = scan::probe(&path)?;
                        let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
                        Some((
                            scan::Scan {
                                entries: vec![entry],
                                skipped_raw: 0,
                                unreadable: Vec::new(),
                                existing_output: 0,
                            },
                            root,
                            true,
                        ))
                    } else {
                        Some((scan::scan(&path), path, false))
                    }
                })
                .await;

            let _ = this.update_in(cx, |audit, window, cx| {
                if audit.scan_generation != request {
                    return;
                }
                audit.scanning = None;
                if let Some((scanned, root, single)) = result {
                    audit.install_dataset(scanned, root, single, window, cx);
                } else {
                    cx.notify();
                }
            });
        })
        .detach();
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
        if self.converting {
            return;
        }
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
                let _ = this.update_in(cx, |audit, window, cx| {
                    audit.request_path(path, cx);
                    window.refresh();
                });
            }
        })
        .detach();
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
        // The word sits left of its control, in the same muted voice as the
        // rest of the strip's metadata.
        div()
            .flex()
            .items_center()
            .gap_2()
            .flex_shrink_0()
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_nowrap()
                    .child(label),
            )
            .child(group.small().compact())
    }

    /// Open the remote-folder browser. Credentials come from the Sirv store; a
    /// missing store opens the browser on an error that names the file to fix.
    fn open_sirv_browser(&mut self, cx: &mut Context<Self>) {
        // A live pairing already holds a warm client; reuse it so the browser
        // and later pushes share one token cache.
        let client = self
            .sirv_pairing
            .as_ref()
            .map(|pairing| pairing.client.clone());
        let client = match client {
            Some(client) => client,
            None => {
                let Some(credentials) = sirv::load_credentials() else {
                    let message = format!(
                        "No Sirv credentials. Add client_id and client_secret to {}",
                        sirv::credentials_path()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "the ImageGuide config file".into())
                    );
                    self.sirv_browser = Some(SirvBrowser {
                        // Never used on this path: the listing is already an error.
                        client: Arc::new(parking_lot::Mutex::new(sirv::Client::new(
                            sirv::Credentials {
                                client_id: String::new(),
                                client_secret: String::new(),
                            },
                        ))),
                        path: "/".into(),
                        nodes: Some(Err(message)),
                        generation: 0,
                        focus: cx.focus_handle(),
                    });
                    cx.notify();
                    return;
                };
                Arc::new(parking_lot::Mutex::new(sirv::Client::new(credentials)))
            }
        };
        let mut browser = SirvBrowser {
            client,
            path: "/".into(),
            nodes: None,
            generation: 0,
            focus: cx.focus_handle(),
        };
        if let Some(pairing) = &self.sirv_pairing {
            browser.path = pairing.dir.clone();
        }
        self.sirv_browser = Some(browser);
        let state = self.sirv_browser.as_mut().unwrap();
        Self::browse_sirv_path(state, cx);
        cx.notify();
    }

    /// Fetch the listing for the browser's current path in the background.
    ///
    /// Clicking into two folders in quick succession used to leave whichever listing
    /// answered last on screen, under whichever path the header showed. The generation
    /// makes a superseded listing land nowhere.
    fn browse_sirv_path(browser: &mut SirvBrowser, cx: &mut Context<Self>) {
        browser.generation = browser.generation.wrapping_add(1);
        let request = browser.generation;
        browser.nodes = None;
        let client = browser.client.clone();
        let path = browser.path.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    client
                        .lock()
                        .readdir(&path)
                        .map_err(|error| error.to_string())
                })
                .await;
            this.update(cx, |audit, cx| {
                if let Some(browser) = audit.sirv_browser.as_mut()
                    && browser.generation == request
                {
                    browser.nodes = Some(result);
                    cx.notify();
                }
            })
        })
        .detach();
    }

    /// Enter a folder of the listing.
    fn descend_sirv(&mut self, name: String, cx: &mut Context<Self>) {
        let Some(browser) = self.sirv_browser.as_mut() else {
            return;
        };
        if !browser.path.ends_with('/') {
            browser.path.push('/');
        }
        browser.path.push_str(&name);
        Self::browse_sirv_path(browser, cx);
        cx.notify();
    }

    /// Go up one folder. The root has no parent, so the button only exists
    /// below it.
    fn ascend_sirv(&mut self, cx: &mut Context<Self>) {
        let Some(browser) = self.sirv_browser.as_mut() else {
            return;
        };
        let trimmed = browser.path.trim_end_matches('/').to_string();
        let Some((parent, _)) = trimmed.rsplit_once('/') else {
            return;
        };
        browser.path = if parent.is_empty() {
            "/".into()
        } else {
            parent.to_string()
        };
        Self::browse_sirv_path(browser, cx);
        cx.notify();
    }

    /// Pair the browsed folder, then list it recursively in the background.
    /// The pairing exists immediately (the header names it); its diff arrives
    /// when the walk lands.
    fn pair_sirv(&mut self, cx: &mut Context<Self>) {
        let (client, dir) = {
            let Some(browser) = self.sirv_browser.as_ref() else {
                return;
            };
            (
                browser.client.clone(),
                browser.path.trim_end_matches('/').to_string(),
            )
        };
        self.sirv_pairing = Some(SirvPairing {
            dir: dir.clone(),
            files: Listing::Walking,
            client,
        });
        self.sirv_counts = None;
        self.sirv_browser = None;
        cx.notify();
        self.walk_sirv_pairing(cx);
    }

    /// List the paired folder end to end and rebuild its diff. Also the
    /// refresh a push finishes with, so pushed files stop reading as new.
    fn walk_sirv_pairing(&mut self, cx: &mut Context<Self>) {
        let Some(pairing) = &self.sirv_pairing else {
            return;
        };
        let client = pairing.client.clone();
        let dir = pairing.dir.clone();
        let generation = self.dataset_generation;
        cx.spawn(async move |this, cx| {
            let walked = cx
                .background_executor()
                .spawn(async move { client.lock().walk(&dir).map_err(|error| error.to_string()) })
                .await;
            this.update(cx, |audit, cx| {
                // A folder swap mid-walk retires this listing with the rest of
                // the detached work the old dataset owned.
                if audit.dataset_generation != generation {
                    return;
                }
                let Some(pairing) = audit.sirv_pairing.as_mut() else {
                    return;
                };
                match walked {
                    Ok(nodes) => {
                        let dir = pairing.dir.clone();
                        pairing.files = Listing::Ready(
                            nodes
                                .into_iter()
                                .filter_map(|node| {
                                    sirv::unpair_remote(&dir, &node.filename).map(|key| (key, node))
                                })
                                .collect(),
                        );
                        audit.refresh_sirv_counts();
                    }
                    // A listing that failed is not a transfer that failed. It used to
                    // be reported as "Sirv pull: 0 of 0, 1 failed", which named the
                    // wrong operation and left `files` looking like a walk still
                    // running.
                    Err(message) => pairing.files = Listing::Failed(message),
                }
                cx.notify();
            })
        })
        .detach();
    }

    fn unpair_sirv(&mut self, cx: &mut Context<Self>) {
        self.sirv_pairing = None;
        self.sirv_counts = None;
        self.sirv_browser = None;
        self.cancel_sirv_transfer();
        cx.notify();
    }

    /// True when this transfer is no longer the one the window wants, or the window
    /// is gone. Checked before each file rather than after, so nothing new starts.
    fn sirv_superseded(
        this: &gpui::WeakEntity<Self>,
        cx: &mut gpui::AsyncApp,
        generation: u64,
    ) -> bool {
        this.read_with(cx, |audit, _| audit.sirv_generation != generation)
            .unwrap_or(true)
    }

    /// Retire any running transfer. The loop checks the generation before each file,
    /// so the file in flight finishes and nothing after it starts.
    fn cancel_sirv_transfer(&mut self) {
        self.sirv_generation = self.sirv_generation.wrapping_add(1);
        if let Some(job) = self.sirv_job.as_mut()
            && !job.finished
        {
            job.finished = true;
            job.failures.push("stopped".into());
        }
    }

    /// Open settings, prefilled with whatever is stored.
    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let stored = sirv::load_credentials();
        let mut make_input = |value: Option<String>| {
            cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                if let Some(value) = value {
                    state.set_value(value, window, cx);
                }
                state
            })
        };
        self.settings_panel = Some(SettingsPanel {
            client_id: make_input(stored.as_ref().map(|c| c.client_id.clone())),
            client_secret: make_input(stored.as_ref().map(|c| c.client_secret.clone())),
            cdn_status: None,
            focus_ix: 0,
            focused: false,
        });
        cx.notify();
    }

    /// Store the CDN credentials.
    fn save_sirv_settings(&mut self, cx: &mut Context<Self>) {
        let Some(panel) = self.settings_panel.as_mut() else {
            return;
        };
        let client_id = panel.client_id.read(cx).value().trim().to_string();
        let client_secret = panel.client_secret.read(cx).value().trim().to_string();
        if client_id.is_empty() || client_secret.is_empty() {
            panel.cdn_status = Some((false, "Both fields are required.".into()));
            cx.notify();
            return;
        }
        // Report what happened, not what was attempted. A read-only config directory
        // used to look exactly like success.
        panel.cdn_status = Some(
            match sirv::save_credentials(&sirv::Credentials {
                client_id,
                client_secret,
            }) {
                Ok(()) => (true, "Saved.".into()),
                Err(error) => (false, format!("Could not save: {error}")),
            },
        );
        cx.notify();
    }

    /// A transfer is already running. One at a time: the client serialises on
    /// its token cache anyway, and two progress lines would lie about order.
    fn sirv_busy(&self) -> bool {
        self.sirv_job.as_ref().is_some_and(|job| !job.finished)
    }

    /// Download every remote file the local folder lacks. Existing files are
    /// never overwritten — pull is additive by design, so it can never destroy
    /// local work.
    fn start_pull(&mut self, cx: &mut Context<Self>) {
        let Some(pairing) = &self.sirv_pairing else {
            return;
        };
        if self.sirv_busy() {
            return;
        }
        let Listing::Ready(files) = &pairing.files else {
            return;
        };
        let files = files.clone();
        let dir = pairing.dir.clone();
        let client = pairing.client.clone();
        let remote: Vec<sirv::Node> = files.values().cloned().collect();
        let local_keys: HashSet<String> = self
            .entries
            .iter()
            .filter_map(|entry| sirv::relative_key(&self.root, &entry.path))
            .collect();
        let plan = sirv::pull_plan(&remote, &dir, &local_keys);
        if plan.is_empty() {
            return;
        }
        let total = plan.len();
        self.sirv_generation = self.sirv_generation.wrapping_add(1);
        let generation = self.sirv_generation;
        self.sirv_job = Some(SirvJob {
            kind: SirvJobKind::Pull,
            done: 0,
            total,
            failures: Vec::new(),
            finished: false,
            generation,
        });
        cx.notify();

        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let mut failures = Vec::new();
            for (ix, key) in plan.iter().enumerate() {
                if Self::sirv_superseded(&this, cx, generation) {
                    return;
                }
                let outcome = cx
                    .background_executor()
                    .spawn({
                        let client = client.clone();
                        let remote_path = format!("{dir}/{key}");
                        async move { client.lock().download(&remote_path) }
                    })
                    .await;
                let written = match outcome {
                    Ok(bytes) => {
                        let target = root.join(key);
                        let dirs_ok = target
                            .parent()
                            .is_none_or(|parent| std::fs::create_dir_all(parent).is_ok());
                        dirs_ok && std::fs::write(&target, bytes).is_ok()
                    }
                    Err(_) => false,
                };
                if !written {
                    failures.push(key.clone());
                }
                this.update(cx, |audit, cx| {
                    // Only onto this loop's own job. A slow last file can land after
                    // the user has already started another transfer.
                    if let Some(job) = audit.sirv_job.as_mut()
                        && job.generation == generation
                    {
                        job.done = ix + 1;
                        job.failures = failures.clone();
                        cx.notify();
                    }
                })
                .ok();
            }
            this.update(cx, |audit, cx| {
                if let Some(job) = audit.sirv_job.as_mut()
                    && job.generation == generation
                {
                    job.finished = true;
                }
                // The pulled files belong in the table: a full rescan, through
                // the same path a folder change takes.
                audit.request_path(audit.root.clone(), cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Upload every local file Sirv lacks. Changed files are left alone in
    /// both directions; overwriting is a decision, not a side effect.
    fn start_push(&mut self, cx: &mut Context<Self>) {
        let Some(pairing) = &self.sirv_pairing else {
            return;
        };
        if self.sirv_busy() {
            return;
        }
        let Listing::Ready(files) = &pairing.files else {
            return;
        };
        let dir = pairing.dir.clone();
        let client = pairing.client.clone();
        let plan: Vec<(String, PathBuf)> = self
            .entries
            .iter()
            .filter_map(|entry| {
                let key = sirv::relative_key(&self.root, &entry.path)?;
                (sirv::classify(entry.bytes, files.get(&key)) == sirv::SyncState::OnlyLocal)
                    .then(|| (key, entry.path.clone()))
            })
            .collect();
        if plan.is_empty() {
            return;
        }
        let total = plan.len();
        self.sirv_generation = self.sirv_generation.wrapping_add(1);
        let generation = self.sirv_generation;
        self.sirv_job = Some(SirvJob {
            kind: SirvJobKind::Push,
            done: 0,
            total,
            failures: Vec::new(),
            finished: false,
            generation,
        });
        cx.notify();

        let folders = sirv::push_folders(plan.iter().map(|(key, _)| key));

        cx.spawn(async move |this, cx| {
            let mut failures = Vec::new();

            let made = cx
                .background_executor()
                .spawn({
                    let client = client.clone();
                    let dir = dir.clone();
                    async move {
                        let mut client = client.lock();
                        for folder in &folders {
                            // mkdir on an existing folder is success upstream, so this
                            // is "ensure", not "create".
                            if client.mkdir(&format!("{dir}/{folder}")).is_err() {
                                return Err(format!("could not create folder {folder}"));
                            }
                        }
                        Ok(())
                    }
                })
                .await;
            if let Err(message) = made {
                failures.push(message);
            }

            for (ix, (key, path)) in plan.iter().enumerate() {
                if Self::sirv_superseded(&this, cx, generation) {
                    return;
                }
                let outcome = cx
                    .background_executor()
                    .spawn({
                        let client = client.clone();
                        let key = key.clone();
                        let path = path.clone();
                        let dir = dir.clone();
                        async move {
                            let mut client = client.lock();
                            match std::fs::read(&path) {
                                Ok(bytes) => client
                                    .upload(
                                        &format!("{dir}/{key}"),
                                        &bytes,
                                        sirv::content_type(&key),
                                    )
                                    .map_err(|error| format!("{key}: {error}")),
                                Err(error) => Err(format!("{key}: {error}")),
                            }
                        }
                    })
                    .await;
                if let Err(message) = outcome {
                    failures.push(message);
                }
                this.update(cx, |audit, cx| {
                    // Only onto this loop's own job. A slow last file can land after
                    // the user has already started another transfer.
                    if let Some(job) = audit.sirv_job.as_mut()
                        && job.generation == generation
                    {
                        job.done = ix + 1;
                        job.failures = failures.clone();
                        cx.notify();
                    }
                })
                .ok();
            }
            this.update(cx, |audit, cx| {
                if let Some(job) = audit.sirv_job.as_mut()
                    && job.generation == generation
                {
                    job.finished = true;
                }
                // Re-list the pair: pushed files must stop reading as new.
                audit.walk_sirv_pairing(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Count push / differs / pull across the whole dataset, not just the
    /// visible rows, so the header numbers do not move with the filter.
    fn refresh_sirv_counts(&mut self) {
        self.sirv_counts = match self.sirv_pairing.as_ref().map(|pairing| &pairing.files) {
            None | Some(Listing::Walking) | Some(Listing::Failed(_)) => None,
            Some(Listing::Ready(files)) => {
                let mut to_push = 0;
                let mut changed = 0;
                let mut local_keys = HashSet::new();
                for entry in &self.entries {
                    let Some(key) = sirv::relative_key(&self.root, &entry.path) else {
                        continue;
                    };
                    local_keys.insert(key.clone());
                    match sirv::classify(entry.bytes, files.get(&key)) {
                        sirv::SyncState::OnlyLocal => to_push += 1,
                        sirv::SyncState::Changed => changed += 1,
                        sirv::SyncState::Same => {}
                    }
                }
                let to_pull = files
                    .keys()
                    .filter(|key| !local_keys.contains(*key))
                    .count();
                Some((to_push, changed, to_pull))
            }
        };
    }

    /// Open the side-by-side view for a row and start building both sides.
    fn open_compare(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(path) = self.entries.get(index).map(|entry| entry.path.clone()) else {
            return;
        };
        let dataset_generation = self.dataset_generation;
        let key = compare::Key::new(&path, self.format, self.quality, self.max_edge);
        self.compare = Some(Comparison {
            index,
            dataset_generation,
            key: key.clone(),
            pair: None,
            failed: false,
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
            // Building a pair is a full decode, encode and second decode. Arrowing
            // through a folder used to start one per keypress and leave every one of
            // them running; wait for the arrow key to stop first.
            cx.background_executor().timer(COMPARE_DELAY).await;
            let still_open = this
                .read_with(cx, |audit, _| {
                    audit
                        .compare
                        .as_ref()
                        .is_some_and(|open| open.index == index && open.key == key)
                })
                .unwrap_or(false);
            if !still_open {
                return;
            }

            let built = cx
                .background_executor()
                .spawn(async move { compare::build(&path, format, quality, max_edge) })
                .await
                .map(Arc::new);

            let _ = this.update(cx, |audit, cx| {
                if let Some(pair) = built.as_ref() {
                    audit.cached = Some((key.clone(), pair.clone()));
                }
                // Ignore a result the user already navigated away from.
                if let Some(comparison) = audit.compare.as_mut()
                    && comparison.index == index
                    && comparison.dataset_generation == dataset_generation
                    && comparison.key == key
                {
                    comparison.failed = built.is_none();
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

        if comparison.failed {
            stage = stage.child(
                div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        Alert::error(
                            "compare-error",
                            "Could not build a comparison preview for this image.",
                        )
                        .max_w(px(420.)),
                    ),
            );
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
                        Button::new("compare-close")
                            .small()
                            .icon(IconName::Close)
                            .tooltip("Back to the audit")
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
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .bg(rgba(0x000000bf))
                    .text_size(px(12.))
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
                                (None, _) if comparison.failed => "Preview unavailable".to_string(),
                                _ => "decoding…".to_string(),
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .flex_shrink_0()
                            .child(
                                Button::new("compare-fit")
                                    .ghost()
                                    .small()
                                    .label("Fit")
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        if let Some(comparison) = audit.compare.as_mut() {
                                            comparison.zoom = None;
                                            comparison.pan = (0., 0.);
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                Button::new("compare-actual")
                                    .ghost()
                                    .small()
                                    .label("100%")
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        if let Some(comparison) = audit.compare.as_mut() {
                                            comparison.zoom = Some(1.);
                                            comparison.pan = (0., 0.);
                                            cx.notify();
                                        }
                                    })),
                            )
                            .child(
                                Button::new("compare-prev")
                                    .ghost()
                                    .small()
                                    .icon(IconName::ArrowLeft)
                                    .label("Prev")
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        audit.step_compare(-1, cx);
                                    })),
                            )
                            .child(
                                Button::new("compare-next")
                                    .ghost()
                                    .small()
                                    .icon(IconName::ArrowRight)
                                    .label("Next")
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        audit.step_compare(1, cx);
                                    })),
                            )
                            .child(
                                div()
                                    .ml_1()
                                    .text_color(rgba(0xffffffcc))
                                    .whitespace_nowrap()
                                    .child("Scroll to zoom · drag to pan"),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// One labelled row of the settings form.
    fn settings_row(
        label: &'static str,
        input: gpui::Entity<InputState>,
        secret: bool,
        cx: &Context<Self>,
    ) -> gpui::Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .w(px(110.))
                    .flex_shrink_0()
                    .text_size(px(12.))
                    .text_color(cx.theme().muted_foreground)
                    .child(label),
            )
            .child(
                Input::new(&input)
                    .small()
                    .when(secret, |field| field.mask_toggle()),
            )
    }

    /// A section heading plus its status line, if one has anything to say.
    fn settings_status(status: Option<(bool, String)>, cx: &Context<Self>) -> gpui::Div {
        match status {
            None => div(),
            Some((ok, message)) => div()
                .text_size(px(11.))
                .text_color(if ok { cx.theme().green } else { cx.theme().red })
                .child(message),
        }
    }

    /// The settings panel: the CDN keys.
    fn settings_panel_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let Some(panel) = self.settings_panel.as_ref() else {
            return div().into_any_element();
        };

        div()
            .w(px(480.))
            .flex()
            .flex_col()
            .gap_3()
            .p_5()
            .rounded_lg()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .font_family("SF Pro Display")
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child("Sirv account"),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .child("Credentials stay on this computer."),
                    ),
            )
            .child(Self::settings_row(
                "Client ID",
                panel.client_id.clone(),
                false,
                cx,
            ))
            .child(Self::settings_row(
                "Client secret",
                panel.client_secret.clone(),
                true,
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(Self::settings_status(panel.cdn_status.clone(), cx))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                Button::new("settings-close")
                                    .ghost()
                                    .small()
                                    .label("Close")
                                    .on_click(cx.listener(|audit, _, _, cx| {
                                        audit.settings_panel = None;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("settings-save")
                                    .primary()
                                    .small()
                                    .label("Save credentials")
                                    .on_click(
                                        cx.listener(|audit, _, _, cx| audit.save_sirv_settings(cx)),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// The remote-folder browser: a small panel over the window. Walk folders
    /// down, pair the folder you land on, or undo a pairing.
    fn sirv_browser_view(&self, browser: &SirvBrowser, cx: &mut Context<Self>) -> gpui::AnyElement {
        let paired = self
            .sirv_pairing
            .as_ref()
            .map(|pairing| pairing.dir.clone());

        let body: gpui::AnyElement = match browser.nodes.as_ref() {
            None => div()
                .flex()
                .items_center()
                .gap_2()
                .text_size(px(12.))
                .text_color(cx.theme().muted_foreground)
                .child(IconName::LoaderCircle)
                .child(format!("Listing {}…", browser.path))
                .into_any_element(),
            Some(Err(message)) => div()
                .text_size(px(12.))
                .text_color(cx.theme().yellow)
                .child(message.clone())
                .into_any_element(),
            Some(Ok(nodes)) => {
                let mut rows: Vec<gpui::AnyElement> = Vec::new();
                if browser.path != "/" {
                    rows.push(
                        Button::new("sirv-up")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowUp)
                            .label("..")
                            .on_click(cx.listener(|audit, _, _, cx| audit.ascend_sirv(cx)))
                            .into_any_element(),
                    );
                }
                for (ix, node) in nodes.iter().filter(|node| node.is_folder()).enumerate() {
                    let name = node
                        .filename
                        .rsplit('/')
                        .next()
                        .unwrap_or(&node.filename)
                        .to_string();
                    let descend_to = name.clone();
                    rows.push(
                        Button::new(("sirv-dir", ix))
                            .ghost()
                            .small()
                            .icon(IconName::FolderOpen)
                            .label(name)
                            .on_click(cx.listener(move |audit, _, _, cx| {
                                audit.descend_sirv(descend_to.clone(), cx);
                            }))
                            .into_any_element(),
                    );
                }
                if rows.is_empty() {
                    rows.push(
                        div()
                            .text_size(px(12.))
                            .text_color(cx.theme().muted_foreground)
                            .child("No subfolders.")
                            .into_any_element(),
                    );
                }
                div()
                    .id("sirv-list")
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap_0p5()
                    .max_h(px(280.))
                    .overflow_y_scroll()
                    .children(rows)
                    .into_any_element()
            }
        };

        div()
            .w(px(440.))
            .flex()
            .flex_col()
            .gap_3()
            .p_4()
            .rounded_lg()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .font_family("SF Pro Display")
                            .text_size(px(15.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().foreground)
                            .child("Sync with Sirv"),
                    )
                    .child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(browser.path.clone()),
                    ),
            )
            .child(body)
            .when_some(self.sirv_job.as_ref(), |panel, job| {
                panel.child(
                    div()
                        .text_size(px(11.))
                        .font_family(cx.theme().mono_font_family.clone())
                        .text_color(if job.failures.is_empty() {
                            cx.theme().muted_foreground
                        } else {
                            cx.theme().yellow
                        })
                        .child(match (job.finished, job.kind) {
                            (false, SirvJobKind::Pull) => {
                                format!("Pulling {} of {}…", job.done, job.total)
                            }
                            (false, SirvJobKind::Push) => {
                                format!("Pushing {} of {}…", job.done, job.total)
                            }
                            (true, kind) => {
                                let verb = if kind == SirvJobKind::Pull {
                                    "Pulled"
                                } else {
                                    "Pushed"
                                };
                                let failures = if job.failures.is_empty() {
                                    String::new()
                                } else {
                                    format!(
                                        ", {} failed: {}",
                                        job.failures.len(),
                                        job.failures.join(", ")
                                    )
                                };
                                format!("{verb} {} of {}{failures}", job.done, job.total)
                            }
                        }),
                )
            })
            .child(div().flex().items_center().justify_between().child(
                div().flex().gap_2().when_some(paired, |row, dir| {
                    let busy = self.sirv_busy();
                    let (to_push, _, to_pull) = self.sirv_counts.unwrap_or((0, 0, 0));
                    row.child(
                        Button::new("sirv-pull")
                            .outline()
                            .small()
                            .icon(IconName::ArrowDown)
                            .label(format!("Pull {to_pull} missing"))
                            .disabled(busy || to_pull == 0)
                            .on_click(cx.listener(|audit, _, _, cx| audit.start_pull(cx))),
                    )
                    .child(
                        Button::new("sirv-push")
                            .outline()
                            .small()
                            .icon(IconName::ArrowUp)
                            .label(format!("Push {to_push} new"))
                            .disabled(busy || to_push == 0)
                            .on_click(cx.listener(|audit, _, _, cx| audit.start_push(cx))),
                    )
                    .child(
                        Button::new("sirv-unpair")
                            .ghost()
                            .small()
                            .label(format!("Unpair {dir}"))
                            .disabled(busy)
                            .on_click(cx.listener(|audit, _, _, cx| {
                                audit.unpair_sirv(cx);
                            })),
                    )
                }),
            ))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("sirv-close")
                            .ghost()
                            .small()
                            .label("Close")
                            .on_click(cx.listener(|audit, _, _, cx| {
                                audit.sirv_browser = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("sirv-pair")
                            .primary()
                            .small()
                            .label("Pair this folder")
                            .disabled(!matches!(browser.nodes, Some(Ok(_))))
                            .on_click(cx.listener(|audit, _, _, cx| audit.pair_sirv(cx))),
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
                segment(
                    gpui::SharedString::from(edge.label()),
                    edge.label(),
                    self.max_edge == *edge,
                )
                .disabled(self.converting)
            }))
            .on_click(cx.listener(move |audit, clicked: &Vec<usize>, _, cx| {
                if audit.converting {
                    return;
                }
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
                segment(
                    format.label(),
                    format.label().to_uppercase(),
                    self.format == *format,
                )
                .disabled(self.converting)
            }))
            .on_click(cx.listener(move |audit, clicked: &Vec<usize>, _, cx| {
                if audit.converting {
                    return;
                }
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
        self.visible_bytes
    }

    fn toolbar_button(
        &self,
        id: &'static str,
        text: &'static str,
        tooltip: &'static str,
        icon: IconName,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Self, &mut Context<Self>) + 'static,
    ) -> Button {
        Button::new(id)
            .small()
            .icon(icon)
            .label(text)
            .tooltip(tooltip)
            .on_click(cx.listener(move |audit, _, _, cx| on_click(audit, cx)))
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
        // Three states, three sentences. A pairing whose walk is still running used to
        // read exactly like one whose walk failed: no Sirv text at all.
        match (&self.sirv_pairing, self.sirv_counts) {
            (Some(_), Some((to_push, changed, to_pull))) => stats.push_str(&format!(
                " · Sirv: {to_push} to push · {changed} differ · {to_pull} to pull"
            )),
            (Some(pairing), None) => stats.push_str(match pairing.files {
                Listing::Walking => " · Sirv: listing…",
                Listing::Failed(_) => " · Sirv: listing failed",
                Listing::Ready(_) => "",
            }),
            (None, _) => {}
        }

        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            // Identity and its two actions share the top-left corner: the name,
            // then the openers that replace it. The path and the count sit
            // underneath as metadata.
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_w(px(220.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .font_family("SF Pro Display")
                                    .text_size(px(15.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(folder),
                            )
                            .child(
                                Button::new("open-folder")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Folder)
                                    .label("Open folder…")
                                    .disabled(self.converting)
                                    .on_click(cx.listener(|audit, _, _, cx| audit.pick(true, cx))),
                            )
                            .child(
                                Button::new("open-file")
                                    .small()
                                    .ghost()
                                    .icon(IconName::File)
                                    .label("Open image…")
                                    .disabled(self.converting)
                                    .on_click(cx.listener(|audit, _, _, cx| audit.pick(false, cx))),
                            )
                            .child(
                                // The sync entry point: opens the remote-folder
                                // browser, which is also where a pairing is undone.
                                Button::new("sirv-browser")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Globe)
                                    .label(match &self.sirv_pairing {
                                        Some(pairing) => pairing.dir.clone(),
                                        None => "Sirv…".into(),
                                    })
                                    .disabled(self.converting)
                                    .on_click(
                                        cx.listener(|audit, _, _, cx| audit.open_sirv_browser(cx)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_baseline()
                            .gap_2()
                            .min_w_0()
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .text_color(cx.theme().muted_foreground)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(self.root.display().to_string()),
                            )
                            .child(
                                div()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(px(11.))
                                    .text_color(cx.theme().muted_foreground)
                                    .whitespace_nowrap()
                                    .flex_shrink_0()
                                    .child(stats),
                            ),
                    ),
            )
            // The view toggle sits at the far end of the window: it changes how
            // the list below is drawn and nothing else.
            .child(
                self.toolbar_button(
                    "view-grid",
                    if self.grid { "List" } else { "Grid" },
                    if self.grid {
                        "Show the audit as a list"
                    } else {
                        "Show the images as a gallery"
                    },
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
                )
                .disabled(self.converting),
            )
            .child(
                // Icon-only: the one global surface, always in the same corner.
                Button::new("open-settings")
                    .small()
                    .ghost()
                    .icon(IconName::Settings)
                    .tooltip("Settings")
                    .on_click(cx.listener(|audit, _, window, cx| audit.open_settings(window, cx))),
            )
    }

    /// The three knobs that decide what a conversion produces, each under its own
    /// name and drawn as one control rather than a run of loose buttons.
    fn controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let lossless = self.quality == Quality::LOSSLESS;
        div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap_3()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            // What the list shows, then what a conversion would do: the reading
            // order of the strip follows the order you use it.
            .child(
                div().w(px(150.)).flex_shrink_0().child(
                    Input::new(&self.filter_input)
                        .small()
                        .cleanable(true)
                        .disabled(self.converting)
                        .prefix(IconName::Search),
                ),
            )
            // The audit colours every row by weight per pixel and then asks you to find
            // the heavy ones yourself. Counted here rather than cached: it is a
            // multiply and a compare per row, unlike the allocating `extension_lies`.
            .children({
                let heavy = self
                    .entries
                    .iter()
                    .filter(|entry| Finding::Heavy.holds(entry))
                    .count();
                (heavy > 0).then(|| {
                    self.finding_button(
                        Finding::Heavy,
                        IconName::TriangleAlert,
                        format!("{heavy} heavy"),
                        cx,
                    )
                })
            })
            .child(
                div()
                    .w(px(1.))
                    .h(px(24.))
                    .flex_shrink_0()
                    .bg(cx.theme().border),
            )
            .child(self.control_group("Resize", self.resize_group(cx), cx))
            .child(self.control_group("Format", self.format_group(cx), cx))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .flex_shrink_0()
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(cx.theme().muted_foreground)
                            .whitespace_nowrap()
                            .child("Quality"),
                    )
                    .child(
                        div()
                            .w(px(110.))
                            .debug_selector(|| "quality-control".to_string())
                            .when(self.converting, |rail| {
                                rail.child(
                                    Progress::new("quality-locked")
                                        .value(self.quality.0.unwrap_or(100.))
                                        .color(cx.theme().primary)
                                        .h(px(6.)),
                                )
                            })
                            .when(!self.converting, |slider| {
                                slider.child(Slider::new(&self.quality_slider).horizontal())
                            }),
                    )
                    .child(
                        div()
                            .w(px(26.))
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(12.))
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
                        Switch::new("lossless")
                            .checked(lossless)
                            .label("Lossless")
                            .disabled(self.converting)
                            .on_click(cx.listener(|audit, _, _, cx| {
                                if audit.converting {
                                    return;
                                }
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
        let target_count = self.target_count();
        // Source bytes only appear before a conversion. While results stream in,
        // avoid walking thousands of rows on every progress redraw.
        let source = if !self.converting && self.results.is_empty() {
            self.target_bytes()
        } else {
            0
        };

        // Four states, one shape: a headline, the share it leaves behind, and a
        // sentence of detail.
        let (headline, tone, detail, bar, tag) = if self.converting {
            let done = self.results.len() + self.failures.len();
            let total = self.active_target_count.unwrap_or(target_count);
            (
                format!("{done} of {total}"),
                cx.theme().foreground,
                format!(
                    "Converting to {} {}…",
                    self.format.label().to_uppercase(),
                    self.quality.label()
                ),
                Some((done as f32 / total.max(1) as f32, cx.theme().primary)),
                None,
            )
        } else if !self.results.is_empty() {
            let (before, after) = self.converted_totals();
            let growth = after > before;
            let delta = before.abs_diff(after);
            let percent = delta as f32 / before.max(1) as f32 * 100.;
            (
                format!(
                    "{} {}",
                    format_bytes(delta),
                    if growth { "larger" } else { "saved" }
                ),
                if growth {
                    cx.theme().yellow
                } else {
                    cx.theme().green
                },
                format!(
                    "{} converted · {} → {}",
                    self.results.len(),
                    format_bytes(before),
                    format_bytes(after)
                ),
                Some((
                    after as f32 / before.max(1) as f32,
                    if growth {
                        cx.theme().yellow
                    } else {
                        cx.theme().green
                    },
                )),
                Some((growth, percent)),
            )
        } else if let Some((projected, sampled)) = self.estimate {
            let growth = projected > source;
            let delta = source.abs_diff(projected);
            let percent = delta as f32 / source.max(1) as f32 * 100.;
            (
                // A projection from a few dozen encodes, said as one. Unqualified it
                // read as a measurement, and the reader had no way to tell it from the
                // completed total above, which is one.
                format!(
                    "≈{} to {}",
                    format_bytes(delta),
                    if growth { "grow" } else { "save" }
                ),
                if growth {
                    cx.theme().yellow
                } else {
                    cx.theme().green
                },
                format!(
                    "{} now → ≈{} as {} {} · sampled {sampled}",
                    format_bytes(source),
                    format_bytes(projected),
                    self.format.label().to_uppercase(),
                    self.quality.label()
                ),
                Some((
                    projected as f32 / source.max(1) as f32,
                    if growth {
                        cx.theme().yellow
                    } else {
                        cx.theme().green
                    },
                )),
                Some((growth, percent)),
            )
        } else {
            (
                "Sizing it up…".to_string(),
                cx.theme().muted_foreground,
                format!("{} on disk", format_bytes(source)),
                None,
                None,
            )
        };

        // A status bar: fixed at the bottom, one height in every state, so the
        // list above it never jumps when the numbers arrive.
        let (fraction, colour) = bar
            .map(|(remaining, colour)| (1. - remaining, colour))
            .unwrap_or((0., gpui::transparent_black()));

        div()
            .flex()
            .flex_col()
            .px_3()
            .pt_1()
            .pb_2()
            // The one strip allowed colour: washed in the tone of the headline,
            // so the state of the job reads before any word does.
            .bg(tone.opacity(0.08))
            .border_t_1()
            .border_color(cx.theme().border)
            .child(meter("saving", fraction, colour, 3.))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .font_family("SF Pro Display")
                            .text_size(px(18.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(tone)
                            .whitespace_nowrap()
                            .flex_shrink_0()
                            .child(headline),
                    )
                    // The share saved, which is the number people actually quote.
                    .children(tag.map(|(growth, percent)| {
                        let tag = if growth {
                            Tag::warning()
                        } else {
                            Tag::success()
                        };
                        tag.small().child(if growth {
                            format!("+{percent:.0}%")
                        } else {
                            format!("−{percent:.0}%")
                        })
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .font_family(cx.theme().mono_font_family.clone())
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
                            .when(self.converting || target_count == 0, |button| {
                                button.ghost()
                            })
                            .label(if self.converting {
                                "Converting…".to_string()
                            } else if self.selected.is_empty() {
                                format!("Convert all to {}", self.format.label().to_uppercase())
                            } else {
                                format!(
                                    "Convert {} to {}",
                                    target_count,
                                    self.format.label().to_uppercase()
                                )
                            })
                            .disabled(
                                self.converting || self.scanning.is_some() || target_count == 0,
                            )
                            .on_click(cx.listener(|audit, _, _, cx| audit.start_conversion(cx))),
                    ),
            )
    }

    /// Everything the scan could not take at face value, in one line rather than
    /// three scattered ones. The mislabelled count is a button: it is the audit's best
    /// finding, and a number you cannot act on is a dead end.
    fn notices(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let mut parts = Vec::new();
        if !self.unreadable.is_empty() {
            parts.push(format!(
                "would not decode: {}",
                named(self.unreadable.iter().filter_map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                }))
            ));
        }
        if self.existing_output > 0 {
            parts.push(match self.existing_output {
                1 => format!("{}/ already holds 1 file", scan::OUTPUT_DIR),
                many => format!("{}/ already holds {many} files", scan::OUTPUT_DIR),
            });
        }
        if !self.failures.is_empty() {
            parts.push(format!("failed: {}", named(self.failures.iter().cloned())));
        }
        if let Some(pairing) = &self.sirv_pairing
            && let Listing::Failed(reason) = &pairing.files
        {
            parts.push(format!("could not list {}: {reason}", pairing.dir));
        }
        if let Some(job) = &self.sirv_job {
            let verb = match job.kind {
                SirvJobKind::Pull => "Sirv pull",
                SirvJobKind::Push => "Sirv push",
            };
            let failures = if job.failures.is_empty() {
                String::new()
            } else {
                format!(
                    ", {} failed: {}",
                    job.failures.len(),
                    job.failures
                        .iter()
                        .take(3)
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            parts.push(format!("{verb}: {} of {}{failures}", job.done, job.total));
        }
        if parts.is_empty() && self.mislabelled == 0 {
            return None;
        }

        // Left-aligned and only as wide as its text. A full-bleed box for six words
        // was a bigger shape on screen than the finding it was reporting.
        Some(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap_2()
                .children((self.mislabelled > 0).then(|| {
                    self.finding_button(
                        Finding::Mislabelled,
                        IconName::TriangleAlert,
                        match self.mislabelled {
                            1 => "1 file is not the format its extension claims".to_string(),
                            many => {
                                format!("{many} files are not the format their extension claims")
                            }
                        },
                        cx,
                    )
                }))
                .children((!parts.is_empty()).then(|| {
                    Alert::warning("notices", parts.join("  ·  "))
                        .icon(IconName::TriangleAlert)
                        .py_1()
                }))
                .into_any_element(),
        )
    }

    /// A finding shown as the control that narrows the list to it. Lit while it is the
    /// one in force, so the count and the list below it never disagree.
    fn finding_button(
        &self,
        finding: Finding,
        icon: IconName,
        label: String,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let active = self.finding == Some(finding);
        Button::new(("finding", finding as usize))
            .small()
            .icon(icon)
            .label(label)
            .selected(active)
            .when(!active, |button| button.ghost())
            .when(active, |button| button.warning())
            .on_click(cx.listener(move |audit, _, _, cx| audit.set_finding(finding, cx)))
    }

    /// Kick off decoding for a row, unless it is already loaded or in flight.
    fn request_thumb(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.thumbs.contains_key(&index) || !self.requested.insert(index) {
            return;
        }
        let dataset_generation = self.dataset_generation;
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
                    if audit.dataset_generation == dataset_generation {
                        audit.thumbs.insert(index, image);
                        audit.thumb_order.push_back(index);
                        audit.trim_thumbs();
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// How one file stands against the paired Sirv folder, as the word and colour the
    /// window says it in. `None` when there is no pairing or its listing is not ready:
    /// the state exists only when it can be known.
    ///
    /// One place for it, so the table and the gallery cannot drift into two
    /// vocabularies for one fact — the gallery had none at all, which is the widest
    /// two vocabularies can drift.
    fn sync_label(&self, entry: &Entry, cx: &App) -> Option<(&'static str, gpui::Hsla)> {
        let Listing::Ready(files) = &self.sirv_pairing.as_ref()?.files else {
            return None;
        };
        let key = sirv::relative_key(&self.root, &entry.path)?;
        Some(match sirv::classify(entry.bytes, files.get(&key)) {
            sirv::SyncState::Same => ("synced", cx.theme().muted_foreground),
            sirv::SyncState::Changed => ("changed", cx.theme().yellow),
            sirv::SyncState::OnlyLocal => ("new", cx.theme().blue),
        })
    }

    /// Drop the oldest thumbnails once the cache is over its bound. `requested` has to
    /// forget them too, or scrolling back to a dropped row would show a permanent gap.
    fn trim_thumbs(&mut self) {
        while self.thumb_order.len() > THUMB_CACHE {
            let Some(oldest) = self.thumb_order.pop_front() else {
                return;
            };
            self.thumbs.remove(&oldest);
            self.requested.remove(&oldest);
        }
    }
}

/// What the audit found, as something the list can be narrowed to.
///
/// The window used to state these as numbers and stop there. A folder of 5,739 images
/// saying "5 files are not the format their extension claims" is a finding you cannot
/// reach: the whole point of an audit is to end up looking at those five.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Finding {
    /// The extension disagrees with the bytes inside the file.
    Mislabelled,
    /// More bytes per pixel than a photograph needs. These are the files a conversion
    /// is actually for.
    Heavy,
}

impl Finding {
    fn holds(self, entry: &Entry) -> bool {
        match self {
            Finding::Mislabelled => entry.extension_lies(),
            Finding::Heavy => entry.bytes_per_pixel() > DENSITY_HEAVY,
        }
    }
}

/// A few names and then a count, rather than a count alone. Used wherever the window
/// reports a set of files it could not handle.
fn named(names: impl Iterator<Item = String>) -> String {
    let all: Vec<String> = names.collect();
    let shown = all.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    match all.len().saturating_sub(3) {
        0 => shown,
        rest => format!("{shown} and {rest} more"),
    }
}

/// One sampled file and the slice of the list it speaks for.
struct Stratum {
    path: PathBuf,
    /// The sampled file's own size on disk.
    bytes: u64,
    /// Every file in its slice, that one included.
    slice_bytes: u64,
}

/// Project the encoded size of a whole list from a few real encodes.
///
/// Each entry is one slice's bytes and, when its sample encoded, that sample's own
/// source and encoded size. A slice is scaled by its own sample; a slice whose sample
/// would not decode is scaled by the average of the ones that did. Returns the total
/// and how many samples stood behind it, or `None` when nothing encoded at all.
///
/// The old version divided the summed sample bytes by the summed source bytes and
/// applied that one ratio to the folder. On a weight-sorted list of 5,739 photos the
/// heaviest file was 109MB of a 110MB sample, so its 300:1 compression became the
/// forecast for all 3GB and the window promised "3.0 GB to save, −100%".
fn project_total(slices: &[(u64, Option<(u64, u64)>)]) -> Option<(u64, usize)> {
    let ratio = |(source, encoded): (u64, u64)| encoded as f64 / source.max(1) as f64;
    let sampled: Vec<f64> = slices
        .iter()
        .filter_map(|(_, sample)| sample.map(ratio))
        .collect();
    if sampled.is_empty() {
        return None;
    }

    let average = sampled.iter().sum::<f64>() / sampled.len() as f64;
    let projected: f64 = slices
        .iter()
        .map(|(slice_bytes, sample)| *slice_bytes as f64 * sample.map_or(average, ratio))
        .sum();
    Some((projected as u64, sampled.len()))
}

fn conversion_targets(visible: &[usize], selected: &HashSet<usize>) -> Vec<usize> {
    if selected.is_empty() {
        visible.to_vec()
    } else {
        visible
            .iter()
            .copied()
            .filter(|index| selected.contains(index))
            .collect()
    }
}

fn progress_batch_ready(completed: usize, workers: usize, work_remaining: bool) -> bool {
    completed >= workers || !work_remaining
}

/// The audit list, as the component library's virtualised table.
///
/// It was a `uniform_list` with the header, the column widths, the sort arrows and
/// the hit testing all written by hand and kept in step by hand. The delegate hands
/// all of that to the library, which is also where column resizing and dragging come
/// from for free.
struct AuditTable {
    /// Weak, because the audit owns the table state, which owns this.
    audit: gpui::WeakEntity<Audit>,
    columns: Vec<TableColumn>,
    /// Width for the name column, recomputed from the window so the fixed columns
    /// do not leave an empty strip on the right. Columns here take a width, not a
    /// share, so somebody has to do the arithmetic.
    name_width: f32,
    compact: bool,
    show_result: bool,
}

/// The columns, in display order. `Column` is what the audit sorts by; this adds the
/// ones that carry no sortable value of their own.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TableColumn {
    Tick,
    Thumb,
    Name,
    Format,
    Pixels,
    Density,
    Weight,
    Sync,
    Result,
}

impl TableColumn {
    /// The audit column this sorts by, if it sorts.
    fn sorts_by(&self) -> Option<Column> {
        match self {
            TableColumn::Name => Some(Column::Name),
            TableColumn::Format => Some(Column::Format),
            TableColumn::Pixels => Some(Column::Pixels),
            TableColumn::Density => Some(Column::Density),
            TableColumn::Weight => Some(Column::Weight),
            _ => None,
        }
    }

    fn spec(&self, name_width: f32, compact: bool) -> TableCol {
        match self {
            TableColumn::Tick => TableCol::new("tick", "").width(px(W_TICK)),
            TableColumn::Thumb => TableCol::new("thumb", "").width(px(THUMB_SLOT + 12.)).p_0(),
            // Name takes whatever the other columns leave, so the window has no dead
            // strip down its right-hand side.
            TableColumn::Name => TableCol::new("name", "Name")
                .width(px(name_width))
                .min_width(px(W_NAME_MIN))
                .sortable()
                .resizable(true),
            TableColumn::Format => TableCol::new("format", "Format")
                .width(px(if compact { W_FORMAT_COMPACT } else { W_FORMAT }))
                .sortable(),
            TableColumn::Pixels => TableCol::new("pixels", "Size")
                .width(px(if compact { W_PIXELS_COMPACT } else { W_PIXELS }))
                .text_right()
                .sortable(),
            TableColumn::Density => TableCol::new("density", "B/px")
                .width(px(if compact {
                    W_DENSITY_COMPACT
                } else {
                    W_DENSITY
                }))
                .text_right()
                .sortable(),
            TableColumn::Weight => TableCol::new("weight", "Weight")
                .width(px(if compact { W_WEIGHT_COMPACT } else { W_WEIGHT }))
                .text_right()
                .sortable(),
            TableColumn::Sync => TableCol::new("sirv", "Sirv").width(px(W_SYNC)),
            TableColumn::Result => TableCol::new("result", "Result")
                .width(px(W_RESULT))
                .text_right(),
        }
    }
}

impl AuditTable {
    /// Chrome the table spends on gaps, cell padding and its own border.
    const CHROME: f32 = 30.;

    fn fixed_width(compact: bool, show_result: bool) -> f32 {
        W_TICK
            + THUMB_SLOT
            + 12.
            + if compact { W_FORMAT_COMPACT } else { W_FORMAT }
            + if compact { W_PIXELS_COMPACT } else { W_PIXELS }
            + if compact {
                W_DENSITY_COMPACT
            } else {
                W_DENSITY
            }
            + if compact { W_WEIGHT_COMPACT } else { W_WEIGHT }
            + if compact { 0. } else { W_SYNC }
            + if show_result { W_RESULT } else { 0. }
    }

    fn layout(width: f32, show_result: bool) -> (bool, f32, Vec<TableColumn>) {
        let compact = width < 900.;
        let name_width =
            (width - Self::fixed_width(compact, show_result) - Self::CHROME).max(W_NAME_MIN);
        let columns = if compact {
            vec![
                TableColumn::Tick,
                TableColumn::Thumb,
                TableColumn::Name,
                TableColumn::Format,
                TableColumn::Pixels,
                TableColumn::Density,
                TableColumn::Weight,
                TableColumn::Result,
            ]
        } else {
            vec![
                TableColumn::Tick,
                TableColumn::Thumb,
                TableColumn::Name,
                TableColumn::Format,
                TableColumn::Pixels,
                TableColumn::Density,
                TableColumn::Weight,
                TableColumn::Sync,
                TableColumn::Result,
            ]
        };
        (compact, name_width, columns)
    }

    fn new(audit: gpui::WeakEntity<Audit>, window: &Window) -> Self {
        let mut table = Self {
            audit,
            name_width: W_NAME_MIN,
            compact: false,
            show_result: false,
            columns: Vec::new(),
        };
        table.set_viewport_width(f32::from(window.viewport_size().width), false);
        table
    }

    fn set_viewport_width(&mut self, width: f32, show_result: bool) {
        self.show_result = show_result;
        (self.compact, self.name_width, self.columns) = Self::layout(width, show_result);
    }
}

impl TableDelegate for AuditTable {
    fn columns_count(&self, _cx: &App) -> usize {
        // The result column only exists once there is something to put in it.
        // Reserving its width up front left a fifth of the window empty in the
        // common case.
        if self.show_result {
            self.columns.len()
        } else {
            self.columns.len() - 1
        }
    }

    fn rows_count(&self, cx: &App) -> usize {
        self.audit
            .upgrade()
            .map_or(0, |audit| audit.read(cx).visible.len())
    }

    fn column(&self, col_ix: usize, cx: &App) -> TableCol {
        let Some(column) = self.columns.get(col_ix) else {
            return TableCol::new("none", "");
        };
        let mut spec = column.spec(self.name_width, self.compact);
        // Show the arrow on whichever column the audit is actually ordered by.
        if let Some(sort) = column.sorts_by()
            && let Some(audit) = self.audit.upgrade()
        {
            let audit = audit.read(cx);
            if audit.sort.column == sort {
                spec = if audit.sort.descending {
                    spec.descending()
                } else {
                    spec.ascending()
                };
            }
        }
        spec
    }

    fn perform_sort(
        &mut self,
        col_ix: usize,
        _sort: ColumnSort,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) {
        let Some(column) = self.columns.get(col_ix).and_then(TableColumn::sorts_by) else {
            return;
        };
        let Some(audit) = self.audit.upgrade() else {
            return;
        };
        audit.update(cx, |audit, cx| audit.set_sort(column, cx));
    }

    fn render_tr(
        &mut self,
        row_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> gpui::Stateful<gpui::Div> {
        let row = div().id(("row", row_ix));
        let Some(audit) = self.audit.upgrade() else {
            return row;
        };
        let audit_state = audit.read(cx);
        let ticked = audit_state
            .entry_at(row_ix)
            .is_some_and(|entry| audit_state.selected.contains(&entry));
        let cursor = audit_state.cursor;

        // The audit's finding, carried on the row's left edge: a tick in the
        // density band colour, so the shape of the folder is visible while
        // scrolling and not only in the B/px column.
        let rail = audit_state
            .entry_at(row_ix)
            .and_then(|index| audit_state.entries.get(index))
            .map(|entry| density_colour(entry.bytes_per_pixel(), cx));
        row.h(px(ROW_HEIGHT))
            .relative()
            .border_1()
            .border_color(gpui::transparent_black())
            .when(ticked, |row| row.bg(cx.theme().list_active))
            .when(row_ix == cursor, |row| row.border_color(cx.theme().ring))
            .children(rail.map(|colour| {
                div()
                    .absolute()
                    .left_0()
                    .top(px(5.))
                    .bottom(px(5.))
                    .w(px(2.))
                    .rounded_full()
                    .bg(colour.opacity(0.9))
            }))
            .on_click(cx.listener(move |table, event: &gpui::ClickEvent, _, cx| {
                let Some(audit) = table.delegate().audit.upgrade() else {
                    return;
                };
                audit.update(cx, |audit, cx| audit.click_row(row_ix, event, cx));
            }))
    }

    fn render_td(
        &mut self,
        row_ix: usize,
        col_ix: usize,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        let Some(column) = self.columns.get(col_ix).copied() else {
            return div().into_any_element();
        };
        let Some(handle) = self.audit.upgrade() else {
            return div().into_any_element();
        };

        // The row the viewport asked for is the row worth decoding.
        handle.update(cx, |audit, cx| {
            if let Some(entry) = audit.entry_at(row_ix) {
                audit.request_thumb(entry, cx);
            }
        });

        let audit = handle.read(cx);
        let Some(index) = audit.entry_at(row_ix) else {
            return div().into_any_element();
        };
        let Some(entry) = audit.entries.get(index) else {
            return div().into_any_element();
        };

        match column {
            TableColumn::Tick => {
                let ticked = audit.selected.contains(&index);
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .debug_selector(move || format!("table-checkbox-{index}"))
                    .on_key_down(cx.listener(|_, event, _, cx| {
                        if is_checkbox_activation_key(event) {
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        Checkbox::new(("tick", index))
                            .checked(ticked)
                            .on_click(cx.listener(move |table, _: &bool, _, cx| {
                                cx.stop_propagation();
                                let Some(audit) = table.delegate().audit.upgrade() else {
                                    return;
                                };
                                audit.update(cx, |audit, cx| {
                                    if audit.converting {
                                        return;
                                    }
                                    if !audit.selected.remove(&index) {
                                        audit.selected.insert(index);
                                    }
                                    audit.schedule_estimate(cx);
                                    cx.notify();
                                });
                            })),
                    )
                    .into_any_element()
            }
            TableColumn::Thumb => div()
                .w(px(THUMB_SLOT))
                .h(px(THUMB_SLOT))
                .flex()
                .items_center()
                .justify_center()
                .rounded_sm()
                .bg(cx.theme().background)
                // A fixed slot, so rows do not jump as thumbnails arrive.
                .when_some(audit.thumbs.get(&index).cloned(), |slot, image| {
                    slot.child(img(image).max_w(px(THUMB_SLOT)).max_h(px(THUMB_SLOT)))
                })
                .into_any_element(),
            TableColumn::Name => div()
                .w_full()
                .overflow_hidden()
                .text_ellipsis()
                .whitespace_nowrap()
                .text_color(cx.theme().foreground)
                .child(entry.name())
                .into_any_element(),
            TableColumn::Format => {
                let lies = entry.extension_lies();
                div()
                    .flex()
                    .items_center()
                    .gap_1()
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
                    .when(lies, |cell| cell.child(div().text_size(px(11.)).child("≠")))
                    .into_any_element()
            }
            TableColumn::Pixels => div()
                .w_full()
                .flex()
                .justify_end()
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(px(12.))
                .text_color(cx.theme().muted_foreground)
                .whitespace_nowrap()
                .child(format!("{}×{}", entry.width, entry.height))
                .into_any_element(),
            TableColumn::Density => {
                let density = entry.bytes_per_pixel();
                div()
                    .w_full()
                    .flex()
                    .justify_end()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(12.))
                    .font_weight(FontWeight::MEDIUM)
                    .whitespace_nowrap()
                    .text_color(density_colour(density, cx))
                    .child(format!("{density:.2}"))
                    .into_any_element()
            }
            // The bar lives under its own number now: one cell, so a bar can
            // never drift away from the figure it measures.
            TableColumn::Weight => {
                let fraction = entry.bytes as f32 / audit.heaviest.max(1) as f32;
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .items_end()
                    .justify_center()
                    .gap_1()
                    .child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .whitespace_nowrap()
                            .text_color(cx.theme().foreground)
                            .child(format_bytes(entry.bytes)),
                    )
                    .child(div().w_full().child(meter(
                        ("weight", index),
                        fraction,
                        cx.theme().primary,
                        3.,
                    )))
                    .into_any_element()
            }
            TableColumn::Sync => {
                // The row's file against the paired Sirv folder. No pairing,
                // no status: the column exists only when it can know.
                let Some((label, colour)) = audit.sync_label(entry, cx) else {
                    return div().into_any_element();
                };
                div()
                    .flex()
                    .items_center()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(px(11.))
                    .text_color(colour)
                    .child(label)
                    .into_any_element()
            }
            TableColumn::Result => div()
                .flex()
                .items_center()
                .justify_end()
                .gap_2()
                .whitespace_nowrap()
                .when_some(audit.results.get(&index), |slot, converted| {
                    let saved = entry.bytes.saturating_sub(*converted);
                    let percent = if entry.bytes == 0 {
                        0.
                    } else {
                        saved as f32 / entry.bytes as f32 * 100.
                    };
                    // A file that grew is a real outcome, not a rounding error:
                    // re-encoding an already-optimal JPEG usually costs bytes.
                    let grew = *converted > entry.bytes;
                    slot.child(
                        div()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(12.))
                            .text_color(cx.theme().muted_foreground)
                            .child(format_bytes(*converted)),
                    )
                    .child(if grew {
                        Tag::warning().small().child("larger")
                    } else {
                        Tag::success().small().child(format!("−{percent:.0}%"))
                    })
                })
                .into_any_element(),
        }
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<TableState<Self>>,
    ) -> impl IntoElement {
        div()
            .p_4()
            .w_full()
            .flex()
            .justify_center()
            .text_size(px(12.))
            .text_color(cx.theme().muted_foreground)
            .child("Nothing matches that filter")
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

        // Cheap enough to compare every frame, and it means a crash still leaves the
        // last good size and folder on disk. The write itself is delayed.
        let viewport = window.viewport_size();
        let current = settings::Settings {
            width: Some(f32::from(viewport.width)),
            height: Some(f32::from(viewport.height)),
            folder: self.root.is_dir().then(|| self.root.clone()),
        };
        if current != self.settings {
            self.remember_settings(current, cx);
        }

        if let Some(table) = self.table.clone() {
            let width = f32::from(viewport.width);
            let show_result = !self.results.is_empty();
            let signature = (width.round().max(0.) as u32, show_result);
            if self.table_signature != Some(signature) {
                self.table_signature = Some(signature);
                cx.defer(move |cx| {
                    table.update(cx, |table, cx| {
                        table.delegate_mut().set_viewport_width(width, show_result);
                        table.refresh(cx);
                    });
                });
            }
        }

        if let Some(scanning) = self.scanning.as_ref() {
            let label = scanning.clone();
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .bg(cx.theme().background)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_2()
                        .w(px(420.))
                        .px_4()
                        .py_4()
                        .rounded_lg()
                        .bg(cx.theme().secondary)
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .font_family("SF Pro Display")
                                .text_size(px(18.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child(format!("Scanning {label}…")),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(cx.theme().muted_foreground)
                                .child(
                                    "The current folder stays untouched until the scan finishes.",
                                ),
                        ),
                )
                .on_drop(cx.listener(|audit, paths: &gpui::ExternalPaths, _, cx| {
                    if let Some(path) = paths.paths().first() {
                        audit.request_path(path.clone(), cx);
                    }
                }))
                .into_any_element();
        }

        if self.entries.is_empty() {
            let empty_folder = self.root.is_dir();
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .bg(cx.theme().background)
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
                        .py_6()
                        .border_dashed()
                        .border_1()
                        .border_color(if self.drag_over {
                            cx.theme().drag_border
                        } else {
                            cx.theme().border
                        })
                        .child(
                            div()
                                .font_family("SF Pro Display")
                                .text_size(px(19.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child(if empty_folder {
                                    "No supported images found"
                                } else {
                                    "Audit a folder of images"
                                }),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(cx.theme().muted_foreground)
                                .text_center()
                                .child(if empty_folder {
                                    "This folder has no supported images. Choose another folder \
                                     or drop an image here."
                                } else {
                                    "Nothing is uploaded. Every file is read, resized and \
                                     re-encoded on this machine."
                                }),
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
                                .text_size(px(12.))
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
                        audit.request_path(path.clone(), cx);
                    }
                }))
                .into_any_element();
        }

        if self.settings_panel.is_some() {
            let view = self.settings_panel_view(cx);
            // The click that opened the panel left focus on the button it
            // replaced; take focus next frame so typing lands in the first
            // field. Once only: after that the field with focus is whichever
            // one Tab or a click chose. Nothing else in the framework moves Tab
            // between inputs, so this panel cycles them itself.
            cx.defer_in(window, |audit, window, cx| {
                if let Some(panel) = audit.settings_panel.as_mut()
                    && !panel.focused
                {
                    panel.focused = true;
                    let handle = panel.client_id.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }
            });
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(cx.theme().background)
                .on_key_down(
                    cx.listener(|audit, event: &gpui::KeyDownEvent, window, cx| {
                        match event.keystroke.key.as_str() {
                            "escape" => {
                                audit.settings_panel = None;
                                cx.notify();
                            }
                            "tab" => {
                                const FIELDS: usize = 2;
                                let direction = if event.keystroke.modifiers.shift {
                                    FIELDS - 1
                                } else {
                                    1
                                };
                                if let Some(panel) = audit.settings_panel.as_mut() {
                                    panel.focus_ix = (panel.focus_ix + direction) % FIELDS;
                                    let handle = [
                                        panel.client_id.read(cx).focus_handle(cx),
                                        panel.client_secret.read(cx).focus_handle(cx),
                                    ][panel.focus_ix]
                                        .clone();
                                    window.focus(&handle, cx);
                                }
                            }
                            _ => {}
                        }
                    }),
                )
                .child(view)
                .into_any_element();
        }

        if let Some(browser) = self.sirv_browser.take() {
            let view = self.sirv_browser_view(&browser, cx);
            self.sirv_browser = Some(browser);
            // The click that opened the browser left focus on the header
            // button it replaced, so Escape had nowhere to land. Same fix as
            // the comparison: take focus next frame, once this tree exists.
            cx.defer_in(window, |audit, window, cx| {
                if let Some(browser) = audit.sirv_browser.as_ref() {
                    window.focus(&browser.focus, cx);
                }
            });
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(cx.theme().background)
                .track_focus(&self.sirv_browser.as_ref().unwrap().focus)
                .on_key_down(cx.listener(|audit, event: &gpui::KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        audit.sirv_browser = None;
                        cx.notify();
                    }
                }))
                .child(view)
                .into_any_element();
        }

        if let Some(comparison) = self.compare.take() {
            // Taken and put back so the view can borrow `self` immutably while the
            // listeners it builds hold a mutable handle to the same entity.
            let view = self.compare_view(&comparison, window, cx);
            self.compare = Some(comparison);
            // The click or Enter that opened this view left focus inside the list
            // it replaced, so Escape had nowhere to land. Take focus back next
            // frame, after the compare tree exists.
            cx.defer_in(window, |audit, window, cx| window.focus(&audit.focus, cx));
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
            .bg(cx.theme().background)
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
            .on_key_down(
                cx.listener(|audit, event: &gpui::KeyDownEvent, window, cx| {
                    // The filter box swallows its own keys, so these only fire when the
                    // list itself has focus. Shift turns any move into a selection
                    // drag from the anchor.
                    let extend = event.keystroke.modifiers.shift;
                    match event.keystroke.key.as_str() {
                        "down" => audit.step_cursor(1, extend, cx),
                        "up" => audit.step_cursor(-1, extend, cx),
                        "left" => audit.step_cursor_lateral(-1, extend, cx),
                        "right" => audit.step_cursor_lateral(1, extend, cx),
                        "pagedown" => audit.step_cursor(10, extend, cx),
                        "pageup" => audit.step_cursor(-10, extend, cx),
                        "home" => audit.step_cursor(isize::MIN / 2, extend, cx),
                        "end" => audit.step_cursor(isize::MAX / 2, extend, cx),
                        "escape" => {
                            // Nothing is open here, so escape means "put the list down":
                            // the ticked set clears, the way it does in every file manager.
                            if !audit.selected.is_empty() && !audit.converting {
                                audit.selected.clear();
                                audit.schedule_estimate(cx);
                                cx.notify();
                            }
                        }
                        "a" if event.keystroke.modifiers.control
                            || event.keystroke.modifiers.platform =>
                        {
                            // Select what the list shows, not what the folder holds:
                            // a filter that hides files from the list must hide them
                            // from Convert too.
                            if !audit.converting {
                                audit.selected.extend(audit.visible.iter().copied());
                                audit.schedule_estimate(cx);
                                cx.notify();
                            }
                        }
                        "," if event.keystroke.modifiers.control
                            || event.keystroke.modifiers.platform =>
                        {
                            audit.open_settings(window, cx);
                        }
                        "space" => audit.toggle_cursor_selection(cx),
                        "enter" => {
                            if !audit.converting
                                && let Some(entry) = audit.entry_at(audit.cursor)
                            {
                                audit.open_compare(entry, cx);
                            }
                        }
                        _ => {}
                    }
                }),
            )
            .on_drop(cx.listener(|audit, paths: &gpui::ExternalPaths, _, cx| {
                audit.drag_over = false;
                if let Some(path) = paths.paths().first() {
                    audit.request_path(path.clone(), cx);
                }
            }))
            .child(self.header(count, cx))
            .child(self.controls(cx))
            .children(self.notices(cx))
            .child(
                // The list runs to the window edge; hairlines above it, not a
                // card floating in padding.
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .overflow_hidden()
                    .bg(cx.theme().table)
                    // Columns take a width, not a share, so the remainder after the
                    // fixed ones has to be handed to the name column by hand.
                    .child(if self.grid {
                        // One virtualised band is one row of fixed-size tiles.
                        let (root_left, root_right) = root_horizontal_chrome(window);
                        let layout = gallery_layout(
                            f32::from(window.viewport_size().width),
                            root_left,
                            root_right,
                            count,
                        );
                        if let Some(previous) = self.gallery_columns
                            && previous != layout.columns
                        {
                            self.gallery_scroll
                                .scroll_to_item_strict(0, ScrollStrategy::Top);
                        }
                        self.gallery_columns = Some(layout.columns);
                        uniform_list(
                            "gallery",
                            layout.rows,
                            cx.processor(|audit, range: std::ops::Range<usize>, _window, cx| {
                                range
                                    .map(|band| {
                                        // A plain loop: the closure form borrows `audit`
                                        // mutably for `request_thumb` and immutably for
                                        // `tile`, which nested closures cannot express.
                                        let mut tiles = Vec::new();
                                        let (root_left, root_right) =
                                            root_horizontal_chrome(_window);
                                        let layout = gallery_layout(
                                            f32::from(_window.viewport_size().width),
                                            root_left,
                                            root_right,
                                            audit.visible.len(),
                                        );
                                        for row in layout.band_range(band) {
                                            let Some(entry) = audit.entry_at(row) else {
                                                continue;
                                            };
                                            audit.request_thumb(entry, cx);
                                            tiles.push(audit.tile(row, entry, layout.tile, cx));
                                        }
                                        div().flex().gap_2().children(tiles)
                                    })
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.gallery_scroll)
                        .flex_1()
                        .p_2()
                        .into_any_element()
                    } else if let Some(table) = self.table.as_ref() {
                        DataTable::new(table)
                            .stripe(false)
                            .bordered(false)
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
            // The status bar sits at the very bottom, so nothing above it ever
            // changes height and the list never jumps.
            .child(self.summary(cx))
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
    let sources: Vec<PathBuf> = entries.iter().map(|entry| entry.path.clone()).collect();
    let by_path: HashMap<&Path, &Entry> = entries
        .iter()
        .map(|entry| (entry.path.as_path(), entry))
        .collect();

    // Lines arrive as files finish rather than in list order, which is what running
    // several at once looks like. The totals are the same either way.
    let totals = parking_lot::Mutex::new((0u64, 0u64, 0usize));
    convert::convert_each(
        root,
        &sources,
        &out_dir,
        format,
        quality,
        max_edge,
        |source, converted| {
            let Some(entry) = by_path.get(source) else {
                return;
            };
            let mut totals = totals.lock();
            match converted {
                Some(converted) => {
                    totals.0 += entry.bytes;
                    totals.1 += converted.bytes;
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
                    totals.2 += 1;
                    println!("{:<52} failed", entry.name());
                }
            }
        },
    );
    let (before, after, failed) = *totals.lock();

    let growth = after > before;
    let delta = before.abs_diff(after);
    let percent = delta as f64 / before.max(1) as f64 * 100.;
    println!(
        "\n{} converted to {} at {} ({}): {} -> {}, {} {} ({percent:.0}%){}",
        entries.len() - failed,
        format.label(),
        quality.label(),
        max_edge.label(),
        format_bytes(before),
        format_bytes(after),
        if growth { "grew" } else { "saved" },
        format_bytes(delta),
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
            unreadable: Vec::new(),
            existing_output: 0,
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
                unreadable: Vec::new(),
                existing_output: 0,
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
        existing_output: scanned.existing_output,
        open_single,
        format: args.format,
        quality: args.quality,
        max_edge: args.max_edge,
        grid: args.grid,
    });
}

/// Build the audit view for a window. Shared by the app and the screenshot harness
/// so that what gets captured is the thing that ships.
fn build_audit(launch: Launch, window: &mut Window, cx: &mut App) -> gpui::Entity<Audit> {
    let Launch {
        root,
        entries,
        skipped_raw,
        unreadable,
        existing_output,
        open_single,
        format,
        quality,
        max_edge,
        grid,
    } = launch;

    let audit = cx.new(|cx| {
        let focus = cx.focus_handle();
        focus.focus(window, cx);

        let filter_input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter by name"));
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
                if audit.converting {
                    return;
                }
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
            table: None,
            table_signature: None,
            root,
            entries,
            skipped_raw,
            heaviest: 0,
            visible_bytes: 0,
            mislabelled,
            thumbs: HashMap::new(),
            requested: HashSet::new(),
            thumb_order: VecDeque::new(),
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
            finding: None,
            filter_input,
            cursor: 0,
            anchor: 0,
            slider_quality: quality.0.unwrap_or(80.),
            grid,
            gallery_scroll: UniformListScrollHandle::new(),
            gallery_columns: None,
            estimate: None,
            estimate_generation: 0,
            dataset_generation: 0,
            scan_generation: 0,
            scanning: None,
            focus,
            titled: String::new(),
            settings: settings::Settings::default(),
            settings_save_pending: false,
            cached: None,
            results: HashMap::new(),
            converting: false,
            active_target_count: None,
            failures: Vec::new(),
            unreadable,
            existing_output,
            drag_over: false,
            sirv_pairing: None,
            sirv_counts: None,
            sirv_job: None,
            sirv_generation: 0,
            sirv_browser: None,
            settings_panel: None,
            compare: None,
        };
        audit.refresh_visible();
        audit.schedule_estimate(cx);
        if open_single {
            audit.open_compare(0, cx);
        }
        audit
    });

    // Only now that the audit is a live entity, because building the
    // table asks the delegate how many rows there are and answering
    // that means reading the audit.
    let table = {
        let delegate = AuditTable::new(audit.downgrade(), window);
        cx.new(|cx| TableState::new(delegate, window, cx))
    };
    audit.update(cx, |audit, _| audit.table = Some(table));

    audit
}

/// Set the library up and pick the colours. Shared with the screenshot harness, so
/// what that captures is what the window draws.
fn init_theme(cx: &mut App) {
    // Must run before any gpui-component type is constructed.
    gpui_component::init(cx);
    // Dark by default. Judging compression against a bright chrome is a bad idea,
    // and the comparison view is full-bleed imagery either way.
    gpui_component::Theme::change(gpui_component::ThemeMode::Dark, None, cx);
    // The stock dark theme paints `primary` white, which makes the one button that
    // commits work a white slab and leaves nothing to point at anything else. This
    // app already has a blue.
    //
    // Both halves of the theme have to be told. A button takes its fill from the
    // token set and its label from the colour set, so setting one and not the other
    // is how you get black text on a blue button.
    let theme = gpui_component::Theme::global_mut(cx);
    // One neutral ramp, barely blue, so the imagery in the table carries the
    // colour and the chrome reads as an instrument panel rather than a website.
    let background = gpui::Hsla::from(gpui::rgb(0x0a0d12));
    let surface = gpui::Hsla::from(gpui::rgb(0x10151d));
    let table = gpui::Hsla::from(gpui::rgb(0x0d1118));
    let border = gpui::Hsla::from(gpui::rgb(0x232d3b));
    let foreground = gpui::Hsla::from(gpui::rgb(0xe8eef6));
    let muted = gpui::Hsla::from(gpui::rgb(0x8fa0b5));
    let base = gpui::Hsla::from(gpui::rgb(0x4c8dff));
    let hover = gpui::Hsla::from(gpui::rgb(0x65a0ff));
    let active = gpui::Hsla::from(gpui::rgb(0x3b79e6));
    let focus = gpui::Hsla::from(gpui::rgb(0x8fbcff));

    theme.background = background;
    theme.secondary = surface;
    theme.table = table;
    theme.input = border;
    theme.border = border;
    theme.foreground = foreground;
    theme.muted_foreground = muted;
    theme.group_box = surface;
    theme.group_box_foreground = foreground;
    theme.list_hover = gpui::Hsla::from(gpui::rgb(0x161d28));
    theme.list_active = gpui::Hsla::from(gpui::rgb(0x1a2740));
    theme.list_active_border = base;
    theme.table_head = background;
    theme.table_head_foreground = muted;
    theme.table_hover = gpui::Hsla::from(gpui::rgb(0x141b26));
    theme.table_row_border = gpui::Hsla::from(gpui::rgb(0x1a222e));
    theme.ring = focus;

    theme.primary = base;
    theme.primary_hover = hover;
    theme.primary_active = active;
    theme.primary_foreground = gpui::white();
    theme.button_primary = base;
    theme.button_primary_hover = hover;
    theme.button_primary_active = active;
    theme.button_primary_foreground = gpui::white();

    theme.tokens.button_primary = base.into();
    theme.tokens.button_primary_hover = hover.into();
    theme.tokens.button_primary_active = active.into();
    theme.tokens.button_primary_foreground = gpui::white().into();

    // SF Pro Text for words, Fira Code for every measured number. A column of
    // byte counts in a proportional face will not align down its right edge,
    // and an audit that will not align is not an audit.
    theme.font_family = "SF Pro Text".into();
    theme.mono_font_family = "Fira Code".into();
    theme.mono_font_size = px(12.);
}

/// Everything the window needs to open. A struct rather than nine positional
/// arguments, three of which are `usize` and two of which are `bool`.
struct Launch {
    root: PathBuf,
    entries: Vec<Entry>,
    skipped_raw: usize,
    unreadable: Vec<PathBuf>,
    existing_output: usize,
    open_single: bool,
    format: Format,
    quality: Quality,
    max_edge: MaxEdge,
    grid: bool,
}

fn run_window(launch: Launch) {
    application()
        // Every `IconName` is an SVG loaded through the app's asset source. Without
        // this the icons resolve to nothing and the toolbar renders as bare words.
        .with_assets(gpui_component_assets::Assets)
        .run(move |cx: &mut App| {
            init_theme(cx);

            // The thumbnail cache grows with every folder ever opened. Bound it once,
            // here, where a whole-directory pass costs a thread nobody waits for.
            cx.background_executor()
                .spawn(async { thumbs::trim_cache() })
                .detach();

            let remembered = settings::load();
            let (width, height) = restored_window_size(remembered.width, remembered.height);
            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_min_size: Some(size(px(WINDOW_MIN_WIDTH), px(WINDOW_MIN_HEIGHT))),
                    app_id: Some("imageguide".to_string()),
                    ..Default::default()
                },
                |window, cx| {
                    let audit = build_audit(launch, window, cx);
                    // Dialogs, notifications and tooltips are drawn by the Root, so
                    // the window's first level has to be one.
                    cx.new(|cx| Root::new(audit, window, cx).bg(cx.theme().background))
                },
            )
            .unwrap();
            cx.activate(true);
        });
}

/// Render the audit window to a PNG, so a change to it can actually be looked at.
///
/// gpui draws the frame to a texture and hands back the pixels, which needs no
/// screen and no screen-recording permission — the alternative was describing the
/// window to someone else and asking them what they saw.
///
///     cargo test --bin imageguide -- --ignored --nocapture screenshot
///
/// Set `IMAGEGUIDE_SHOT_DIR` to choose the folder to audit and `IMAGEGUIDE_SHOT_OUT`
/// to choose where the picture lands.
#[cfg(test)]
mod screenshot {
    use super::*;
    use gpui::HeadlessAppContext;

    #[test]
    #[ignore = "renders a window; run it deliberately"]
    fn screenshot() {
        let folder = std::env::var("IMAGEGUIDE_SHOT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("imageguide-demo"));
        let out = std::env::var("IMAGEGUIDE_SHOT_OUT")
            .unwrap_or_else(|_| "/tmp/imageguide-shot.png".to_string());
        // Which of the shapes the window can take: list, grid, compare or empty.
        let mode = std::env::var("IMAGEGUIDE_SHOT_MODE").unwrap_or_else(|_| "list".to_string());

        let mut scanned = scan::scan(&folder);
        assert!(
            !scanned.entries.is_empty(),
            "{} holds no images to draw",
            folder.display()
        );
        // The empty state only appears for a root that is not a folder at all.
        let root = if mode == "empty" {
            scanned.entries.clear();
            PathBuf::new()
        } else {
            folder.clone()
        };

        // A real platform, only for its text system: glyph metrics decide every
        // width in the window, so a fake one would measure a different layout.
        let text_system = gpui_platform::current_platform(true).text_system();
        let mut cx = HeadlessAppContext::with_platform(
            text_system,
            std::sync::Arc::new(gpui_component_assets::Assets),
            gpui_platform::current_headless_renderer,
        );

        cx.update(init_theme);

        let window = cx
            .open_window(size(px(1100.), px(720.)), |window, cx| {
                let audit = build_audit(
                    Launch {
                        root: root.clone(),
                        entries: scanned.entries,
                        skipped_raw: scanned.skipped_raw,
                        unreadable: scanned.unreadable,
                        existing_output: scanned.existing_output,
                        open_single: mode == "compare",
                        format: Format::WebP,
                        quality: Quality::lossy(80.),
                        max_edge: MaxEdge::FULL,
                        grid: mode == "grid",
                    },
                    window,
                    cx,
                );
                cx.new(|cx| Root::new(audit, window, cx).bg(cx.theme().background))
            })
            .expect("window opens");

        // Let the thumbnail decodes and the estimate land before drawing. The
        // estimate waits out a settling timer first, so the clock has to move.
        cx.allow_parking();
        cx.run_until_parked();
        cx.advance_clock(ESTIMATE_DELAY + Duration::from_millis(200));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(1200));
        cx.run_until_parked();

        let image = cx
            .capture_screenshot(window.into())
            .expect("frame renders to an image");
        image.save(&out).expect("png writes");
        println!("wrote {out} ({}x{})", image.width(), image.height());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use image::ImageFormat;
    use std::path::PathBuf;

    struct AuditHarness {
        audit: gpui::Entity<Audit>,
    }

    impl gpui::Render for AuditHarness {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            self.audit.clone()
        }
    }

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

    /// The list is sorted heaviest first, so its outlier is always sample one. Whatever
    /// that file does must stop at the slice it was taken from.
    #[test]
    fn each_slice_is_projected_by_its_own_sample() {
        // A gigabyte of images that compress 100:1, then a gigabyte that does not
        // compress at all.
        let (projected, counted) = project_total(&[
            (1_000_000_000, Some((10_000_000, 100_000))),
            (1_000_000_000, Some((10_000_000, 10_000_000))),
        ])
        .expect("two samples encoded");

        assert_eq!(counted, 2);
        assert_eq!(
            projected, 1_010_000_000,
            "10 MB from the first slice and the whole gigabyte from the second"
        );
        // The summed-bytes ratio this replaced: 10.1 MB of sample from 20 MB of source
        // called the entire 2 GB half its size.
    }

    #[test]
    fn a_slice_whose_sample_would_not_decode_borrows_the_average() {
        let (projected, counted) = project_total(&[
            (100, Some((1000, 100))),
            (100, Some((1000, 300))),
            (100, None),
        ])
        .expect("two of three encoded");

        assert_eq!(counted, 2, "the broken file is not counted as evidence");
        assert_eq!(projected, 10 + 30 + 20, "its slice takes the 0.2 average");
    }

    #[test]
    fn nothing_encoded_is_no_estimate() {
        assert!(project_total(&[(1000, None), (2000, None)]).is_none());
        assert!(project_total(&[]).is_none());
    }

    #[test]
    fn conversion_targets_follow_visible_order() {
        let visible = [2, 0, 1];
        assert_eq!(conversion_targets(&visible, &HashSet::new()), vec![2, 0, 1]);

        let selected = HashSet::from([0, 3]);
        assert_eq!(conversion_targets(&visible, &selected), vec![0]);

        let hidden = HashSet::from([3]);
        assert!(conversion_targets(&visible, &hidden).is_empty());
    }

    #[test]
    fn conversion_progress_publishes_by_worker_window_and_flushes_the_tail() {
        assert!(!progress_batch_ready(7, 8, true));
        assert!(progress_batch_ready(8, 8, true));
        assert!(progress_batch_ready(3, 8, false));
    }

    #[test]
    fn table_layout_keeps_decision_columns_at_compact_width() {
        let (compact, compact_name, compact_columns) = AuditTable::layout(760., true);
        assert!(compact);
        assert!(compact_name >= W_NAME_MIN);
        assert!(compact_columns.contains(&TableColumn::Weight));
        assert!(compact_columns.contains(&TableColumn::Result));
        assert!(compact_columns.contains(&TableColumn::Density));

        let (wide, wide_name, wide_columns) = AuditTable::layout(1100., true);
        assert!(!wide);
        assert!(wide_name > compact_name);
        assert!(wide_columns.contains(&TableColumn::Density));
        assert!(wide_columns.contains(&TableColumn::Weight));
        assert!(wide_columns.contains(&TableColumn::Result));
    }

    /// The app sorts indices into an unmoved `entries`; these tests sort the data
    /// directly, which is the same comparator either way.
    fn sort_entries(entries: &mut [Entry], sort: Sort) {
        entries.sort_by(|a, b| compare_entries(a, b, sort));
    }

    /// `img` will not scale an image past its own size, so a thumbnail smaller than the
    /// slot it is drawn in does not fill it — it sits in the middle of the empty space.
    /// The gallery looked like that at 96px in a 224px tile. The two constants live in
    /// different modules, so this is what stops them drifting apart again.
    #[test]
    fn the_gallery_never_asks_for_more_than_a_thumbnail_holds() {
        // `tile` draws the image inside the tile's own padding.
        let widest = TILE_MAX - 16.;
        assert!(
            widest <= thumbs::THUMB_EDGE as f32,
            "a {TILE_MAX}px tile draws an image {widest}px wide, \
             and thumbnails are only {}px",
            thumbs::THUMB_EDGE
        );
    }

    /// Names before counts, wherever the window reports a set of files it could not
    /// handle. "3 would not decode" gives you nowhere to look.
    #[test]
    fn a_report_names_a_few_files_and_then_counts_the_rest() {
        let of = |names: &[&str]| named(names.iter().map(|name| name.to_string()));
        assert_eq!(of(&[]), "");
        assert_eq!(of(&["a.png"]), "a.png");
        assert_eq!(of(&["a.png", "b.png", "c.png"]), "a.png, b.png, c.png");
        assert_eq!(
            of(&["a.png", "b.png", "c.png", "d.png", "e.png"]),
            "a.png, b.png, c.png and 2 more"
        );
    }

    /// The audit's findings have to be reachable. Narrowing to one shows those rows and
    /// nothing else, and asking for the same one again widens the list back out.
    #[gpui::test]
    fn a_finding_narrows_the_list_and_a_second_click_widens_it(cx: &mut TestAppContext) {
        let (audit, cx) = finding_audit(cx);
        let shown = |audit: &Audit| -> Vec<String> {
            audit
                .visible
                .iter()
                .filter_map(|index| audit.entries.get(*index))
                .map(|entry| entry.name())
                .collect()
        };

        audit.update(cx, |audit, cx| {
            assert_eq!(audit.visible.len(), 3, "everything, to begin with");

            audit.set_finding(Finding::Mislabelled, cx);
            assert_eq!(
                shown(audit),
                ["liar.webp"],
                "only the file whose extension disagrees with its bytes"
            );

            audit.set_finding(Finding::Heavy, cx);
            assert_eq!(
                shown(audit),
                ["screenshot.png"],
                "one finding at a time, and heavy means bytes per pixel"
            );

            audit.set_finding(Finding::Heavy, cx);
            assert_eq!(audit.visible.len(), 3, "asking again puts the list back");
        });
    }

    /// Unpairing has to stop a transfer, not leave it uploading into a folder the
    /// window is no longer paired to. The loop reads `sirv_generation` before each
    /// file, so bumping it is the stop.
    #[gpui::test]
    fn unpairing_stops_a_running_transfer(cx: &mut TestAppContext) {
        let (audit, cx) = finding_audit(cx);

        audit.update(cx, |audit, cx| {
            audit.sirv_job = Some(SirvJob {
                kind: SirvJobKind::Push,
                done: 3,
                total: 100,
                failures: Vec::new(),
                finished: false,
                generation: audit.sirv_generation,
            });
            let running = audit.sirv_generation;

            audit.unpair_sirv(cx);

            assert_ne!(
                audit.sirv_generation, running,
                "the loop's next check has to fail"
            );
            let job = audit.sirv_job.as_ref().expect("the job is still reported");
            assert!(job.finished, "and it stops saying it is running");
            assert_eq!(
                job.failures,
                ["stopped"],
                "the reason is named, not implied"
            );
        });
    }

    /// A finding belongs to the folder it was found in.
    #[gpui::test]
    fn opening_another_folder_clears_the_finding(cx: &mut TestAppContext) {
        let (audit, cx) = finding_audit(cx);
        audit.update(cx, |audit, cx| audit.set_finding(Finding::Heavy, cx));

        cx.update(|window, cx| {
            audit.update(cx, |audit, cx| {
                audit.install_dataset(
                    scan::Scan {
                        entries: vec![entry("new.png", 10, 10, 100, ImageFormat::Png)],
                        skipped_raw: 0,
                        unreadable: Vec::new(),
                        existing_output: 0,
                    },
                    PathBuf::from("/elsewhere"),
                    false,
                    window,
                    cx,
                );
            });
        });

        audit.read_with(cx, |audit, _| {
            assert_eq!(audit.finding, None);
            assert_eq!(audit.visible.len(), 1);
        });
    }

    fn finding_audit(
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<Audit>, &mut gpui::VisualTestContext) {
        cx.update(init_theme);
        // A PNG named `.webp` is the mislabelled one. The screenshot is 30 bytes per
        // pixel; the photo is a tenth of one.
        let launch = Launch {
            root: PathBuf::new(),
            entries: vec![
                entry("photo.jpg", 1000, 1000, 100_000, ImageFormat::Jpeg),
                entry("screenshot.png", 100, 100, 300_000, ImageFormat::Png),
                entry("liar.webp", 100, 100, 1_000, ImageFormat::Png),
            ],
            skipped_raw: 0,
            unreadable: Vec::new(),
            existing_output: 0,
            open_single: false,
            format: Format::WebP,
            quality: Quality::lossy(80.),
            max_edge: MaxEdge::FULL,
            grid: false,
        };
        let (harness, cx) = cx.add_window_view(move |window, cx| AuditHarness {
            audit: build_audit(launch, window, cx),
        });
        let audit = harness.read_with(cx, |harness, _| harness.audit.clone());
        (audit, cx)
    }

    fn pointer_checkbox_audit(
        grid: bool,
        cx: &mut TestAppContext,
    ) -> (gpui::Entity<Audit>, &mut gpui::VisualTestContext) {
        cx.update(init_theme);
        let launch = Launch {
            root: PathBuf::new(),
            entries: vec![
                entry("first.png", 10, 10, 100, ImageFormat::Png),
                entry("second.png", 10, 10, 200, ImageFormat::Png),
            ],
            skipped_raw: 0,
            unreadable: Vec::new(),
            existing_output: 0,
            open_single: false,
            format: Format::WebP,
            quality: Quality::lossy(80.),
            max_edge: MaxEdge::FULL,
            grid,
        };
        let (harness, cx) = cx.add_window_view(move |window, cx| {
            let built = build_audit(launch, window, cx);
            AuditHarness { audit: built }
        });
        let audit = harness.read_with(cx, |harness, _| harness.audit.clone());
        audit.update(cx, |audit, _| {
            audit.selected.extend([0, 1]);
            audit.estimate = Some((123, 2));
        });
        cx.update(|window, cx| window.draw(cx).clear(cx));
        (audit, cx)
    }

    fn assert_pointer_checkbox_toggle(
        audit: &gpui::Entity<Audit>,
        selector: &'static str,
        cx: &mut gpui::VisualTestContext,
    ) {
        let checkbox = cx
            .debug_bounds(selector)
            .expect("the checkbox must be rendered in its parent event tree");
        let before = audit.read_with(cx, |audit, _| audit.estimate_generation);

        cx.simulate_click(checkbox.center(), gpui::Modifiers::none());
        audit.read_with(cx, |audit, _| {
            assert_eq!(audit.selected, [1].into_iter().collect());
            assert!(audit.compare.is_none());
            assert_eq!(audit.estimate_generation, before + 1);
            assert_eq!(audit.estimate, None);
        });

        cx.update(|window, cx| window.draw(cx).clear(cx));
        let checkbox = cx
            .debug_bounds(selector)
            .expect("the checkbox must remain rendered after its controlled state changes");
        cx.simulate_click(checkbox.center(), gpui::Modifiers::none());
        audit.read_with(cx, |audit, _| {
            assert_eq!(audit.selected, [0, 1].into_iter().collect());
            assert!(audit.compare.is_none());
            assert_eq!(audit.estimate_generation, before + 2);
            assert_eq!(audit.estimate, None);
        });
    }

    #[gpui::test]
    fn grid_checkbox_pointer_click_stays_inside_checkbox(cx: &mut TestAppContext) {
        let (audit, cx) = pointer_checkbox_audit(true, cx);
        assert_pointer_checkbox_toggle(&audit, "grid-checkbox-0", cx);
    }

    #[gpui::test]
    fn table_checkbox_pointer_click_stays_inside_checkbox(cx: &mut TestAppContext) {
        let (audit, cx) = pointer_checkbox_audit(false, cx);
        assert_pointer_checkbox_toggle(&audit, "table-checkbox-0", cx);
    }

    #[gpui::test]
    fn keyboard_selection_refreshes_estimate(cx: &mut TestAppContext) {
        let (audit, cx) = pointer_checkbox_audit(false, cx);
        let before = audit.read_with(cx, |audit, _| audit.estimate_generation);

        audit.update(cx, |audit, cx| audit.toggle_cursor_selection(cx));
        audit.read_with(cx, |audit, _| {
            assert_eq!(audit.selected, [0].into_iter().collect());
            assert_eq!(audit.estimate_generation, before + 1);
            assert_eq!(audit.estimate, None);
        });
    }

    #[test]
    fn checkbox_activation_owns_only_unmodified_space_and_enter() {
        for key in ["space", "enter"] {
            let event = gpui::KeyDownEvent {
                keystroke: gpui::Keystroke {
                    key: key.into(),
                    ..Default::default()
                },
                is_held: false,
                prefer_character_input: false,
            };
            assert!(is_checkbox_activation_key(&event));

            let mut modified = event.clone();
            modified.keystroke.modifiers.control = true;
            assert!(!is_checkbox_activation_key(&modified));
        }

        let other = gpui::KeyDownEvent {
            keystroke: gpui::Keystroke {
                key: "down".into(),
                ..Default::default()
            },
            is_held: false,
            prefer_character_input: false,
        };
        assert!(!is_checkbox_activation_key(&other));
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

    #[test]
    fn restored_window_size_defaults_invalid_values_and_clamps_finite_values() {
        for invalid in [
            None,
            Some(f32::NAN),
            Some(f32::INFINITY),
            Some(f32::NEG_INFINITY),
        ] {
            assert_eq!(
                restored_window_size(invalid, invalid),
                (WINDOW_DEFAULT_WIDTH, WINDOW_DEFAULT_HEIGHT)
            );
        }
        assert_eq!(
            restored_window_size(Some(600.), Some(400.)),
            (WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT)
        );
        assert_eq!(restored_window_size(Some(1100.), Some(720.)), (1100., 720.));
    }

    #[test]
    fn gallery_geometry_accounts_for_root_chrome_and_supported_widths() {
        assert_eq!(gallery_layout(760., 0., 0., 100).columns, 4);
        assert_eq!(gallery_layout(760., 21., 21., 100).columns, 3);
        assert_eq!(gallery_layout(760., 0., 21., 100).columns, 3);

        assert_eq!(gallery_layout(760., 22., 22., 100).columns, 3);
        assert_eq!(gallery_layout(873., 22., 22., 100).columns, 4);
        assert_eq!(gallery_layout(900., 22., 22., 100).columns, 4);
        assert_eq!(gallery_layout(1100., 22., 22., 100).columns, 5);
    }

    #[test]
    fn gallery_changes_column_only_at_each_reachable_threshold() {
        let root = 22.;
        for columns in 2..=GALLERY_MAX_COLUMNS {
            let threshold = 2. * root
                + 2. * (ROOT_PADDING + ROOT_BORDER + GALLERY_PADDING + GALLERY_BORDER)
                + columns as f32 * TILE_MIN
                + (columns - 1) as f32 * TILE_GAP;
            assert_eq!(
                gallery_layout(threshold - 1., root, root, 100).columns,
                columns - 1
            );
            assert_eq!(gallery_layout(threshold, root, root, 100).columns, columns);
        }
    }

    #[test]
    fn gallery_bands_cover_each_entry_once_for_one_three_and_five_columns() {
        for columns in [1, 3, 5] {
            let chrome = 2. * (ROOT_PADDING + ROOT_BORDER + GALLERY_PADDING + GALLERY_BORDER);
            let width = chrome + columns as f32 * TILE_MIN + (columns - 1) as f32 * TILE_GAP;
            let layout = gallery_layout(width, 0., 0., 13);
            assert_eq!(layout.columns, columns);
            assert_eq!(layout.rows, 13_usize.div_ceil(columns));
            assert_eq!(
                layout.bands().flatten().collect::<Vec<_>>(),
                (0..13).collect::<Vec<_>>()
            );
        }
    }

    #[gpui::test]
    fn gallery_scroll_resets_only_when_the_production_column_count_changes(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(init_theme);
        let entries = (0..120)
            .map(|index| entry(&format!("image-{index}.png"), 1, 1, 1, ImageFormat::Png))
            .collect();
        let mut audit_entity = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let audit = build_audit(
                Launch {
                    root: PathBuf::new(),
                    entries,
                    skipped_raw: 0,
                    unreadable: Vec::new(),
                    existing_output: 0,
                    open_single: false,
                    format: Format::WebP,
                    quality: Quality::lossy(80.),
                    max_edge: MaxEdge::FULL,
                    grid: true,
                },
                window,
                cx,
            );
            audit_entity = Some(audit.clone());
            Root::new(audit, window, cx).bg(cx.theme().background)
        });
        let audit = audit_entity.expect("audit is built for the production Root");

        cx.simulate_resize(size(px(873.), px(720.)));
        cx.run_until_parked();
        // Root installs its client inset during its first draw. Settle that frame
        // before establishing the deliberately deep scroll position.
        cx.simulate_resize(size(px(873.), px(720.)));
        cx.run_until_parked();
        audit.update_in(cx, |audit, window, _| {
            audit
                .gallery_scroll
                .scroll_to_item_strict(12, ScrollStrategy::Top);
            window.refresh();
        });
        cx.simulate_resize(size(px(873.), px(720.)));
        cx.run_until_parked();
        assert!(audit.read_with(cx, |audit, _| audit.gallery_scroll.is_scrollable()));
        assert!(audit.read_with(cx, |audit, _| {
            audit.gallery_scroll.0.borrow().base_handle.offset().y < px(0.)
        }));

        cx.simulate_resize(size(px(600.), px(720.)));
        cx.run_until_parked();
        assert_eq!(
            audit.read_with(cx, |audit, _| audit
                .gallery_scroll
                .0
                .borrow()
                .base_handle
                .offset()
                .y),
            px(0.)
        );

        audit.update_in(cx, |audit, window, _| {
            audit
                .gallery_scroll
                .scroll_to_item_strict(12, ScrollStrategy::Top);
            window.refresh();
        });
        cx.simulate_resize(size(px(600.), px(720.)));
        cx.run_until_parked();
        cx.simulate_resize(size(px(700.), px(720.)));
        cx.run_until_parked();
        assert!(audit.read_with(cx, |audit, _| {
            audit.gallery_scroll.0.borrow().base_handle.offset().y < px(0.)
        }));
    }

    #[gpui::test]
    fn opening_another_large_folder_resets_gallery_scroll_at_the_same_column_count(
        cx: &mut gpui::TestAppContext,
    ) {
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00,
            0x00, 0xb5, 0x1c, 0x0c, 0x02, 0x00, 0x00, 0x00, 0x0b, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0xfc, 0xff, 0x9f, 0x01, 0x00, 0x03, 0x03, 0x02, 0x00, 0xee, 0xfe, 0x3d,
            0x68, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];

        let test_root = std::env::temp_dir().join(format!(
            "imageguide-open-path-scroll-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("the system clock is after the Unix epoch")
                .as_nanos()
        ));
        let first_folder = test_root.join("first");
        let second_folder = test_root.join("second");
        for folder in [&first_folder, &second_folder] {
            std::fs::create_dir_all(folder).expect("the test gallery folder is created");
            for index in 0..120 {
                std::fs::write(folder.join(format!("image-{index}.png")), PNG)
                    .expect("the test gallery image is written");
            }
        }

        cx.update(init_theme);
        let mut audit_entity = None;
        let (_, cx) = cx.add_window_view(|window, cx| {
            let audit = build_audit(
                Launch {
                    root: PathBuf::new(),
                    entries: Vec::new(),
                    skipped_raw: 0,
                    unreadable: Vec::new(),
                    existing_output: 0,
                    open_single: false,
                    format: Format::WebP,
                    quality: Quality::lossy(80.),
                    max_edge: MaxEdge::FULL,
                    grid: true,
                },
                window,
                cx,
            );
            audit_entity = Some(audit.clone());
            Root::new(audit, window, cx).bg(cx.theme().background)
        });
        let audit = audit_entity.expect("audit is built for the production Root");

        cx.simulate_resize(size(px(873.), px(720.)));
        cx.run_until_parked();
        cx.simulate_resize(size(px(873.), px(720.)));
        cx.run_until_parked();
        let first_scan = scan::scan(&first_folder);
        audit.update_in(cx, |audit, window, cx| {
            audit.install_dataset(first_scan, first_folder.clone(), false, window, cx);
            window.refresh();
        });
        cx.simulate_resize(size(px(873.), px(720.)));
        cx.run_until_parked();
        audit.update_in(cx, |audit, window, _| {
            audit
                .gallery_scroll
                .scroll_to_item_strict(12, ScrollStrategy::Top);
            window.refresh();
        });
        cx.simulate_resize(size(px(873.), px(720.)));
        cx.run_until_parked();
        assert!(audit.read_with(cx, |audit, _| {
            audit.gallery_scroll.0.borrow().base_handle.offset().y < px(0.)
        }));

        let second_scan = scan::scan(&second_folder);
        audit.update_in(cx, |audit, window, cx| {
            audit.install_dataset(second_scan, second_folder.clone(), false, window, cx);
            window.refresh();
        });
        cx.simulate_resize(size(px(873.), px(720.)));
        cx.run_until_parked();
        assert_eq!(
            audit.read_with(cx, |audit, _| audit
                .gallery_scroll
                .0
                .borrow()
                .base_handle
                .offset()
                .y),
            px(0.)
        );

        std::fs::remove_dir_all(test_root).expect("the test gallery folders are removed");
    }
}
