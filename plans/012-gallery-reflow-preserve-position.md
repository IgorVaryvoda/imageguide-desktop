# Plan 012: Reflow gallery columns without jumping the visible images

> Execute after approved plan 011 in the same worktree. Run every command. Reviewer
> owns the index. Drift: `git diff --stat 05384d3..HEAD -- src/main.rs`.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: plan 011
- **Category**: UI
- **Planned at**: `05384d3`, 2026-08-20

## Why this matters

Grid always renders five 168px tiles and clips in compact windows. Changing column
count without handling scroll makes a resize jump hundreds of images in large folders.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --locked` | pass, screenshot ignored |
| Lint | `cargo clippy --all-targets -- -D warnings` | pass |
| Format | `cargo fmt --check` | pass |
| Build | `cargo build --release --locked` | pass |

## Scope

**In scope**: `src/main.rs`. **Out**: table, fluid tiles, dependencies.

## Git workflow

Continue `improve/011-012-compact-ui`; focused `src/main.rs` commit; no push/index edit.

## Steps

1. Add an O(1) gallery layout helper returning columns and row count, plus lazy
   `band_range(band, entry_count)`. Usable width subtracts outer `p_3` and `border_2`,
   gallery `border_1` and `p_2`, then fits 168px tiles with 8px gaps, clamped 1–5.
   Use it for all production band math; leave no fixed-five arithmetic.
2. Store a `UniformListScrollHandle` and last column count in `Audit`. Read the
   production scroll top through the pinned crate's public state,
   `handle.0.borrow().base_handle.logical_scroll_top().0`; encapsulate that expression
   in one local helper so the GPUI reach-through is isolated. Do not use the
   test-feature-only `logical_scroll_top_index`, and do not record the processor range
   because the same processor is also invoked with a one-item measurement range. When
   columns change, derive the old top entry (`first_visible_band * old_columns`) and use
   `scroll_to_item_strict(top_entry / new_columns, ScrollStrategy::Top)` so even a
   newly mapped band that is already visible becomes the actual top band. Do not
   reset when the count is unchanged.
3. Tests assert columns at 760, 873, 900, 1100 and both sides of thresholds including
   the gallery border; lazy ranges cover 1/3/5 columns without gaps/duplicates. A
   stateful resize test starts at a deep band, resizes both directions, and proves the
   old top entry remains in the first visible new band. Include a nearby-visible case
   (for example five to four columns at old band four) that fails with non-strict
   scrolling, and compile the production path in the release build.

## Done criteria

- [ ] 1–5 columns fit exact interior width with no clipped tile.
- [ ] Resize preserves the top logical image; virtualization stays intact.
- [ ] Gates pass; main reviewer captures grid at 760×560, 873×720, 900×640, 1100×720.

## STOP conditions

- `UniformListScrollHandle` cannot expose/set the needed logical position.
