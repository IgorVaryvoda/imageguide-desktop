//! ImageGuide Desktop — audit a folder of images without uploading them anywhere.
//!
//! The browser tools on imageguide.dev post files to a worker to convert them. This
//! does the same work locally, so nothing leaves the machine and the folder size is
//! bounded by the disk rather than by a tab.

mod audit;
mod avif;
mod compare;
mod convert;
mod scan;
mod settings;
mod sirv;
mod thumbs;
#[cfg(feature = "updater")]
mod update;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use convert::{Format, MaxEdge, Quality};
use gpui::{App, Bounds, WindowBounds, WindowOptions, prelude::*, px, size};
use gpui_component::{ActiveTheme, Root};
use gpui_platform::application;
use scan::{Entry, format_bytes};

/// The smallest compositor window that supports every production view.
const WINDOW_MIN_WIDTH: f32 = 760.;
const WINDOW_MIN_HEIGHT: f32 = 560.;
const WINDOW_DEFAULT_WIDTH: f32 = 900.;
const WINDOW_DEFAULT_HEIGHT: f32 = 640.;

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
    #[cfg(feature = "updater")]
    update::install_if_available();

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
                    let audit = audit::build_audit(launch, window, cx);
                    // Dialogs, notifications and tooltips are drawn by the Root, so
                    // the window's first level has to be one.
                    cx.new(|cx| Root::new(audit, window, cx).bg(cx.theme().background))
                },
            )
            .unwrap();
            cx.activate(true);
        });
}
