# Plan 006: Invalidate stale async work when the dataset changes

> **Executor instructions**: Follow this plan in its isolated worktree. Run every
> verification command. The reviewer maintains `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 05384d3..HEAD -- src/main.rs`
> Stop if the estimate, thumbnail, comparison, or path-replacement seams changed.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `05384d3`, 2026-08-20

## Why this matters

Detached estimate, thumbnail, and comparison callbacks identify rows by numeric
index. Opening another path reuses those indices, so old data can appear on a new
folder. The fix is one dataset identity checked at every publication seam.

## Current state

- `src/main.rs:170-249` has an estimate generation but no dataset generation.
- `src/main.rs:497-539`, `909-919`, and `1640-1644` publish detached results.
- Comparison completion checks only the row index; its cache key is checked before
  spawning, not when publishing.
- `src/main.rs:767-786` replaces entries but does not fully reset filter, estimate,
  cursor, or anchor state.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test --locked` | existing and new tests pass; screenshot stays ignored |
| Lint | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs`

**Out of scope**:
- Background folder scanning
- Conversion-control locking; that is plan 007
- Renderer or screenshot infrastructure
- Encoder, scan, thumbnail, or comparison modules

## Git workflow

- Branch: `improve/006-007-ui-state`
- Use one focused imperative commit. Stage only `src/main.rs`; do not push, merge,
  or edit `plans/README.md`.

## Steps

### Step 1: Guard dataset-derived publications

Add one monotonically increasing `u64` dataset generation. Advance it before a new
accepted file or folder replaces the current dataset. Capture it in estimate,
thumbnail, comparison, and conversion tasks; reject completions whose generation is
not current.

For comparison, also store the active `compare::Key` with the open `Comparison` and
publish only when both generation and the full key match. This prevents an older
same-row request with different format, quality, or size from winning.

Do not add a generic job framework, cancellation type, or dependency.

**Verify**: tests exercise every publication seam and an out-of-order same-row,
different-key comparison pair. Each test must prove its callback seam was reached.

### Step 2: Make dataset replacement complete

On accepted path replacement, clear the estimate and advance its generation; clear
thumbnails, requested rows, selection, results, failures, comparison, and comparison
cache; reset cursor and anchor; reset the retained table viewport to row zero; clear
both filter text and `InputState` with its pinned `set_value` API; rebuild visible
rows and schedule a fresh estimate. Preserve single-file comparison opening.

Path replacement also ends the old run's visible converting state. Conversion
completion must clear `converting` only when its captured dataset generation is still
current. Therefore replacement sets `converting = false`; an old final callback can
neither leave the new dataset stuck nor clear a newer conversion started on it.

Adjust picker/drop callers only as needed to provide `Window`; use GPUI's existing
`update_in` path for async picker completion.

**Verify**: a focused GPUI test starts with filter, estimate, anchor, a nonzero table
viewport, and derived state, replaces the dataset, and proves all state belongs only
to the new dataset at row zero. Separate ordering tests cover replacement during a
run and an old final callback arriving after a new run starts.

## Test plan

- Keep the smallest helpers in the existing `src/main.rs` test module.
- Cover stale estimate, thumbnail, comparison, and conversion results; full path
  reset; and same-row different-key comparison ordering.
- Do not create a fixture framework.

## Done criteria

- [ ] Dataset replacement invalidates every old detached publication.
- [ ] Same-row comparison requests require full active-key equality.
- [ ] Filter entity and text, estimate, selection anchor, cursor, and caches reset.
- [ ] Table viewport resets to row zero and stale conversion finalization cannot
  leave converting stuck or clear a newer run.
- [ ] A fresh estimate is scheduled for the new path.
- [ ] Tests, clippy, and format pass.
- [ ] Only `src/main.rs` differs and the executor worktree is clean after commit.
- [ ] The main reviewer opens a second path in the real app and captures the fresh
  list/summary state.

## STOP conditions

- Clearing `InputState` requires changes outside `src/main.rs`.
- A publication seam cannot carry a generation without changing another module.
- A focused test requires a broad UI harness rewrite.

## Maintenance notes

Every future detached task that publishes dataset-derived state must capture and
check this generation. Numeric row identity alone is never sufficient.
