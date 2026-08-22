# Plan 016: Keep pointer checkbox clicks inside the checkbox

> Execute in an isolated worktree. Run every command. Reviewer owns the index.
> Drift: `git diff --stat 05384d3..HEAD -- src/main.rs`; stop if checkbox trees changed.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: `05384d3`, 2026-08-20

## Why this matters

The live grid checkbox cannot be unchecked because its pointer click also activates
the parent tile, which immediately selects it again.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --locked` | pass, screenshot ignored |
| Lint | `cargo clippy --all-targets -- -D warnings` | pass |
| Format | `cargo fmt --check` | pass |

## Scope

**In scope**: `src/main.rs`, pointer activation in grid and table. **Out**: keyboard
event ownership, target filtering, cursor/navigation redesign.

## Git workflow

Branch `improve/016-checkbox-pointer`; commit only `src/main.rs`; no push/index edit.

## Steps

1. In both grid and table checkbox callbacks, call `cx.stop_propagation()` before
   exactly one selection mutation, then schedule the estimate exactly once and notify.
   Leave parent row/tile click behavior unchanged.
2. GPUI pointer tests exercise the real grid and table checkbox event trees. Starting
   with two selected entries, each click removes only the clicked entry, does not
   activate the parent or open comparison, increments `estimate_generation` exactly
   once, and invalidates the existing estimate. A second click adds the entry once and
   again increments the generation once.

## Done criteria

- [ ] Grid and table pointer clicks check and uncheck exactly once.
- [ ] Every click refreshes the estimate exactly once; parent activation stays zero.
- [ ] Gates pass; main reviewer captures list and grid checked then unchecked.

## STOP conditions

- Stopping the checkbox click also prevents its own callback.
