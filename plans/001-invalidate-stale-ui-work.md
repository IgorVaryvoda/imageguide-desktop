# Plan 001: Invalidate stale async UI work when the dataset changes

> **Executor instructions**: Follow this plan step by step. Run every verification
> command. If a STOP condition occurs, stop and report; do not improvise. The
> reviewer maintains `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 05384d3..HEAD -- src/main.rs`
> If `src/main.rs` changed, compare the symbols and excerpts below with live code.
> Stop if the async completion paths or `open_path` have materially changed.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `05384d3`, 2026-08-20

## Why this matters

`Audit` launches detached work for estimates, thumbnails, comparisons, and
conversion. Those callbacks identify rows by numeric index. Opening another path
reuses the same indices without invalidating pending work, so data from folder A can
appear on folder B. Conversion controls also remain mutable during a run, so live
labels and progress can stop describing the captured job.

## Current state

- `src/main.rs:170-249` owns all UI state, including `estimate_generation` but no
  generation for the active folder/dataset.
- `src/main.rs:378-395` inserts conversion results into current rows by index.
- `src/main.rs:497-539` rejects stale estimate settings generations, but
  `open_path` does not advance that generation.
- `src/main.rs:909-919` accepts a comparison result when only the numeric index
  matches. `compare::Key` is checked for cache reuse before spawning, not when a
  completion publishes.
- `src/main.rs:1640-1644` inserts a thumbnail by numeric index with no path or
  dataset check.
- `src/main.rs:767-786` replaces entries and clears some caches, but leaves the
  current estimate, estimate generation, selection anchor, and filter input entity
  unchanged.
- Existing convention: async tasks capture immutable values and reject stale work
  before mutating `Audit`; follow the estimate check at `src/main.rs:497-503`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test --locked` | 36 existing plus new tests pass; screenshot remains ignored |
| Lint | `cargo clippy --all-targets -- -D warnings` | exit 0, no warnings |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs`

**Out of scope**:
- `src/scan.rs`, `src/convert.rs`, `src/compare.rs`, `src/thumbs.rs`
- Changing output naming, encoders, scan rules, or file formats
- Cross-platform screenshot infrastructure
- Moving folder scanning off-thread; that is plan 002

## Git workflow

- Branch: `improve/001-002-ui-state`
- Worktree: sibling isolated worktree chosen by the dispatcher
- Use one focused imperative commit, matching `Look at the window instead of guessing at it`.
- Do not push, merge, or edit `plans/README.md`.

## Steps

### Step 1: Add one dataset generation

Add one monotonically increasing `u64` field to `Audit`. Advance it whenever a new
file or folder is accepted, before old derived state can publish again. Capture the
generation in estimate, thumbnail, and comparison tasks. Each completion must check
the captured generation before it changes `Audit`.

For comparison, dataset generation is necessary but not sufficient: two requests for
the same row can use different format/quality/size settings. Store the active
`compare::Key` with the open `Comparison`, and publish only when both dataset
generation and the full active key match the request that completed. Do not accept a
same-index result by index alone.

Do not add a generic job framework, cancellation trait, event bus, or per-feature
token type. One dataset counter is the shared root-cause guard.

**Verify**: `rg -n "dataset_generation" src/main.rs` -> the field, path transition,
and all three read-job completion paths are present.

### Step 2: Make path replacement a complete state transition

When a new dataset is installed:

- clear `estimate` and advance `estimate_generation`;
- clear thumbnails, requested rows, selection, results, failures, comparison, and
  comparison cache as today;
- reset `cursor` and `anchor`;
- clear both the plain filter string and the `InputState` value using the pinned
  component API (`InputState::set_value` needs a `Window` and context);
- rebuild `visible`, schedule a fresh estimate, and then open comparison for a
  single file.

Adjust `open_path` and its picker/drop callers only as needed to provide the
`Window`. Use GPUI's existing `update_in` path for async picker completion instead of
inventing a window registry.

**Verify**: add a focused GPUI test that starts with a non-empty filter and estimate,
replaces the dataset, and asserts the filter value, estimate, anchor, and visible
rows describe only the new dataset.

### Step 3: Freeze state that defines an active conversion

The smallest safe rule is that a conversion run owns its dataset, targets, format,
quality, and size until it finishes. While `converting` is true:

- disable format, resize, quality, filter, selection, open-folder, and open-image
  controls using existing component `disabled` APIs;
- reject drop/path replacement and non-double-click selection mutation defensively;
- keep comparison viewing and list/grid switching available;
- capture the dataset generation with conversion callbacks and ignore a completion
  if the generation is no longer current.

The guards are required even when a control looks disabled because drag/drop and
programmatic callbacks are separate entry paths. Do not build cancellation of an
encode already writing to disk.

**Verify**: add one interaction/state test showing a blocked mutation cannot change
the active conversion's target count or settings, and one stale-callback test showing
an old generation cannot add a result to the current dataset.

## Test plan

- Add the smallest test helpers inside the existing `#[cfg(test)] mod tests` in
  `src/main.rs`; do not create a framework or fixture module.
- Cover: complete dataset reset, stale thumbnail/estimate/comparison publication,
  conversion-state mutation guards, and out-of-order same-row comparison requests
  with different keys.
- Every async-result assertion must first assert that the test actually reached the
  callback seam; an empty/no-op test is not acceptable.

## Done criteria

- [ ] Dataset replacement invalidates every estimate, thumbnail, comparison, and
  conversion completion that captured an older dataset.
- [ ] A same-dataset comparison completion cannot overwrite a newer request for the
  same row when its format, quality, or size key differs.
- [ ] Filter text and filter state are both empty after a path switch.
- [ ] A fresh estimate is scheduled for the new path.
- [ ] Conversion-defining controls and drop mutation are inert during conversion.
- [ ] `cargo test --locked` passes with the ignored screenshot still ignored.
- [ ] `cargo clippy --all-targets -- -D warnings` passes.
- [ ] `cargo fmt --check` passes.
- [ ] `git status --porcelain` is empty after committing, and only `src/main.rs`
  differs from the base.
- [ ] The main reviewer captured and inspected the real app with an active
  conversion, proving disabled controls remain legible and comparison/list switching
  still works.

## STOP conditions

- The pinned components do not expose disabled input/button/slider APIs needed for
  the lock rule.
- Clearing `InputState` requires changes outside `src/main.rs` or a component fork.
- Correctness requires canceling or deleting files already written by a conversion.
- A focused test cannot observe the state transition without a broad UI rewrite.

## Maintenance notes

Every future detached task that publishes dataset-derived state must capture and
check the same generation. Reviewers should reject numeric-index-only callbacks.
