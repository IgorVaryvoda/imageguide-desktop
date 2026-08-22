# Plan 029: Give "changed" rows a way to resolve

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update the status row for
> this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 030aca8..HEAD -- src/main.rs src/sirv.rs`
> On a mismatch with "Current state", STOP — unless your reviewer said earlier
> batch plans already landed; then verify semantically.

## Status

- **Priority**: P2
- **Effort**: M
- **Risk**: MED (adds overwrite semantics — destructive by nature)
- **Depends on**: plans/018-sirv-glue-characterization-tests.md,
  plans/020-push-mkdir-once.md, plans/025-cancel-sirv-transfers.md,
  plans/027-truthful-listing-states.md
- **Category**: UX
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

Push uploads only `OnlyLocal` files; pull downloads only keys the local side
lacks. A file present on both sides with a different size reads yellow
"changed" in every view — and no control anywhere resolves it. The count
never reaches zero, the header looks permanently dirty, and the user must go
to Sirv's own web app to break the tie.

## Current state

`src/main.rs:1623-1645` (`start_push`) selects targets:

```rust
/// Upload every local file Sirv lacks. Changed files are left alone in
/// both directions; overwriting is a decision, not a side effect.
...
(sirv::classify(entry.bytes, files.get(&key)) == sirv::SyncState::OnlyLocal)
    .then(|| (key, entry.path.clone()))
```

`src/sirv.rs:150-157` (`pull_plan`) filters to keys local lacks:

```rust
pub fn pull_plan(remote: &[Node], dir: &str, local_keys: &HashSet<String>) -> Vec<String> {
    remote.iter()
        .filter_map(|node| unpair_remote(dir, &node.filename))
        .filter(|key| !local_keys.contains(key))
        .collect()
}
```

Pull write step (`src/main.rs:1587+`) writes downloaded bytes to
`root.join(key)` — currently only reached for absent files, so overwriting
never happens today.

Job kinds at `src/main.rs:430-434`: `enum SirvJobKind { Pull, Push }`, used
by `notices()` for the verb string ("Sirv pull" / "Sirv push").

Buttons at `src/main.rs:2477-2494`: "Pull {to_pull} missing" and
"Push {to_push} new", each `.disabled(busy || count == 0)`. Counts tuple:
`(to_push, changed, to_pull)` from `sirv_counts`.

After plan 018, plan selection lives in free functions
(`sirv_push_plan`, `sirv_counts`) with tests; after 025, loops honour a
cancel flag; after 020, folder creation is hoisted out of the loop.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Fast check | `cargo check --locked` | exit 0 |
| Tests | `cargo test --bin imageguide --locked` | all pass, 1 ignored |
| Clippy | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs` (`SirvJobKind`, `start_pull`, `start_push`, buttons block,
  tests module)
- `src/sirv.rs` (`pull_plan` signature + its test)

**Out of scope**:
- Per-row actions in the table/gallery (bulk buttons only, this plan).
- Any content comparison or hashing — size stays the classifier.

## Git workflow

Commit on the reviewer's worktree branch, e.g.
`feat: resolve changed files with forced push and pull`.

## Steps

### Step 1: Extend the job kinds

```rust
#[derive(Clone, Copy, PartialEq)]
enum SirvJobKind {
    Pull,
    Push,
    /// Overwrite remote copies that differ. A decision, not a side effect.
    PushChanged,
    /// Overwrite local copies that differ. Same rule, other direction.
    PullChanged,
}
```

Update every `match job.kind` site: the verb match in `notices()`
("Sirv push (overwrite)" / "Sirv pull (overwrite)") and anywhere else
(`rg -n "SirvJobKind::" src/main.rs`). Plan 027's walk-error arm does not
create jobs anymore, so no change there.

### Step 2: Generalise the push plan

Change plan 018's helper to take the accepted state:

```rust
fn sirv_push_plan(
    root: &Path,
    entries: &[scan::Entry],
    files: &HashMap<String, sirv::Node>,
    accept: sirv::SyncState,
) -> Vec<(String, PathBuf)>
```

Filter becomes `sirv::classify(...) == accept`. `start_push` passes
`SyncState::OnlyLocal`; a new `start_push_changed(cx)` passes
`SyncState::Changed`. Factor the shared body of the two starters into one
private method `fn run_push(&mut self, accept: sirv::SyncState, cx: ...)`
so there is exactly one job loop (it must keep plan-025's cancel checks and
plan-020's hoisted mkdir). Update plan 018's test to pass `OnlyLocal` and add:

```rust
#[test]
fn the_forced_push_plan_takes_changed_files_and_leaves_synced_ones() {
    ... same fixture as the existing push-plan test ...
    assert_eq!(sirv_push_plan(root, &entries, &files, sirv::SyncState::Changed), vec![changed_entry_pair]);
    assert!(sirv_push_plan(root, &entries, &files, sirv::SyncState::Same).is_empty());
}
```

### Step 3: Generalise the pull side

Extend `pull_plan` with an explicit mode instead of a bool soup:

```rust
/// Remote keys worth downloading. `Missing` lists what local lacks;
/// `Differing` lists keys whose remote size differs from local — the
/// overwrite set.
pub fn pull_plan(remote: &[Node], dir: &str, local_sizes: &HashMap<String, u64>, differing: bool) -> Vec<String>
```

Hmm — the current callers pass `local_keys: &HashSet<String>` of known keys
without sizes. To classify Changed you need sizes. Concretely:

- In `start_pull`, build `local_sizes: HashMap<String, u64>` from entries
  (key from `relative_key`, value `entry.bytes`) — replacing the
  `local_keys` HashSet.
- `pull_plan(remote, dir, &local_sizes, false)` = keys where
  `!local_sizes.contains_key(key)` (identical behaviour to today).
- `pull_plan(remote, dir, &local_sizes, true)` = keys where the key exists
  and `classify(size, Some(node)) == Changed`.
- Keep `differing=false` semantics byte-compatible; update the existing
  `pull_plan_lists_only_keys_the_local_side_lacks` test to the new signature
  and add a `changed_keys_only_when_asked` test.

Then factor `start_pull` into `run_pull(&mut self, differing: bool, cx)`
with one loop; in the write step, when `differing` is true the write
overwrites an existing file (that is the point); keep plan 024's
`safe_key` guard either way. The completion handler keeps the cancel-aware
shape from plan 025.

### Step 4: Buttons

In the paired-controls row (`src/main.rs:2477-2505`), next to the existing
two, add two ghost small buttons shown only when `changed > 0`:

```rust
.label(format!("Overwrite {changed} on Sirv"))
.disabled(busy)
.on_click(... audit.run_push(sirv::SyncState::Changed, cx) ...)
```

```rust
.label(format!("Take {changed} from Sirv"))
.disabled(busy)
.on_click(... audit.run_pull(true, cx) ...)
```

Both are deliberate destructive actions; their wording names the direction
and the overwrite. No confirmation dialog exists in this app's toolkit flow;
the two-step affordance is the explicit label plus the notice line reporting
each outcome with filenames. Add nothing modal.

### Step 5: Gates

`cargo check --locked` → exit 0 after each step. Full gates below.

## Test plan

- Updated `push plan` test (accept parameter) as above.
- New pull-plan test for `differing=true` returning exactly the Changed keys.
- Existing counts/notices tests unchanged and green (verb strings for new
  kinds need no new tests unless you touched `notice_lines`' verb match — if
  so extend the existing progress test with one PushChanged case).

## Done criteria

- [ ] With a pairing loaded and changed files present, four buttons render:
      Pull N new · Push N new · Overwrite N on Sirv · Take N from Sirv
- [ ] `rg -n "OnlyLocal" src/main.rs` shows the plain paths using it only as
      the default accept state
- [ ] New tests pass; suite, clippy, fmt green
- [ ] No files outside `src/main.rs` and `src/sirv.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- Any dependency plan has not landed in your worktree.
- You cannot keep exactly ONE push loop and ONE pull loop without duplicating
  the transfer bodies — report the obstacle instead of copying them.
- `pull_plan` callers exist beyond `start_pull` (report them).

## Maintenance notes

The overwrite actions are the only code paths that may destroy data; any
future change to the write steps must re-check `safe_key` (024) and the
cancel flag (025). Reviewers should verify a completed forced pull triggers
the rescan and a forced push triggers `walk_sirv_pairing`, same as the plain
directions.
