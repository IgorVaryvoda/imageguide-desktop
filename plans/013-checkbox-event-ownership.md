# Plan 013: Give checkbox activation one owner

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

The live grid checkbox cannot be unchecked because its click also activates the tile.
Focused Space/Enter can also reach the root before the checkbox click is synthesized.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --locked` | pass, screenshot ignored |
| Lint | `cargo clippy --all-targets -- -D warnings` | pass |
| Format | `cargo fmt --check` | pass |

## Scope

**In scope**: `src/main.rs`. **Out**: target filtering, cursor/navigation redesign.

## Git workflow

Branch `improve/013-checkbox-events`; commit only `src/main.rs`; no push/index edit.

## Steps

1. Grid and table checkbox click callbacks call `cx.stop_propagation()` before exactly
   one selection mutation and schedule the estimate once.
2. Wrap each checkbox with a key-down listener that stops propagation for Space and
   Enter only when the modifiers match the pinned checkbox activation predicate
   (unmodified key). The checkbox's synthesized key-up click remains the single owner.
   Modified Space/Enter keep propagating exactly as before; do not change root/table
   behavior otherwise.
3. GPUI tests run pointer, unmodified Space, and unmodified Enter against both grid
   and table checkbox event trees. Each toggles once, preserves another selected row,
   does not activate the parent, and does not open comparison. Modified Space and
   Enter regression cases prove existing ancestor behavior is unchanged.

## Done criteria

- [ ] All six surface/input cases toggle exactly once; tests/gates pass.
- [ ] Main reviewer captures list and grid checked→unchecked in the real app.

## STOP conditions

- A wrapper cannot intercept bubbled key-down without blocking checkbox activation.
