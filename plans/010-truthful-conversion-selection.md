# Plan 010: Make checkbox and filter conversion scope truthful

> **Executor instructions**: Follow this plan in its isolated worktree. Run every
> command. The reviewer maintains `plans/README.md`.
>
> **Drift check**: `git diff --stat 05384d3..HEAD -- src/main.rs`; stop if targets,
> selection callbacks, or conversion progress changed.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: `05384d3`, 2026-08-20

## Why this matters

The real grid checkbox toggles twice because its click bubbles into the tile. Hidden
selected rows also bypass the visible filter, and conversion progress recomputes its
denominator from mutable live selection.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test --locked` | existing and new selection tests pass |
| Lint | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**: `src/main.rs`

**Out of scope**: arrow-key/table cursor redesign, conversion control locking,
dataset generation, encoders, new selection modes.

## Git workflow

- Branch: `improve/010-truthful-selection`.
- Commit only `src/main.rs`; do not push or edit `plans/README.md`. The executor
  worktree starts clean and must finish clean.

## Steps

1. `targets()` returns visible rows in visible order when nothing is selected, or
   the visible/selected intersection when selection exists. Conversion labels use
   its length; `Clear N` keeps total `selected.len()`. If targets are empty,
   `start_conversion` returns before mutating any state and Convert uses the pinned
   button's real `.disabled(true)` semantics, not only ghost styling.
2. Snapshot the target count when conversion begins and use it as progress denominator
   until completion. Tests mutate live filter/selection mid-run and prove progress
   still describes the captured job.
3. In grid and table checkbox callbacks, call `cx.stop_propagation()` before the one
   selection mutation. Wrap each checkbox with a key-down listener that stops
   propagation for Space and Enter after the focused checkbox has recorded the key;
   its synthesized key-up click then owns the one activation. Leave the root handler
   and table focus ownership unchanged for all other keys.
4. Pointer, focused-Space, and focused-Enter GPUI tests prove a checkbox changes
   exactly once and preserves an existing multi-selection. Verify the empty-target
   Convert control is semantically disabled and not focusable/clickable. Keep Space
   estimate refresh when root selection actually changes.

## Done criteria

- [ ] Hidden selected rows are not conversion targets.
- [ ] Convert count and progress equal the actual captured job; Clear count remains
  total persistent selection.
- [ ] Empty-target activation changes no state.
- [ ] Pointer, focused-Space, and focused-Enter checkbox activation toggle exactly once.
- [ ] Empty-target Convert is semantically disabled, not merely a no-op callback.
- [ ] Tests, clippy, and format pass; only `src/main.rs` changed.
- [ ] Main reviewer captures list and grid before/after checking and unchecking a box.

## STOP conditions

- Checkbox focus cannot be distinguished from root focus in the pinned GPUI API.
- Correct target scoping requires deleting hidden selection.

## Maintenance notes

`visible` is display/conversion order, `selected` is persistent membership, and the
captured target count owns progress for one run.
