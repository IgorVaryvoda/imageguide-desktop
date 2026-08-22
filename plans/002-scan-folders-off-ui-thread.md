# Plan 002: Scan newly opened folders off the UI thread

> **Executor instructions**: Execute only after plan 001 is approved in this same
> integration worktree. Run every command. The reviewer maintains the plan index.
>
> **Drift check (run first)**:
> `git diff --stat 05384d3..HEAD -- src/main.rs src/scan.rs`
> Plan 001 changes in `src/main.rs` are expected. Stop if `scan::scan`, the accepted
> dataset-generation rule, or the picker/drop entry paths changed for another reason.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: `plans/001-invalidate-stale-ui-work.md`
- **Category**: perf
- **Planned at**: commit `05384d3`, 2026-08-20

## Why this matters

The file dialog runs in the background, but the recursive folder walk runs after
returning to the UI entity. A large folder therefore freezes rendering and input
while every candidate image is opened and probed. Plan 001 provides the generation
guard needed to publish a late scan safely.

## Current state

- `src/main.rs:812-829` returns the picker result to `Audit::open_path`.
- `src/main.rs:746-765` synchronously calls `scan::scan` or `scan::probe`.
- `src/scan.rs:120-157` recursively walks the tree and probes every image candidate.
- Existing convention: CPU/file work uses `cx.background_executor().spawn`, as in
  thumbnail loading at `src/main.rs:1634-1638`.
- Expected dependency state: plan 001 added a dataset generation and full dataset
  installation/reset path. That generation identifies installed data. A pending
  scan request needs its own request identity because the old dataset remains
  installed until a scan succeeds.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test --locked` | all tests pass; screenshot remains ignored |
| Lint | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs`

**Out of scope**:
- Changing `scan::scan`, walking rules, formats, or symlink policy
- Progress percentages or parallel file probing
- New channels, worker pools, traits, or dependencies
- Any renderer/screenshot change

## Git workflow

- Continue branch `improve/001-002-ui-state` in its existing worktree.
- One focused imperative commit; do not push, merge, or edit `plans/README.md`.

## Steps

### Step 1: Separate scan request from dataset installation

Keep one method that starts opening a requested path and one method that installs a
completed `scan::Scan` plus root/single-file metadata. Add one scan-request counter
separate from plan 001's installed-dataset generation. The request method must:

- validate file versus directory;
- advance the scan-request counter without changing the installed-dataset
  generation;
- put the UI in an explicit scanning state and notify;
- run `scan::scan` or `scan::probe` on the background executor;
- publish only if its scan-request counter is still current.

Only a successful current scan installs data. Installation then advances plan 001's
dataset generation exactly once, which invalidates work started against the old
entries. An invalid current scan clears loading but keeps the old dataset and its
generation unchanged, so its thumbnails, estimate, and comparison remain valid. A
stale success or failure must not install data or clear a newer loading request.

Do not clear the working dataset twice. Do not duplicate the reset list from plan
001; the installation method remains the single source of truth.

**Verify**: `rg -n "scan::(scan|probe)" src/main.rs` -> interactive calls occur
inside background work; startup/headless calls outside `Audit` may remain synchronous.

### Step 2: Render one honest loading state

While a requested path is scanning, keep the window responsive and show concise copy
such as `Scanning <folder>…`. The user must still be able to request a newer path;
that newer generation wins and the older result is ignored. Do not add per-file
progress because the scanner does not expose it.

On an invalid or unprobeable selected file, clear loading and leave the prior dataset
intact with the same installed-dataset generation. Never leave `loading` stuck.

**Verify**: deterministic tests start two requests and complete them in reverse
order, then assert only the second result is installed. Also cover a current invalid
request and a stale failure while a newer request remains loading; the prior dataset
must remain usable and a stale failure must not clear the newer request.

### Step 3: Keep startup and headless execution boring

Do not make CLI startup or `--convert` async. They run before a window exists and are
not the UI freeze. Keep `main` and `convert_headless` behavior unchanged.

**Verify**: `cargo test --locked` -> all existing scan and conversion tests pass.

## Test plan

- Add focused GPUI/state tests in `src/main.rs` for out-of-order scan completion,
  current-request failure, stale failure, and loading cleanup.
- The test must assert non-empty entries from the winning result before checking
  names, so a dropped/no-op callback cannot pass.
- Reuse existing temporary-image helpers or create the smallest local images needed;
  no fixture directory in the repo.

## Done criteria

- [ ] Interactive folder recursion and single-image probing run off the UI thread.
- [ ] A later path request always wins over an earlier slow request.
- [ ] Installed-dataset generation advances only when a current scan succeeds; scan
  request generation advances at request start.
- [ ] A failed current scan retains a usable prior dataset, and a stale failure does
  not clear a newer loading request.
- [ ] The loading state clears on success, invalid result, and window/entity loss.
- [ ] Startup and `--convert` semantics do not change.
- [ ] `cargo test --locked`, clippy, and format checks pass.
- [ ] Only `src/main.rs` changes relative to the plan-001 branch.
- [ ] Worktree is clean after the commit.
- [ ] The main reviewer captured the real app's scanning state and proved the window
  still accepts a newer folder request while the earlier scan is pending.

## STOP conditions

- Plan 001 is not approved and present in the worktree.
- GPUI's background executor cannot return `scan::Scan` without making `Scan` sendable.
- A responsive scan requires changing `scan::scan` itself or adding a dependency.
- The implementation needs unbounded parallel probing.

## Maintenance notes

The dataset generation, not completion order or path equality alone, decides which
scan wins. If scan progress is added later, progress messages need the same guard.
