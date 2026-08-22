# Plan 017: Make the supported window and gallery agree

> Execute in an isolated worktree. Run every command. Reviewer owns the index.
> Drift: compare `src/main.rs` against `05384d3` including committed and working-tree
> changes; stop if window restore or gallery band math already changed.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: UI
- **Planned at**: `05384d3`, 2026-08-20

## Why this matters

The app can restore an unusably small window and the five-column grid clips. These
must land atomically so the declared minimum supports every view.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --locked` | pass, screenshot ignored |
| Lint | `cargo clippy --all-targets -- -D warnings` | pass |
| Format | `cargo fmt --check` | pass |
| Build | `cargo build --release --locked` | pass |

## Scope

**In scope**: `src/main.rs`. **Out**: table column redesign, fluid tiles,
dependencies.

## Git workflow

Branch `improve/011-012-compact-ui`; focused `src/main.rs` commit; no push/index edit.

## Steps

1. Define one 760x560 supported minimum. A pure restore helper treats missing, NaN,
   positive infinity, and negative infinity as 900x640 defaults, then clamps finite
   dimensions before `Bounds::centered`. Use the same constants for
   `WindowOptions::window_min_size`.
2. Replace fixed-five production math with an O(1) gallery helper returning columns,
   row count, and lazy band ranges. Start from `window.viewport_size().width`. Deduct
   Root chrome exactly: `gpui_component::window_paddings(window)` plus the pinned
   Root's 1px border on each client-decorated edge whose tiling flag is false. Then
   deduct the audit's two `p_3` and `border_2` edges and the gallery's two `p_2` and
   `border_1` edges. Fit 168px border-box tiles with 8px gaps, clamped 1–5. Leave no
   fixed-five production band arithmetic.
3. Track the gallery with one `UniformListScrollHandle` and store the last column count.
   On initial layout or an unchanged count, do nothing. On a real count change, call
   `scroll_to_item_strict(0, ScrollStrategy::Top)`. This deterministic reset needs no
   unreliable top-band read.
4. Pure tests cover restore edge cases; zero, full-floating 21px-per-side, and
   asymmetric 0/21 Root insets; both sides of every reachable column threshold; widths
   760, 873, 900, 1100; and 1/3/5-column band ranges without gaps or duplicates.
5. Route the existing render-time settings write through one private helper that calls
   `settings::save` only under `#[cfg(not(test))]` and always updates the cached
   `self.settings`. Production behavior is unchanged, while any GPUI test can render
   Audit without writing the user's configuration; do not mutate HOME/XDG environment
   variables or add a persistence abstraction.
6. A GPUI test renders enough production gallery entries to scroll deeply. It crosses
   a column threshold, draws, and asserts the tracked base offset is zero. It scrolls
   deeply again, resizes within the same column band, draws, and asserts the offset
   stays non-zero. The test must fail if `.track_scroll` or the transition call is removed.

## Done criteria

- [ ] Native and restored minima share 760x560 constants.
- [ ] Every tile fits at supported widths; virtualization remains.
- [ ] Column changes reset to the first image; same-column resizes do not reset.
- [ ] Gates pass. Main reviewer attempts 600x400 and captures list/grid at the enforced
      minimum plus grid at 873x720, 900x640, and 1100x720. A deep-scrolled resize pair
      visibly proves the reset.

## STOP conditions

- The compositor does not enforce 760x560, controls or tiles clip there, or the GPUI
  integration test cannot drive the production gallery path.
