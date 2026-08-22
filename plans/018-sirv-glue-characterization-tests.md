# Plan 018: Give the Sirv glue characterization tests

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If anything in the "STOP conditions" section occurs, stop and
> report — do not improvise. When done, update the status row for this plan
> in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 030aca8..HEAD -- src/main.rs src/sirv.rs`
> If those files changed since `030aca8`, compare the "Current state" excerpts
> against the live code before proceeding; on a mismatch, treat it as a STOP
> condition. Exception: your reviewer may tell you earlier plans of this batch
> already landed in your worktree — then only verify the excerpts still match
> semantically.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW
- **Depends on**: none
- **Category**: tests
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

The last three feature commits added ~700 lines of Sirv sync glue with zero
test coverage. The riskiest logic — count computation, push-plan selection,
failure reporting — can regress silently. Later plans in this batch modify
exactly these functions; they need a safety net first. The repo's own pattern
is "extract the pure decision, test it" (see `conversion_targets` and its
test near the bottom of `src/main.rs`).

## Current state

- `src/main.rs` — the whole UI. Relevant pieces:
  - `src/main.rs:1637-1645` — `start_push` builds its upload plan inline:
    ```rust
    let plan: Vec<(String, PathBuf)> = self
        .entries
        .iter()
        .filter_map(|entry| {
            let key = sirv::relative_key(&self.root, &entry.path)?;
            (sirv::classify(entry.bytes, files.get(&key)) == sirv::SyncState::OnlyLocal)
                .then(|| (key, entry.path.clone()))
        })
        .collect();
    ```
  - `src/main.rs:1719-1748` — `refresh_sirv_counts(&mut self)`: loops over
    `self.entries`, computes `(to_push, changed, to_pull)`, stores it in
    `self.sirv_counts`. Pure dataset arithmetic trapped in a method.
  - `src/main.rs:3084-3142` — `notices(&self)`: assembles `parts: Vec<String>`
    from `self.mislabelled`, `self.unreadable`, `self.failures`, and the
    `sirv_job` progress line (`format!("{verb}: {} of {}{failures}", ...)`),
    then renders `Alert::warning("notices", parts.join("  ·  "))`.
- `src/sirv.rs:587-723` — existing test module; nine tests, all on pure
  helpers (`classify`, `pull_plan`, `ancestor_dirs`, credential round trip,
  etc.). Match their naming style: snake_case sentences stating behaviour.
- `src/main.rs` has a large `#[cfg(test)] mod tests` at the bottom with
  helpers like `entry(name, w, h, bytes, format)` building `scan::Entry`
  values. Find it with `rg -n "mod tests" src/main.rs`.

## Commands you will need

| Purpose   | Command                                  | Expected on success |
|-----------|------------------------------------------|---------------------|
| Fast check | `cargo check --locked`                   | exit 0              |
| Tests     | `cargo test --bin imageguide --locked`    | all pass, 1 ignored |
| Clippy    | `cargo clippy --all-targets -- -D warnings` | exit 0           |
| Format    | `cargo fmt --check`                       | exit 0             |

Note: `rust-toolchain.toml` pins the toolchain; plain `cargo` resolves it.

## Scope

**In scope** (the only files you should modify):
- `src/main.rs`

**Out of scope** (do NOT touch):
- `src/sirv.rs` — its helpers already have tests.
- Any behavioural change: refactored functions must return identical results.

## Git workflow

- Branch: whatever your reviewer's worktree is on (ask nothing; just commit).
- Commit style: `fix:` / `feat:` / conventional subjects, one concern per
  commit. Example from the repo: `fix: surface comparison preview failures`.
- Do NOT push.

## Steps

### Step 1: Extract the push-plan builder

In `src/main.rs`, add a free function (near the other free helpers, not
inside `impl Audit`):

```rust
/// Local files Sirv lacks, newest scan order preserved: the upload list.
fn sirv_push_plan(
    root: &Path,
    entries: &[scan::Entry],
    files: &HashMap<String, sirv::Node>,
) -> Vec<(String, PathBuf)>
```

Body: exactly the inline logic quoted above. Replace the inline block in
`start_push` with `let plan = sirv_push_plan(&self.root, &self.entries, files);`.

**Verify**: `cargo check --locked` → exit 0.

### Step 2: Extract the count computation

Add a free function:

```rust
/// (to push, changed, to pull) across the whole dataset, or None without a
/// loaded remote listing.
fn sirv_counts(
    root: &Path,
    entries: &[scan::Entry],
    files: Option<&HashMap<String, sirv::Node>>,
) -> Option<(usize, usize, usize)>
```

Move the body of `Audit::refresh_sirv_counts` here; the method becomes
`self.sirv_counts = sirv_counts(&self.root, &self.entries, pairing_files);`
preserving the current `None => None` shape (the caller at
`src/main.rs:1719` already unwraps the pairing to `files`; pass it through).

**Verify**: `cargo check --locked` → exit 0.

### Step 3: Extract the notice-line assembly

Add a free function that owns everything up to rendering:

```rust
/// Every warning line the footer alert shows, in display order.
fn notice_lines(
    mislabelled: usize,
    unreadable: usize,
    failures: &[String],
    sirv_job: Option<&SirvJob>,
) -> Vec<String>
```

Move the four `if` blocks from `notices()` (mislabelled, unreadable,
failures, sirv_job) into it verbatim. `notices()` becomes: call
`notice_lines(...)`, map empty to `None`, else build the Alert.

**Verify**: `cargo check --locked` → exit 0.

### Step 4: Write the tests

In the existing `#[cfg(test)] mod tests` in `src/main.rs`, add plain `#[test]`
functions (no gpui needed):

1. `push_plan_lists_only_files_sirv_lacks` — entries covering OnlyLocal,
   Same, Changed; assert only the OnlyLocal path/key survives.
2. `counts_cover_both_directions_and_changes` — build `files` with a missing
   key, a same-size node, a different-size node, plus a remote-only key;
   expect `(1, 1, 1)`.
3. `counts_are_none_without_a_remote_listing` — `files: None` → `None`.
4. `notices_name_failures_and_cap_the_list` — four failures, assert the
   joined line names the first three and says "and 1 more".
5. `notices_report_job_progress` — a half-done job renders
   `"Sirv pull: 5 of 10"`; with failures, the suffix lists them.
6. `notices_skip_when_nothing_is_wrong` — empty input → empty vec.

Model structure after existing plain tests in the same module.

**Verify**: `cargo test --bin imageguide --locked` → all pass, including the
six new ones (names above appear in output).

## Done criteria

- [ ] `cargo test --bin imageguide --locked` passes; six new tests present
- [ ] `cargo clippy --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] `start_push`, `refresh_sirv_counts`, `notices` contain no inline
      classification/count/format logic anymore (`git diff` confirms moves)
- [ ] No files outside `src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- The excerpts above do not match the live code and no reviewer exception applies.
- Extraction requires changing observable behaviour (different strings, counts, ordering).
- The test module layout prevents plain `#[test]` functions (report what you found).

## Maintenance notes

Later plans (025 cancel jobs, 026 pairing generation, 027 truthful walk
states, 029 changed-file actions) extend `sirv_push_plan`, `sirv_counts`, and
`notice_lines`. Keep them pure; the UI methods stay thin wrappers. A reviewer
should check the extraction did not reorder plan entries (scan order is the
product).
