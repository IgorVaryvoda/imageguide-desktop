# Plan 021: Stop reading the credentials file on every settings render

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
- **Category**: perf
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

`settings_panel_view` calls `sirv::load_credentials()` synchronously during
render just to compute a boolean. GPUI re-renders this view on every
`cx.notify()` — including each keystroke in the panel's input fields — so
typing in the settings box does blocking disk I/O and materialises the
plaintext secret on the UI thread per keystroke. `src/sirv.rs:4-5` states the
contract: sync work belongs on a background executor, not in render.

## Current state

`src/main.rs:2200-2208`:

```rust
/// The settings panel: CDN keys, and the Studio link minted from them.
fn settings_panel_view(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
    let Some(panel) = self.settings_panel.as_ref() else {
        return div().into_any_element();
    };
    let connected = sirv::load_credentials()
        .and_then(|creds| creds.studio_key)
        .is_some();
```

`open_settings` (`src/main.rs:1402-1422`) already calls
`sirv::load_credentials()` once when building the panel, and constructs
`SettingsPanel { ..., cdn_status: None, studio_status: None, focus_ix: 0 }`.

`connect_studio` (`src/main.rs:1447-1498`) sets
`panel.studio_status = Some((true, format!("Connected · ...")))` on success —
that is the only moment `connected` can flip to true.

The `SettingsPanel` struct is at `src/main.rs:456-465`.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Fast check | `cargo check --locked` | exit 0 |
| Tests | `cargo test --bin imageguide --locked` | all pass, 1 ignored |
| Clippy | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs` (the `SettingsPanel` struct, `open_settings`,
  `connect_studio`, `settings_panel_view`)

**Out of scope**:
- `src/sirv.rs`
- `save_sirv_settings`, `check_studio` (they do not change the flag's meaning;
  `check_studio` only confirms an existing key)

## Git workflow

Commit on the reviewer's worktree branch, e.g.
`perf: cache the Studio link flag instead of rereading it per render`.

## Steps

### Step 1: Add the cached flag to the panel struct

Add to `SettingsPanel`:

```rust
/// Whether a Studio key exists in the store. Read once at open time and
/// updated on connect, so render never touches the credentials file.
studio_linked: bool,
```

### Step 2: Set it at the two moments that define it

- In `open_settings`: `studio_linked: stored.as_ref().and_then(|c| c.studio_key.as_ref()).is_some(),`
  (`stored` is the `Option<Credentials>` already loaded at line 1403).
- In `connect_studio`'s success arm, next to the existing status assignment:
  `panel.studio_linked = true;`

### Step 3: Use it in the view

Replace the `load_credentials()` block in `settings_panel_view` with:

```rust
let connected = panel.studio_linked;
```

**Verify**: `cargo check --locked` → exit 0. Then
`rg -n "load_credentials" src/main.rs` — occurrences must remain only in
`open_settings`, `save_sirv_settings`, `connect_studio`, `check_studio`, and
`open_sirv_browser` (event handlers), never inside `settings_panel_view`.

## Test plan

No new test: the flag is UI state with no pure logic. Existing suite must
stay green. (Visual proof, if your reviewer asks: open Settings, type in a
field — behaviour unchanged.)

## Done criteria

- [ ] `settings_panel_view` contains no `load_credentials` call
- [ ] Suite, clippy, fmt all green
- [ ] No files outside `src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- `settings_panel_view` does not match the excerpt.
- `open_settings` no longer loads `stored` (the prefill source is gone).

## Maintenance notes

If a future feature deletes or rotates the Studio key, it must also set
`studio_linked = false`. Reviewers should check the flag cannot go stale
within one panel session: the only writers are open and connect.
