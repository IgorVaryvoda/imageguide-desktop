# Plan 025: Cancel Sirv transfers when the user unpairs or changes folder

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update the status row for
> this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 030aca8..HEAD -- src/main.rs`
> On a mismatch with "Current state", STOP — unless your reviewer said earlier
> batch plans already landed; then verify semantically.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: plans/018-sirv-glue-characterization-tests.md
- **Category**: bug
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

Pull and push loops run detached with no cancellation. Unpairing mid-pull
leaves downloads writing into the old local folder, and the finished job's
rescan fires against whatever dataset is current. Switching folders has the
same effect. The user said stop; the app keeps transferring and then mutates
state they did not ask it to touch.

## Current state

`src/main.rs:436-442`:

```rust
struct SirvJob {
    kind: SirvJobKind,
    done: usize,
    total: usize,
    failures: Vec<String>,
    finished: bool,
}
```

`src/main.rs:1536-1538` — busy check:

```rust
fn sirv_busy(&self) -> bool {
    self.sirv_job.as_ref().is_some_and(|job| !job.finished)
}
```

`unpair_sirv` (`src/main.rs:1394-1399`) clears pairing state and never touches
`sirv_job`. `request_path` (`src/main.rs:1081+`) bumps `dataset_generation`
(`src/main.rs:1037` shows the bump pattern:
`self.dataset_generation = self.dataset_generation.wrapping_add(1);`).

Both job loops follow one shape (pull at `src/main.rs:1576-1621`, push at
`1659-1715`): per item, spawn the transfer on the background executor, await,
then `this.update(cx, |audit, cx| { ... job.done = ix + 1; ... })`; after the
loop, mark `finished = true` and rescan/re-walk.

House rule: `parking_lot::Mutex` wherever a lock would appear; gpui ships it.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Fast check | `cargo check --locked` | exit 0 |
| Tests | `cargo test --bin imageguide --locked` | all pass, 1 ignored |
| Clippy | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs` (`SirvJob`, `start_pull`, `start_push`, `unpair_sirv`,
  `request_path`, `sirv_busy`)
- `src/main.rs` tests module (one new test)

**Out of scope**:
- `src/sirv.rs` (no transport-level abort; in-flight HTTP finishes)
- The compare/convert machinery's own generation handling
- New UI elements (no cancel button in this plan)

## Git workflow

Commit on the reviewer's worktree branch, e.g.
`fix: stop Sirv transfers when their pairing or folder goes away`.

## Steps

### Step 1: Add a cancellation flag the loops can watch

Add to `Audit` (near where `sirv_job` lives):

```rust
/// Set when the current transfer's owner (pairing, dataset) went away.
/// The running loop checks it between items.
sirv_cancel: Arc<std::sync::atomic::AtomicBool>,
```

Initialise `Arc::new(std::sync::atomic::AtomicBool::new(false))` in the
constructor (`rg -n "thumbs: HashMap::new" src/main.rs` finds the struct
literal around line 4402). Use an atomic, not a mutex: the loop polls it from
a background task while the UI thread writes it.

### Step 2: Set the flag at the two moments ownership ends

- In `unpair_sirv`, before clearing state:
  `self.sirv_cancel.store(true, std::sync::atomic::Ordering::Relaxed);`
- In `request_path`, next to the existing `dataset_generation` bump: the same
  store call.

### Step 3: Honour the flag in both loops

At the top of each per-item iteration (before spawning the transfer):

```rust
if cancel.load(std::sync::atomic::Ordering::Relaxed) {
    break;
}
```

where `cancel = self.sirv_cancel.clone()` is captured before `cx.spawn`. In
each completion handler, replace the unconditional finish/rescan with:

```rust
this.update(cx, |audit, cx| {
    let cancelled = audit.sirv_cancel.load(std::sync::atomic::Ordering::Relaxed);
    if cancelled {
        // The pairing/dataset this transfer served is gone: drop its
        // progress line instead of reporting work nobody asked for.
        audit.sirv_job = None;
    } else {
        if let Some(job) = audit.sirv_job.as_mut() {
            job.finished = true;
        }
        // ...existing rescan / walk_sirv_pairing call stays here...
    }
    cx.notify();
})
.ok();
```

Also clear the flag when a new job starts: in both `start_pull` and
`start_push`, right before creating the new `SirvJob`:
`self.sirv_cancel.store(false, std::sync::atomic::Ordering::Relaxed);`

### Step 4: Keep the busy gate truthful

`sirv_busy` needs no change: a cancelled loop breaks out and its handler sets
`sirv_job = None`, so the gate opens as soon as cancellation lands. Verify by
reading: after unpair, `sirv_job` is either still running-but-doomed (buttons
stay disabled until the current item completes) or gone. That residual delay
of one in-flight item is acceptable; note it in the commit message body.

### Step 5: Test what is testable

The pure seam is the decision "cancelled ⇒ no rescan". Extract it only if it
does not contort the code; otherwise cover the flag semantics directly:

```rust
#[test]
fn a_cancelled_job_is_dropped_not_reported() {
    // Directly exercises the atomic contract the loops rely on.
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    assert!(!cancel.load(std::sync::atomic::Ordering::Relaxed));
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(cancel.load(std::sync::atomic::Ordering::Relaxed));
}
```

If that feels vacuous, prefer extending plan 018's `notice_lines` test: a
cancelled job is `None`, so assert `notice_lines(0, 0, &[], None)` is empty —
already covered. Either way, keep exactly one small test and say which in the
commit message. Real proof of the loop behaviour comes from the live app
(reviewer runs it).

**Verify**: `cargo check --locked` → exit 0 after each step.

## Done criteria

- [ ] `unpair_sirv` and `request_path` both set the flag
- [ ] Both loops break on the flag; cancelled completions set
      `sirv_job = None` and skip the rescan/walk
- [ ] Starting a job resets the flag to false
- [ ] Suite, clippy, fmt green
- [ ] No files outside `src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- The job loops do not match the described shape (e.g. plan 020 already
  restructured `start_push`) — verify semantics with your reviewer.
- You find a third path that invalidates a running job (report it; do not
  silently wire it).

## Maintenance notes

A future Cancel button just stores the flag plus `cx.notify()` — no loop
changes. Reviewers should scrutinise: does any code path read `sirv_job`
after cancellation expecting progress? (`notices()` handles `None` already.)
