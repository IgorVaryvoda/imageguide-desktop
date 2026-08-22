# Plan 020: Create remote push folders once, not per file

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update the status row for
> this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 030aca8..HEAD -- src/main.rs src/sirv.rs`
> On a mismatch with "Current state", STOP — unless your reviewer said earlier
> batch plans already landed; then verify semantically.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/018-sirv-glue-characterization-tests.md
- **Category**: perf
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

`start_push` runs `mkdir` for every ancestor of every file inside the
per-file loop. mkdir on an existing folder is a network round trip that
returns 409. Pushing N files across D folders costs roughly N×depth extra
HTTP calls against a client that serialises anyway. Creating each distinct
folder once up front removes nearly all of them.

## Current state

`src/main.rs:1659-1691` — inside the per-file background task:

```rust
for (ix, (key, path)) in plan.iter().enumerate() {
    let outcome = cx.background_executor().spawn({
        ...
        async move {
            let mut client = client.lock();
            // mkdir on an existing folder is success upstream,
            // so every ancestor is simply ensured.
            for ancestor in sirv::ancestor_dirs(&key) {
                let full = format!("{dir}/{ancestor}");
                if client.mkdir(&full).is_err() {
                    return Err(format!("{key}: could not create folder"));
                }
            }
            match std::fs::read(&path) { ... }
        }
    }).await;
```

`sirv::ancestor_dirs("sub/deep/a.jpg")` returns `["sub", "sub/deep"]`
(`src/sirv.rs:161-173`). `plan` is `Vec<(String, PathBuf)>`. The failure
string shape `"{key}: could not create folder"` feeds `job.failures`.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Fast check | `cargo check --locked` | exit 0 |
| Tests | `cargo test --bin imageguide --locked` | all pass, 1 ignored |
| Clippy | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs` (the `start_push` function only)
- `src/main.rs` tests module (one new test)

**Out of scope**:
- `src/sirv.rs` (`mkdir`, `ancestor_dirs` stay as they are)
- `start_pull`

## Git workflow

Commit on the reviewer's worktree branch, conventional subject, e.g.
`perf: create push folders once per run`.

## Steps

### Step 1: Compute the unique folder list before the job loop

At the top of the detached closure (before `for (ix, ...) ...`), build the
distinct ancestor paths in first-seen order:

```rust
let mut seen = HashSet::new();
let dirs: Vec<String> = plan
    .iter()
    .flat_map(|(key, _)| sirv::ancestor_dirs(key))
    .filter(|dir| seen.insert(dir.clone()))
    .map(|ancestor| format!("{dir}/{ancestor}"))
    .collect();
```

### Step 2: Create them once in one background task

Immediately after building `dirs`:

```rust
if !dirs.is_empty() {
    let failed = cx
        .background_executor()
        .spawn({
            let client = client.clone();
            async move {
                let mut client = client.lock();
                let mut failed = None;
                for full in &dirs {
                    if client.mkdir(full).is_err() && failed.is_none() {
                        failed = Some(full.clone());
                    }
                }
                failed
            }
        })
        .await;
    if let Some(full) = failed {
        // Record one failure naming the folder and stop: nothing can upload
        // into it.
        this.update(cx, |audit, cx| {
            if let Some(job) = audit.sirv_job.as_mut() {
                job.failures = vec![format!("{full}: could not create folder")];
                job.finished = true;
            }
            cx.notify();
        })
        .ok();
        return;
    }
}
```

Keep the comment about mkdir-on-existing being success upstream, moved to the
new site.

### Step 3: Remove the per-file mkdir block

Delete the `for ancestor in sirv::ancestor_dirs(&key) { ... }` loop from the
upload task so it goes straight to `std::fs::read` + `upload`.

### Step 4: Unit-test the ordering guarantee

The dedup/ordering logic deserves a test. Extract it as a free function next
to `sirv_push_plan` from plan 018:

```rust
/// Distinct folders an upload list needs, paired-folder-qualified, in
/// first-seen order.
fn sirv_push_dirs(dir: &str, plan: &[(String, PathBuf)]) -> Vec<String>
```

Use it in Step 1. In the tests module add:

```rust
#[test]
fn push_dirs_are_unique_and_in_first_seen_order() {
    let plan = vec![
        ("sub/a.jpg".to_string(), PathBuf::from("/r/sub/a.jpg")),
        ("sub/b.jpg".to_string(), PathBuf::from("/r/sub/b.jpg")),
        ("deep/c.jpg".to_string(), PathBuf::from("/r/deep/c.jpg")),
    ];
    assert_eq!(
        sirv_push_dirs("/d", &plan),
        vec!["/d/sub".to_string(), "/d/deep".to_string()]
    );
}
```

**Verify each step**: `cargo check --locked` → exit 0. Final:
`cargo test --bin imageguide --locked` → all pass including the new test.

## Done criteria

- [ ] Exactly one call site of `client.mkdir` remains in `src/main.rs`
      (`rg -n "mkdir" src/main.rs` shows one occurrence outside comments)
- [ ] New test passes; suite green; clippy + fmt clean
- [ ] No files outside `src/main.rs` modified

## STOP conditions

- The per-file mkdir block does not match the excerpt (e.g. plan 029 already
  reshaped `start_push`) — verify semantics with your reviewer instead of improvising.
- `sirv::ancestor_dirs` no longer exists or changed signature.

## Maintenance notes

If uploads later become concurrent, keep folder creation strictly before the
first upload that needs it. Reviewers should confirm the abort path marks the
job finished so `sirv_busy()` clears (no stuck "Push" button).
