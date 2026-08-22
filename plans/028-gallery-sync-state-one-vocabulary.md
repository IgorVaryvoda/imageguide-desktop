# Plan 028: Surface sync state and results on gallery tiles, in one vocabulary

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
- **Effort**: M
- **Risk**: LOW
- **Depends on**: plans/018-sirv-glue-characterization-tests.md (for the pure
  helper pattern); execute after 026/027 to keep their diffs clean
- **Category**: UX
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

The list view shows each file's Sirv state ("new"/"synced"/"changed") and its
conversion result; gallery tiles show neither. A user working in grid mode
converts and syncs blind. The same three states also carry three vocabularies:
the column says "missing"-style words per state, buttons say "Push N new" /
"Pull N missing", the header says "to push · differ · to pull". One concept,
one word.

## Current state

Gallery tile: `src/main.rs:889-969` — `fn tile(...)` renders thumbnail box,
tick checkbox (absolute, top-left at ~4px), cursor ring via border colour.
The thumbnail slot is:

```rust
.child(
    div()
        .relative()
        .w_full()
        .h(px(tile_size - 68.))
        ...
        .when_some(thumb, |slot, image| { ... })
        .child(/* absolute top-left checkbox, debug_selector "grid-checkbox-{index}" */)
)
```

Below the image slot the tile shows name and weight (`density` is computed at
line 903 but only used for text elsewhere).

Sync column logic, `src/main.rs:3596-3612`:

```rust
let state = audit.sirv_pairing.as_ref()
    .and_then(|pairing| pairing.files.as_ref())
    .and_then(|files| {
        let key = sirv::relative_key(&audit.root, &entry.path)?;
        Some(sirv::classify(entry.bytes, files.get(&key)))
    });
let (label, colour) = match state {
    None => return div().into_any_element(),
    Some(sirv::SyncState::Same) => ("synced", cx.theme().muted_foreground),
    Some(sirv::SyncState::Changed) => ("changed", cx.theme().yellow),
    Some(sirv::SyncState::OnlyLocal) => ("new", cx.theme().blue),
};
```

Result column, `src/main.rs:3622-3649`: `audit.results.get(&index)` gives
converted bytes; percent `saved / entry.bytes * 100`; grown files show
`Tag::warning().small().child("larger")`, others `Tag::success()...−N%`.

Buttons/header: `src/main.rs:2478-2493` "Pull {to_pull} missing" /
"Push {to_push} new"; header at `src/main.rs:2635`
`"{to_push} to push · {changed} differ · {to_pull} to pull"`.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Fast check | `cargo check --locked` | exit 0 |
| Tests | `cargo test --bin imageguide --locked` | all pass, 1 ignored |
| Clippy | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs` (`tile`, the Sync column block, `render_td`'s call into it if
  needed for the shared helper, buttons/header label strings, tests module)

**Out of scope**:
- The comparison view, status bar height, table layout thresholds.
- Plan 029's changed-file actions (separate plan).

## Git workflow

Commit on the reviewer's worktree branch, e.g.
`feat: show sync state and results on gallery tiles`.

## Steps

### Step 1: One shared classifier

Extract the Sync-column lookup into an `Audit` method used by both surfaces:

```rust
impl Audit {
    /// This row's Sirv state, or None without a loaded listing.
    fn sync_state_of(&self, entry: &scan::Entry) -> Option<sirv::SyncState> {
        self.sirv_pairing.as_ref()
            .and_then(|pairing| pairing.files.as_ref())
            .and_then(|files| {
                let key = sirv::relative_key(&self.root, &entry.path)?;
                Some(sirv::classify(entry.bytes, files.get(&key)))
            })
    }
}
```

Use it in the Sync column (keeping its existing label/colour match) and in
the tile.

### Step 2: Badge tiles with the sync state

In `tile`, inside the relative image-slot div, add an absolute badge at the
top-right (mirroring the checkbox's top-left), rendered only when
`self.sync_state_of(entry)` is `Some(state)`:

```rust
.child(
    div()
        .absolute()
        .top(px(4.))
        .right(px(4.))
        .px_1()
        .rounded_sm()
        .text_size(px(10.))
        .font_family(cx.theme().mono_font_family.clone())
        .bg(cx.theme().background)
        // Same three colours the list column uses.
        .text_color(match state {
            sirv::SyncState::Same => cx.theme().muted_foreground,
            sirv::SyncState::Changed => cx.theme().yellow,
            sirv::SyncState::OnlyLocal => cx.theme().blue,
        })
        .child(match state { ...same three labels: "new" / "synced" / "changed"... }),
)
```

### Step 3: Show the conversion outcome on the tile

Under the tile's existing name/weight text, when
`audit.results.get(&index)` exists, add one mono line mirroring the Result
column exactly: `−N%` in muted foreground, or `larger` when the file grew
(compute percent the same way, guarding `entry.bytes == 0`). Keep it to one
line; do not add the absolute byte figure.

### Step 4: One vocabulary

Unify on: **new / synced / changed** (the column's words).

- Button labels become `"Push {to_push} new"` (already correct) and
  `"Pull {to_pull} new"` (was "missing").
- Header becomes `"{to_push} new · {changed} changed · {to_pull} missing"` →
  no: keep direction explicit, settle on
  `"Sirv: {to_push} to push · {changed} changed · {to_pull} to pull"`
  (only "differ" → "changed").

If plan 027 landed, edit its `sirv_header_suffix` instead of the inline
format string, and update its test expectation.

### Step 5: Test the pure parts

Add/extend unit tests:

```rust
#[test]
fn every_sync_state_has_one_word_everywhere() {
    // The single source of truth both views share.
    fn label(state: sirv::SyncState) -> &'static str {
        match state {
            sirv::SyncState::OnlyLocal => "new",
            sirv::SyncState::Same => "synced",
            sirv::SyncState::Changed => "changed",
        }
    }
    assert_eq!(label(sirv::SyncState::OnlyLocal), "new");
    assert_eq!(label(sirv::SyncState::Same), "synced");
    assert_eq!(label(sirv::SyncState::Changed), "changed");
}
```

To make this real rather than vacuous: extract that `label` as a free
function `sirv::state_label`-style helper in `main.rs`
(`fn sync_label(state: sirv::SyncState) -> &'static str`) and use it in BOTH
the column and the tile badge instead of inline string literals. The test
then pins the shared source. Update `rg -n '"missing"' src/main.rs` → no
matches after Step 4.

Visual proof of tile badges follows the repo's live-capture contract
(`plans/README.md`) — your reviewer runs it; you do not need a display.

**Verify each step**: `cargo check --locked` → exit 0. Final gates below.

## Done criteria

- [ ] `rg -n '"differ"|"missing"' src/main.rs` returns no matches
- [ ] Tile badge and Sync column both read their word from `sync_label`
- [ ] Tile shows result percent/larger when a result exists
- [ ] Suite, clippy, fmt green
- [ ] No files outside `src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- `tile` does not match the excerpt (e.g. another batch plan reshaped it).
- The theme exposes no equivalent of `muted_foreground`/`yellow`/`blue` —
  report what exists rather than inventing colours.

## Maintenance notes

Any future surface showing sync state must use `sync_state_of` + `sync_label`.
Reviewers should check the badge cannot overlap the checkbox (opposite
corners) and adds no layout shift to non-paired folders (badge absent).
