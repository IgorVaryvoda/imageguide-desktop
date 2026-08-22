# Plan 015: Enforce a usable minimum and reflow the gallery

> Execute in an isolated worktree. Run every command. Reviewer owns the index.
> Drift: `git diff --stat 05384d3..HEAD -- src/main.rs`.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: UI
- **Planned at**: `05384d3`, 2026-08-20

## Why this matters

The app can restore arbitrarily small windows, while grid always renders five 168px
tiles and clips in compact windows. The minimum and responsive gallery must land
together so every view is usable at the declared size. Pinned GPUI does not expose a
reliable production top-band index, so preserving a deep logical image through reflow
is not a sound promise.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --locked` | pass, screenshot ignored |
| Lint | `cargo clippy --all-targets -- -D warnings` | pass |
| Format | `cargo fmt --check` | pass |
| Build | `cargo build --release --locked` | pass |

## Scope

**In scope**: `src/main.rs`. **Out**: table, fluid tiles, scroll-position behavior,
dependencies.

## Git workflow

Branch `improve/011-012-compact-ui`; focused `src/main.rs` commit; no push/index edit.

## Steps

1. Define width 760px and height 560px constants. A pure restore helper treats missing,
   NaN, positive infinity, and negative infinity as the 900x640 defaults, then clamps
   finite dimensions before `Bounds::centered`. Set `WindowOptions::window_min_size`
   from the same minimum constants.
2. Replace fixed-five production math with one O(1) gallery layout helper returning
   columns and row count, plus lazy `band_range(band, entry_count)`. Start from
   `window.viewport_size().width`, subtract the dynamic left/right values from the
   exported `gpui_component::window_paddings(window)` for Root's Linux CSD wrapper,
   then subtract both sides of outer `p_3` and `border_2`, gallery `border_1` and
   `p_2`. Fit 168px tiles with 8px gaps, clamped 1–5. Keep the pure helper explicit
   about the supplied Root insets so tests cover both server/tiled zero insets and
   floating Linux 20px-per-side insets.
3. Pure tests cover missing/non-finite/restored sizes and assert columns at 760, 873,
   900, 1100 and both sides of every threshold
   for zero and floating-Linux Root insets, including all other border/padding
   deductions. Lazy ranges cover 1/3/5 columns without gaps or duplicates.

## Done criteria

- [ ] Restore clamp and native hint share 760x560 constants.
- [ ] 1–5 columns fit exact interior width with no clipped tile at that minimum.
- [ ] Fixed tile geometry and `uniform_list` virtualization stay intact.
- [ ] Gates pass; main reviewer captures grid at 760x560, 873x720, 900x640, and
      1100x720 with no clipped tiles or controls.

## STOP conditions

- The real compositor cannot maintain 760x560 or still clips a tile/control there.
