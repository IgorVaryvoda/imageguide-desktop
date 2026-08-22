# Plan 011: Enforce one supported minimum window size

> Execute in an isolated worktree. Run every command. Reviewer owns the index.
> Drift: `git diff --stat 05384d3..HEAD -- src/main.rs`; stop if window restoration changed.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: UI
- **Planned at**: `05384d3`, 2026-08-20

## Why this matters

Persisted dimensions are restored without limits and the native window declares no
minimum, so the pre-alpha interface can reopen smaller than its controls.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --locked` | pass, screenshot ignored |
| Lint | `cargo clippy --all-targets -- -D warnings` | pass |
| Format | `cargo fmt --check` | pass |
| Build | `cargo build --release --locked` | pass |

## Scope

**In scope**: `src/main.rs`. **Out**: controls, table, gallery, settings format.

## Git workflow

Branch `improve/011-012-compact-ui`; commit only `src/main.rs`; no push/index edit.

## Steps

1. Define width 760px and height 560px constants. A pure restore helper treats
   missing, NaN, positive infinity, and negative infinity as the 900×640 defaults,
   then clamps finite dimensions before `Bounds::centered`. Set
   `WindowOptions::window_min_size` from the same minimum constants.
2. Pure tests cover missing and every non-finite value, below-minimum, exactly
   minimum, one dimension below, normal default, and larger values.

## Done criteria

- [ ] Restore clamp and native hint share constants; tests/gates pass.
- [ ] Main reviewer opens a populated audit with selection and completed results (the
  widest conditional summary controls), attempts a 600×400 compositor resize, proves
  actual bounds remain at least 760×560, and captures that real release window with
  all controls reachable.

## STOP conditions

- Real compositor cannot maintain 760×560 or controls remain unusable there.
