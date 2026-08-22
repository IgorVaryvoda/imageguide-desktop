# Plan 009: Make the gallery and window usable at compact size

> **Executor instructions**: Follow this plan in its isolated worktree. Run every
> command. The reviewer maintains `plans/README.md`.
>
> **Drift check**: `git diff --stat 05384d3..HEAD -- src/main.rs`; stop if gallery
> band math or window restoration changed.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: UI
- **Planned at**: `05384d3`, 2026-08-20

## Why this matters

Grid mode always renders five fixed tiles, so compact windows clip the gallery. The
app also restores arbitrarily small persisted dimensions and declares no native
minimum.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test --locked` | existing and new layout tests pass |
| Lint | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |
| Build | `cargo build --release --locked` | exit 0 |

## Scope

**In scope**: `src/main.rs`

**Out of scope**: table columns, fluid tile size, gallery scroll reset, dependencies.

## Git workflow

- Branch: `improve/009-responsive-gallery`.
- Commit only `src/main.rs`; do not push or edit `plans/README.md`. The executor
  worktree starts clean and must finish clean.

## Steps

1. Define one supported minimum of 760×560. Clamp both restored dimensions with the
   same constants and set `WindowOptions::window_min_size`. Keep the 900×640 default.
2. Add one pure gallery-layout helper from exact viewport width and entry count. Its
   usable width must subtract the existing outer `p_3` plus `border_2` on both sides
   and gallery `p_2` on both sides, then account for the existing 168px tile and 8px
   gap. Return the clamped 1–5 column count plus the exact band ranges. Use that one
   result for row count, first index, and last index; leave no production
   `TILE_COLUMNS` band arithmetic behind. Keep fixed tile size, height, and
   `uniform_list` virtualization.
3. Pure tests cover restored values below/above each minimum; exact gallery columns
   at 760, 873, 900, and 1100; both sides of every column threshold reachable at or
   above the supported minimum; and band ranges for 1, 3, and 5 columns, including
   incomplete final rows with no missing or duplicate visible indices. The unchanged
   900px default must resolve to the actual fitting count rather than five clipped
   tiles.

## Done criteria

- [ ] Restored and native minimum size share 760×560 constants.
- [ ] Gallery adapts 1–5 columns with stable tile geometry and virtualization.
- [ ] Tests, clippy, format, and release build pass; only `src/main.rs` changed.
- [ ] Main reviewer captures grid mode at 760×560, 873×720, and 1100×720 with no
  clipped tiles or controls.

## STOP conditions

- Controls cannot fit 760×560 without a broader toolbar redesign.
- The compositor ignores minimum size and content remains unusable at 760×560.

## Maintenance notes

Gallery column count is a render-time viewport decision; fixed tile geometry keeps
virtualization boring.
