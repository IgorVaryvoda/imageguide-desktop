# Plan 027: Show the pairing walk as listing, and its failure as its own notice

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update the status row for
> this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 030aca8..HEAD -- src/main.rs`
> On a mismatch with "Current state", STOP — unless your reviewer said earlier
> batch plans already landed; then verify semantically.

## Status

- **Priority**: P2
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/018-sirv-glue-characterization-tests.md,
  plans/025-cancel-sirv-transfers.md, plans/026-pairing-walk-generation.md
  (all reshape the same functions)
- **Category**: bug / UX
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

Two states of the recursive pairing walk are invisible or lying:

1. While the walk runs, `files` is `None` and nothing indicates progress — a
   large folder looks paired-but-broken.
2. On walk failure the handler fabricates a finished pull job of zero files,
   so an auth or quota error renders as "Sirv pull: 0 of 0, 1 failed: …" —
   nonsense text for a listing that never started.

## Current state

The header builds Sirv stats at `src/main.rs:2633-2637`:

```rust
if let Some((to_push, changed, to_pull)) = self.sirv_counts {
    stats.push_str(&format!(
        " · Sirv: {to_push} to push · {changed} differ · {to_pull} to pull"
    ));
}
```

`sirv_counts` is `None` while `files` is `None`, so nothing shows.

The walk's error arm at `src/main.rs:1378-1386`:

```rust
Err(message) => {
    audit.sirv_job = Some(SirvJob {
        kind: SirvJobKind::Pull,
        done: 0,
        total: 0,
        failures: vec![message],
        finished: true,
    });
}
```

`notice_lines` (extracted by plan 018 from `notices()`,
`src/main.rs:3084-3142`) turns a job into
`"{verb}: {} of {}{failures}"`.

After plans 025/026 the walk completion handler also carries generation
guards and may consult the cancel flag — keep them intact.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Fast check | `cargo check --locked` | exit 0 |
| Tests | `cargo test --bin imageguide --locked` | all pass, 1 ignored |
| Clippy | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs` (`Audit` field block, `walk_sirv_pairing`, the header stats
  block at ~2633, `notice_lines` signature/body, `pair_sirv`/`unpair_sirv`
  clearing)

**Out of scope**:
- The Sync column rendering (`src/main.rs:3596+`) stays as is.
- The browser panel, buttons, and gallery tiles.

## Git workflow

Commit on the reviewer's worktree branch, e.g.
`fix: show the Sirv listing as pending and its failure honestly`.

## Steps

### Step 1: Give the failed walk its own slot

Add to `Audit`:

```rust
/// The last pairing-walk failure. Separate from `sirv_job`: a listing that
/// never ran is not a transfer that moved files.
sirv_walk_error: Option<String>,
```

Init `None` in the constructor. Clear it in `pair_sirv` (fresh pairing),
`unpair_sirv`, and on walk success.

### Step 2: Report the failure there instead of faking a job

In the walk's `Err(message)` arm:

```rust
Err(message) => {
    audit.sirv_walk_error = Some(message);
}
```

Delete the fabricated `SirvJob`.

### Step 3: Extend notice_lines

New parameter `sirv_walk_error: Option<&str>` after `sirv_job`. New branch:

```rust
if let Some(message) = sirv_walk_error {
    parts.push(format!("Sirv listing failed: {message}"));
}
```

Update `notices()`'s call to pass `self.sirv_walk_error.as_deref()`.

### Step 4: Show the pending state in the header

Replace the header block quoted above with:

```rust
match self.sirv_counts {
    Some((to_push, changed, to_pull)) => {
        stats.push_str(&format!(
            " · Sirv: {to_push} to push · {changed} differ · {to_pull} to pull"
        ));
    }
    // A fresh pairing has no diff yet; say so instead of going quiet.
    None if self.sirv_pairing.is_some() => {
        let dir = self.sirv_pairing.as_ref().map(|p| p.dir.clone()).unwrap_or_default();
        stats.push_str(&format!(" · Sirv: listing {dir}…"));
    }
    None => {}
}
```

Extract this match into a pure function next to plan 018's helpers, so it is
testable:

```rust
/// The Sirv part of the header line: counts when known, "listing…" while
/// the walk runs, nothing without a pairing.
fn sirv_header_suffix(
    pairing_dir: Option<&str>,
    counts: Option<(usize, usize, usize)>,
) -> String
```

(empty string when no pairing). Use it in `header`. Note: if plan 025 landed,
`sirv_pairing` may be gone mid-cancel; `None` covers that.

### Step 5: Tests

Extend the tests module:

```rust
#[test]
fn the_header_lists_while_the_walk_runs_and_counts_once_it_lands() {
    assert_eq!(sirv_header_suffix(None, None), "");
    assert_eq!(sirv_header_suffix(Some("/photos"), None), " · Sirv: listing /photos…");
    assert_eq!(
        sirv_header_suffix(Some("/photos"), Some((1, 2, 3))),
        " · Sirv: 1 to push · 2 differ · 3 to pull"
    );
}

#[test]
fn a_failed_walk_is_its_own_notice_not_a_zero_file_transfer() {
    let lines = notice_lines(0, 0, &[], None, Some("Sirv 403: quota"));
    assert_eq!(lines, vec!["Sirv listing failed: Sirv 403: quota".to_string()]);
}
```

Adjust existing `notice_lines` call sites in tests (plan 018 added some) to
the new arity with `None`.

**Verify**: `cargo test --bin imageguide --locked` → all pass.

## Done criteria

- [ ] No code path fabricates a `SirvJob` for a walk failure
      (`rg -n "total: 0" src/main.rs` returns no Sirv-related matches)
- [ ] Header shows "listing …" whenever a pairing exists without counts
- [ ] New tests pass; suite, clippy, fmt green
- [ ] No files outside `src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- Plans 025/026 have not landed and the walk handler does not carry their guards.
- `notice_lines` does not exist yet (plan 018 missing).

## Maintenance notes

If a retry button for failed listings is added later, hang it off
`sirv_walk_error` being set. Reviewers should confirm the Sync column still
renders blank (not an error) while `files` is `None` — that is intended.
