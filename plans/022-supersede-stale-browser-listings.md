# Plan 022: Supersede in-flight Sirv browser listings

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
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

`browse_sirv_path` stores no identifier with its request. Click "…" then a
subfolder quickly: two `readdir` calls race on the shared client mutex and
whichever finishes last wins. The panel can show listing A's rows under path
B, so clicking a row descends into a wrongly composed path. The codebase's
established defence against this class of bug is a generation counter
(`dataset_generation`, `estimate_generation`) — this plan applies the same
pattern to the browser.

## Current state

`src/main.rs:444-452`:

```rust
struct SirvBrowser {
    client: Arc<parking_lot::Mutex<sirv::Client>>,
    path: String,
    /// `None` while the listing is in flight.
    nodes: Option<Result<Vec<sirv::Node>, String>>,
    focus: gpui::FocusHandle,
}
```

`src/main.rs:1261-1284`:

```rust
/// Fetch the listing for the browser's current path in the background.
fn browse_sirv_path(browser: &mut SirvBrowser, cx: &mut Context<Self>) {
    browser.nodes = None;
    let client = browser.client.clone();
    let path = browser.path.clone();
    cx.spawn(async move |this, cx| {
        let result = cx.background_executor()
            .spawn(async move { client.lock().readdir(&path).map_err(|error| error.to_string()) })
            .await;
        this.update(cx, |audit, cx| {
            if let Some(browser) = audit.sirv_browser.as_mut() {
                browser.nodes = Some(result);
            }
            cx.notify();
        })
    })
    .detach();
}
```

Callers: `open_sirv_browser` (1257), `descend_sirv` (1295),
`ascend_sirv` (1314). `descend`/`ascend` mutate `browser.path` then call
`browse_sirv_path`, which resets `nodes = None`.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Fast check | `cargo check --locked` | exit 0 |
| Tests | `cargo test --bin imageguide --locked` | all pass, 1 ignored |
| Clippy | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs` (`SirvBrowser` struct, `browse_sirv_path`, its three callers)

**Out of scope**:
- The pairing walk (`walk_sirv_pairing`) — covered by another plan.
- Any change to how rows are rendered from `nodes`.

## Git workflow

Commit on the reviewer's worktree branch, e.g.
`fix: drop superseded Sirv browser listings`.

## Steps

### Step 1: Add the counter to the browser state

Add a field to `SirvBrowser`:

```rust
/// Bumped on every browse request; a listing may only land if its request
/// is still the newest one.
browse_generation: u64,
```

Initialise it wherever `SirvBrowser { ... }` literals are constructed — there
are two: `open_sirv_browser`'s error path (`src/main.rs:1227`) and its main
path (`src/main.rs:1246`). Use `0` for both.

### Step 2: Capture and check the generation

In `browse_sirv_path`:

```rust
fn browse_sirv_path(browser: &mut SirvBrowser, cx: &mut Context<Self>) {
    browser.browse_generation = browser.browse_generation.wrapping_add(1);
    browser.nodes = None;
    let client = browser.client.clone();
    let path = browser.path.clone();
    let generation = browser.browse_generation;
    cx.spawn(async move |this, cx| {
        let result = ...same as today...;
        this.update(cx, |audit, cx| {
            // A newer navigation owns the panel now; this listing is stale.
            if audit
                .sirv_browser
                .as_ref()
                .is_some_and(|browser| browser.browse_generation == generation)
            {
                if let Some(browser) = audit.sirv_browser.as_mut() {
                    browser.nodes = Some(result);
                }
                cx.notify();
            }
        })
    })
    .detach();
}
```

Match the repo's comment voice (why, not what).

**Verify**: `cargo check --locked` → exit 0.

### Step 3: Confirm no other construction sites exist

`rg -n "SirvBrowser {" src/main.rs` → only the two sites from Step 1 compile
with the new field. If a third appears, initialise it too.

## Test plan

The race is timing-based and lives behind GPUI async plumbing; the repo proves
UI behaviour through the real app (see `plans/README.md` "Live visual proof
contract"), not unit tests. No new unit test is required. Existing suite must
stay green.

## Done criteria

- [ ] `browse_generation` checked before every `nodes = Some(...)` assignment
- [ ] Suite, clippy, fmt green
- [ ] No files outside `src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- `browse_sirv_path` does not match the excerpt.
- Adding the struct field breaks a construction site you cannot identify.

## Maintenance notes

If the browser ever gains refresh-on-focus or polling, every new entry point
must go through `browse_sirv_path` so the counter stays authoritative.
