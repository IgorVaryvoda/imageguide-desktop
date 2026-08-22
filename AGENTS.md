# Repository Guidelines

## Project Overview

ImageGuide Desktop audits a local folder of images and re-encodes it to WebP/AVIF without uploading anything. It is the desktop companion to imageguide.dev and its Chrome extension: those audit pages and stop there; this one rewrites the files. Three views share one window: the audit list (or gallery), a 1:1 before/after comparison, and an empty state. A headless `--convert` mode does the same work with no window.

## Architecture & Data Flow

Single crate, one binary (`imageguide`). UI is [GPUI](https://www.gpui.rs) with [gpui-component](https://github.com/longbridge/gpui-component) widgets.

```
scan::scan(root) ──► Launch ──► build_audit ──► Entity<Audit> (all UI state)
                                    │
     ┌──────────────────────────────┼──────────────────────────┐
     ▼                              ▼                          ▼
thumbs::load (per visible row)  compare::build (per view)  convert::convert_file
  Arc<RenderImage>                Pair (in-memory)           writes root/optimized/
```

- `src/scan.rs` — header-only folder walk. Never decodes to learn dimensions. `Entry` carries `path/format/width/height/bytes`; `extension_lies()` flags files whose magic bytes disagree with the extension; output goes to `OUTPUT_DIR = "optimized"`, which the walk skips.
- `src/convert.rs` — re-encode. WebP via libwebp (real transparency forces lossless), AVIF via system libavif/libaom speed 6 with libyuv conversion where packaged (no lossless AVIF). `MaxEdge` downscales with Lanczos3, never up. `output_path` mirrors the source tree.
- `src/compare.rs` — original-vs-converted pair built in memory, decode-encode-decode so both sides are real pixels. Cached by `Key` (path+format+quality+max_edge).
- `src/thumbs.rs` — decode + 96px thumbnail + BGRA swap (`to_bgra`, shared with compare). `None` means draw a gap, not an error.
- `src/settings.rs` — hand-rolled `key=value` file at `<config>/imageguide/settings`; tolerant parse.
- `src/main.rs` — everything else: `main` (line ~3090), `parse_args`, `convert_headless`, `run_window`/`build_audit`/`init_theme`/`Launch`, and the whole `Audit` UI (struct at ~254, `Render` impl at ~2593, `AuditTable` delegate at ~2286).

Key patterns an editor must respect:

- **Indices, not moves.** `Audit::entries` is never reordered. `visible: Vec<usize>` holds filtered+sorted indices; thumbs, ticks and results are keyed by entry index so they survive re-sorting.
- **Heavy work off the main thread.** Scans, thumbnails, encodes, compare builds and estimates all run on `cx.background_executor().spawn(...)` inside `cx.spawn(async move |this, cx| { ...; this.update(cx, ...) }).detach()`.
- **Generation counters invalidate stale async work.** `dataset_generation`, `scan_generation`, `estimate_generation` on `Audit`. Check the generation you captured before applying a result; a slider drag supersedes an in-flight estimate.
- **`WORKERS = 8` bounds conversion** — each in-flight file holds a fully decoded image, so the constant is a memory bound.
- **Thumbnails are viewport-driven.** `render_td`/gallery bands call `request_thumb`; `requested: HashSet` dedupes; never decode eagerly.
- **Theme comes from `cx.theme()`**, set once in `init_theme` (~3298). Colour set and `theme.tokens.button_primary*` must agree or buttons render black-on-blue. Fonts: SF Pro Text / SF Pro Display, mono Fira Code.
- **Checkbox keyboard ownership**: unmodified Space/Enter must stop at a wrapper `on_key_down` (`is_checkbox_activation_key`), or the component toggles and the root cursor handler toggles again.
- **`TableState` caches column groups**: after a viewport/result-signature change, update the delegate and call `TableState::refresh` from `cx.defer`, never during `Audit::render`.

## Key Directories

| Path | Purpose |
|---|---|
| `src/` | All source. `main.rs` plus five focused modules. |
| `docs/` | README screenshots (`audit.webp`, `comparison.webp`) — compressed by the tool itself. |
| `plans/` | Numbered executor briefs (`NNN-slug.md`) from improve-style audits, indexed by `plans/README.md`. Statuses: DONE / REJECTED / etc. Read the relevant plan before touching its area. |
| `.github/workflows/` | CI: build/test/clippy/fmt on ubuntu + macos. |

## Development Commands

```bash
cargo build --release        # needs dav1d and libavif/libaom; Linux also packages libyuv
cargo test --locked          # 48 tests; screenshot test stays ignored
cargo clippy --all-targets -- -D warnings
cargo fmt --check
cargo run --release -- ~/path/to/folder            # audit window
cargo run --release -- ~/folder --convert --avif   # headless convert
cargo test --bin imageguide -- --ignored --nocapture screenshot   # known-broken on Linux (no HeadlessRenderer); prove UI with the real app instead
```

CI runs the same gates with `--locked` (see `.github/workflows/ci.yml`).

## Code Conventions & Common Patterns

- Rust 2024, `cargo fmt` canonical; clippy warnings are errors in CI.
- Comments explain *why*, often as a short paragraph over the item they justify. Match that voice; do not add narration comments.
- Test names are snake_case sentences stating behaviour: `ties_fall_back_to_the_filename`, `a_file_decodes_by_its_contents_not_its_name`.
- No config/argument crates: `parse_args` and `settings.rs` are hand-rolled and tolerant of junk input.
- Errors are named, not counted: failures keep filenames (`Audit::failures: Vec<String>`); "N failed" alone is not a report.
- Truthful UI: a file that grew is reported as grown; stale results are dropped, not shown next to a new folder.
- Commit subjects: `fix: ...` / conventional, one concern per commit.

## Important Files

- `src/main.rs` — entry point, window lifecycle, theme, entire Audit UI, headless convert, all tests.
- `src/scan.rs` — `Scan`/`Entry`, header-only walk, `OUTPUT_DIR`.
- `src/convert.rs` — `Format`/`Quality`/`MaxEdge`, `convert_file`.
- `src/compare.rs` — `Key`/`Pair`, `build`.
- `Cargo.toml` — git-pin policy comment; read before adding dependencies.
- `Cargo.lock` — the actual version pin for gpui and gpui-component.
- `rust-toolchain.toml` — pinned 1.97.1; the locked zed commit does not build on other compilers.
- `.Codex/napkin.md` — session lessons: host-specific gotchas and patterns that work (Hyprland/ydotool/grim for UI proof).

## Runtime/Tooling Preferences

- Rust toolchain is pinned; use the pinned toolchain, not a system default.
- gpui/gpui_platform (zed) and gpui-component(+assets) are unpinned git deps on purpose: pinning a revision in Cargo.toml would give cargo two git sources for gpui and two incompatible copies of every type. `Cargo.lock` pins commits; CI builds `--locked`. Do not "fix" this by adding revs.
- Linux build uses rfd `xdg-portal` (no GTK) and gpui_platform `wayland,x11,font-kit`; other targets use defaults. AVIF encoding links system libavif >= 1.0 with its libaom backend and libyuv where packaged.
- `gpui-component-assets` must be registered as the asset source or every `IconName` renders blank.
- Only Linux is tested; macOS/Windows builds are believed-correct, not verified. Windows is blocked on dav1d via vcpkg.

## Testing & QA

- Framework: built-in `cargo test`; UI tests use `#[gpui::test]` with `TestAppContext`/`VisualTestContext` (`cx.update(init_theme)`, `cx.add_window_view(...)`, `simulate_click`, `debug_bounds(selector)`).
- Coverage: table/gallery layout thresholds, sort stability (ties fall back to filename), checkbox pointer/keyboard ownership, conversion round-trips, AVIF alpha preservation, scan rules, settings round-trip.
- Helpers: `entry(name,w,h,bytes,format)`, `pointer_checkbox_audit(grid, cx)`, `photo(w,h)` (deterministic noise — flat colours compress to nothing and make assertions false).
- The ignored screenshot test fails on this Linux host (`render_to_image not available`). For visual proof, launch the real release binary and capture it through Hyprland/ydotool/grim instead of fixing the harness incidentally.
- Baseline gate before landing: `cargo test --locked`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` all green.
