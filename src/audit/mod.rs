//! Audit window state, background jobs, rendering, and tests.

mod table;
#[cfg(test)]
mod tests;
mod view;

use table::AuditTable;
#[cfg(test)]
use table::{TableColumn, W_NAME_MIN};
use view::meter;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::compare::Pair;
use crate::convert::{Format, MaxEdge, Quality};
use crate::scan::{Entry, format_bytes, format_name};
use crate::{Launch, compare, convert, scan, settings, sirv, thumbs};
use futures::future::select_all;
use gpui::{
    App, Context, Decorations, FocusHandle, Focusable as _, FontWeight, RenderImage,
    ScrollStrategy, UniformListScrollHandle, Window, div, img, prelude::*, px, rgb, rgba,
    uniform_list,
};
use gpui_component::alert::Alert;
use gpui_component::button::{Button, ButtonGroup, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::progress::Progress;
use gpui_component::scroll::{Scrollbar, ScrollbarMode};
use gpui_component::slider::{Slider, SliderEvent, SliderState};
use gpui_component::switch::Switch;
use gpui_component::table::{Column as TableCol, ColumnSort, DataTable, TableDelegate, TableState};
use gpui_component::tag::Tag;
use gpui_component::{ActiveTheme, Disableable, IconName, Selectable, Sizable};

// Colours come from `cx.theme()` rather than a private palette. The window is
// built out of this library's buttons, inputs and tags, and a hand-picked set of
// greys sitting behind them agreed with nothing — the chrome and the controls were
// two designs in one window.

/// Bytes per output pixel, banded. A photographic JPEG lands near 0.2; a
/// screenshot saved as PNG can be ten times that. The number was already in the
/// list and every row printed it in the same grey, which made the app's one
/// diagnostic something you had to read rather than see.
const DENSITY_GOOD: f32 = 0.5;
const DENSITY_HEAVY: f32 = 1.5;
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

pub(crate) struct Audit {
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
    /// Deliberately overwrite the differing local copy.
    PullChanged,
    Push,
    /// Deliberately overwrite the differing remote copy.
    PushChanged,
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
        self.run_pull(false, cx);
    }

    /// Deliberately replace every differing local copy with the remote one.
    fn start_pull_changed(&mut self, cx: &mut Context<Self>) {
        self.run_pull(true, cx);
    }

    fn run_pull(&mut self, differing: bool, cx: &mut Context<Self>) {
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
        let local_sizes: HashMap<String, u64> = self
            .entries
            .iter()
            .filter_map(|entry| {
                sirv::relative_key(&self.root, &entry.path).map(|key| (key, entry.bytes))
            })
            .collect();
        let plan = sirv::pull_plan(&remote, &dir, &local_sizes, differing);
        if plan.is_empty() {
            return;
        }
        let total = plan.len();
        self.sirv_generation = self.sirv_generation.wrapping_add(1);
        let generation = self.sirv_generation;
        self.sirv_job = Some(SirvJob {
            kind: if differing {
                SirvJobKind::PullChanged
            } else {
                SirvJobKind::Pull
            },
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

    /// Upload every local file Sirv lacks.
    fn start_push(&mut self, cx: &mut Context<Self>) {
        self.run_push(sirv::SyncState::OnlyLocal, cx);
    }

    /// Deliberately replace every differing remote copy with the local one.
    fn start_push_changed(&mut self, cx: &mut Context<Self>) {
        self.run_push(sirv::SyncState::Changed, cx);
    }

    fn run_push(&mut self, accept: sirv::SyncState, cx: &mut Context<Self>) {
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
        let plan = sirv_push_plan(&self.root, &self.entries, files, accept);
        if plan.is_empty() {
            return;
        }
        let total = plan.len();
        self.sirv_generation = self.sirv_generation.wrapping_add(1);
        let generation = self.sirv_generation;
        self.sirv_job = Some(SirvJob {
            kind: if accept == sirv::SyncState::Changed {
                SirvJobKind::PushChanged
            } else {
                SirvJobKind::Push
            },
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

fn sirv_push_plan(
    root: &Path,
    entries: &[scan::Entry],
    files: &HashMap<String, sirv::Node>,
    accept: sirv::SyncState,
) -> Vec<(String, PathBuf)> {
    entries
        .iter()
        .filter_map(|entry| {
            let key = sirv::relative_key(root, &entry.path)?;
            (sirv::classify(entry.bytes, files.get(&key)) == accept)
                .then(|| (key, entry.path.clone()))
        })
        .collect()
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

/// Build the audit view for a window. Shared by the app and the screenshot harness
/// so that what gets captured is the thing that ships.
pub(crate) fn build_audit(
    launch: Launch,
    window: &mut Window,
    cx: &mut App,
) -> gpui::Entity<Audit> {
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
