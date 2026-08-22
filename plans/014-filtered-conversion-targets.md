# Plan 014: Make the visible filter authoritative for conversion targets

> Execute after approved plan 013 in the same worktree. Run every command. Reviewer
> owns the index. Drift: `git diff --stat 05384d3..HEAD -- src/main.rs`.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plan 013
- **Category**: bug
- **Planned at**: `05384d3`, 2026-08-20

## Why this matters

Selected rows hidden by the filename filter remain conversion targets, contradicting
the UI promise that narrowing the list narrows Convert.

## Commands you will need

| Purpose | Command | Expected |
|---|---|---|
| Tests | `cargo test --locked` | pass, screenshot ignored |
| Lint | `cargo clippy --all-targets -- -D warnings` | pass |
| Format | `cargo fmt --check` | pass |

## Scope

**In scope**: `src/main.rs`, including the numeric denominator owned by an active
conversion. **Out**: conversion locking, dataset generations, keyboard cursor, and
the direction/styling of the visual progress bar.

## Git workflow

Continue `improve/013-014-selection`; focused `src/main.rs` commit; no push/index edit.

## Steps

1. `targets()` is visible order when selection is empty and visible/selected
   intersection otherwise. Conversion labels use `targets.len()`; `Clear N` keeps
   total `selected.len()` because hidden membership persists.
2. If targets are empty, `start_conversion` returns before state changes and the
   Convert button uses real `.disabled(true)` semantics.
3. Capture the target count when a conversion starts and render active numeric
   progress from that captured count, never from live filter/selection state. The
   count belongs to the in-flight run: filter, selection, and `open_path` mutations
   do not recompute or clear it while `converting` is true; the run completion clears
   it, and the next run replaces it. Keep this as one `Option<usize>` field, with no
   run object or new abstraction.
4. Pure/state tests cover visible order, partial/empty intersection, total Clear count,
   exact Convert count, pre-mutation empty guard, disabled button semantics, and a
   mid-run filter/path mutation that leaves the captured denominator unchanged until
   completion and resets it for the next run.

## Done criteria

- [ ] Hidden rows never enter a newly started conversion.
- [ ] Counts describe their actual actions before and during a run; empty Convert is
      disabled; gates pass.
- [ ] Main reviewer filters a selected row out and captures the corrected Convert count.

## STOP conditions

- Correct scoping requires deleting hidden selection rather than intersecting it.
