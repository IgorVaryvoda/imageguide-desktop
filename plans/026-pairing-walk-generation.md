# Plan 026: Invalidate the pairing walk when the pairing changes

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
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/018-sirv-glue-characterization-tests.md
- **Category**: bug
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

`walk_sirv_pairing` captures only `dataset_generation`. Pairing folder A,
then quickly re-pairing folder B, lets A's finished walk land in B's pairing.
Almost every A key fails `unpair_remote` against B's prefix, so `files`
becomes an empty map: counts read "push everything / pull 0" and a push would
upload every local file into B. Same pattern as plan 022, one level up.

## Current state

`src/main.rs:1321-1340` (`pair_sirv`) installs a fresh pairing and calls the
walk:

```rust
self.sirv_pairing = Some(SirvPairing { dir: dir.clone(), files: None, client });
self.sirv_counts = None;
self.sirv_browser = None;
cx.notify();
self.walk_sirv_pairing(cx);
```

`src/main.rs:1344-1392` (`walk_sirv_pairing`):

```rust
fn walk_sirv_pairing(&mut self, cx: &mut Context<Self>) {
    let Some(pairing) = &self.sirv_pairing else { return; };
    let client = pairing.client.clone();
    let dir = pairing.dir.clone();
    let generation = self.dataset_generation;
    cx.spawn(async move |this, cx| {
        let walked = ...client.lock().walk(&dir)...;
        this.update(cx, |audit, cx| {
            if audit.dataset_generation != generation {
                return;
            }
            ...
        })
    })
    .detach();
}
```

`unpair_sirv` (`src/main.rs:1394-1399`) sets `sirv_pairing = None` (the
`as_mut() else return` already guards that direction).

`Audit::dataset_generation` bump pattern: `src/main.rs:1037`.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Fast check | `cargo check --locked` | exit 0 |
| Tests | `cargo test --bin imageguide --locked` | all pass, 1 ignored |
| Clippy | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs` (`pair_sirv`, `walk_sirv_pairing`, the `Audit` field block)

**Out of scope**:
- The fabricated `SirvJob` on walk error (`src/main.rs:1379-1385`) — another
  plan owns that.
- Plan 025's cancellation flag.

## Git workflow

Commit on the reviewer's worktree branch, e.g.
`fix: retire a Sirv listing when its pairing is replaced`.

## Steps

### Step 1: Add a pairing generation to Audit

Next to `dataset_generation`:

```rust
/// Bumped every time the Sirv pairing is replaced, so a walk for the old
/// pairing cannot fill the new one.
pairing_generation: u64,
```

Initialise to `0` in the constructor struct literal (same place
`dataset_generation` starts; find it via
`rg -n "dataset_generation:" src/main.rs`).

### Step 2: Bump it when the pairing is replaced

In `pair_sirv`, alongside the existing assignment:
`self.pairing_generation = self.pairing_generation.wrapping_add(1);`
Place it just before `self.sirv_pairing = Some(...)` so the walk started at
the end of the function captures the new value.

(`unpair_sirv` needs no bump: it clears `sirv_pairing`, and the completion
handler's `as_mut()` guard already drops the result.)

### Step 3: Capture and check it in the walk

In `walk_sirv_pairing`: capture `let pairing_generation = self.pairing_generation;`
next to the dataset generation capture, and extend the guard:

```rust
if audit.dataset_generation != generation
    || audit.pairing_generation != pairing_generation
{
    return;
}
```

Update the existing comment above the guard to say both conditions.

**Verify each step**: `cargo check --locked` → exit 0. Final gates below.

## Test plan

The race spans GPUI async plumbing plus a network walk; unit-testing it would
test mocks. The repo proves such behaviour through the real app (see the
"Live visual proof contract" in `plans/README.md`). Existing suite must stay
green; no new unit test required.

## Done criteria

- [ ] The walk completion checks both generations
- [ ] `pair_sirv` bumps `pairing_generation`; no other call site does
- [ ] Suite, clippy, fmt green
- [ ] No files outside `src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- `walk_sirv_pairing` or `pair_sirv` do not match the excerpts.
- `pairing_generation` collides with a similarly named field added by another
  batch plan (report instead of merging semantics).

## Maintenance notes

Any future code path that replaces or re-targets the pairing must bump this
counter. Reviewers should confirm the bump happens before `walk_sirv_pairing`
runs, or the fresh walk would invalidate itself.
