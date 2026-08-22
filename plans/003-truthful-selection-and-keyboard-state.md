# Plan 003: Make conversion selection truthful for pointer, keyboard, and filters

> **Executor instructions**: Follow this plan in its isolated worktree. Run every
> verification command. The reviewer maintains `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 05384d3..HEAD -- src/main.rs`
> Stop if selection, filtering, table delegate, or keyboard routing changed.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `05384d3`, 2026-08-20

## Why this matters

The UI promises that the visible filter narrows what Convert will touch. Today an
existing selection bypasses that filter. Checkbox events also bubble to their parent
row/tile, Space changes selection without refreshing the estimate, and list-mode
keyboard movement has no visible cursor or scroll. The smallest coherent fix is one
selection contract shared by all input paths.

## Current state

- `src/main.rs:310-319`: when `selected` is non-empty, `targets` returns every
  selected index without intersecting `visible`.
- `src/main.rs:597-604`: Space toggles selection but does not schedule an estimate.
- `src/main.rs:644-690` and `1840-1896`: checkboxes are nested in clickable tiles or
  rows. The pinned checkbox calls `prevent_default`, not `stop_propagation`.
- `src/main.rs:544-551`: cursor movement only changes an index and notifies.
- `src/main.rs:1831-1842`: list rows style selection but not `cursor`.
- `src/main.rs:2208-2219`: arrows/Page/Home/End/Space/Enter use the cursor.
- Existing component capability: the pinned table exposes `visible_range()` and its
  existing vertical scroll handle supports `ScrollStrategy::Nearest`.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test --locked` | existing and new selection tests pass |
| Lint | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs`

**Out of scope**:
- Encoder/output behavior
- New selection modes, saved filters, or issue queues
- Background scan work from plan 002
- General table redesign from plan 004

## Git workflow

- Branch: `improve/003-selection-state`
- Use one focused imperative commit. Stage only `src/main.rs`; do not push, merge,
  or edit `plans/README.md`.

## Steps

### Step 1: Make targets visible-first

Keep the existing rule that no selection means all visible rows. When a selection
exists, return only selected indices that are also in `visible`, in visible order.
This makes sorting deterministic and filtering authoritative without deleting hidden
selection; clearing the filter may reveal those ticks again.

Conversion-related counts and the Convert label must use `targets.len()`, not
`selected.len()`, so the button describes the exact job. Selection-management UI,
including `Clear N`, must keep using `selected.len()` because it clears persistent
membership, including hidden selections. If the target intersection is empty,
Convert is inert. When conversion starts, store the captured target count and use it
for progress until the run completes; do not recompute the denominator from live
filter or selection state.

**Verify**: add pure/state tests for no selection, selected visible rows in visible
order, and a selection fully hidden by the filter. Start a conversion, then mutate
filter/selection through the state seam and prove progress retains the captured
target denominator.

### Step 2: Stop checkbox activation at the checkbox

In both grid and table checkbox callbacks call `cx.stop_propagation()` before
mutating selection. Keep parent row/tile click behavior unchanged. The root keyboard
handler must route list shortcuts only when `Audit::focus` itself is focused; a
focused checkbox owns Space and must not also trigger the root's earlier key-down
path. Ensure keyboard activation of a focused checkbox still works through the
component.

**Verify**: add or adapt focused GPUI tests proving one pointer click toggles exactly
once and preserves an existing multi-selection, and Space on a focused checkbox
changes only that checkbox's entry exactly once.

### Step 3: Give all selection paths the same derived-state update

After Space successfully toggles a row, schedule a fresh estimate before notifying,
matching the checkbox and row-click paths. Do not schedule when no visible entry
exists.

**Verify**: a test records the estimate generation, invokes the keyboard toggle, and
asserts the generation advanced and the intended entry changed selection.

### Step 4: Render and reveal the keyboard cursor

Style the table row whose visible index equals `Audit::cursor` with a distinct focus
border/background that does not rely on the selection fill. After keyboard movement,
inspect the table's existing `visible_range()`. If the target row is outside that
range, use its existing vertical scroll handle with `ScrollStrategy::Nearest`; if it
is already visible, leave the scroll position unchanged. Grid cursor styling already
exists; preserve it.

Do not replace `Audit::cursor` with the table component's independent selection
model; that would create two selection sources.

**Verify**: a state/UI test proves movement within the visible range does not change
scroll position, then moves the cursor beyond the viewport and confirms the target
is revealed. The main reviewer, not the executor, will perform live-window visual
proof because the Linux headless capture is a known baseline failure.

## Test plan

- Keep tests in `src/main.rs` beside existing sort tests.
- Cover target intersection/order and captured conversion count, pointer and
  focused-keyboard checkbox non-bubbling, Space estimate invalidation, and nearest
  table cursor reveal.
- Prefer one small GPUI harness for interaction cases; do not introduce a general UI
  test framework.

## Done criteria

- [ ] Hidden selected rows are never conversion targets while filtered out.
- [ ] Conversion labels/counts equal the actual target slice; `Clear N` still equals
  total persistent selection.
- [ ] Checkbox clicks do not also invoke row/tile selection.
- [ ] Space refreshes the estimate only when it changes a visible entry.
- [ ] The list cursor is visible and keyboard movement scrolls it into view.
- [ ] `cargo test --locked`, clippy, and format checks pass.
- [ ] Relative to the executor worktree's starting state, only `src/main.rs` differs;
  after its scoped commit, `git status --short` is empty.
- [ ] The main reviewer captured list and grid states before and after checking then
  unchecking a box, and visually proved the keyboard cursor at an off-screen row.

## STOP conditions

- The pinned checkbox already stops propagation in a way contradicted by its source
  or an interaction test.
- Table scrolling requires replacing the component or exposing private internals.
- Correct target scoping requires deleting hidden selection rather than intersecting
  it at conversion time.

## Maintenance notes

`visible` is the display and conversion order; `selected` is persistent membership;
`targets` is their intersection. Future bulk actions must use `targets`, not read the
set directly.
