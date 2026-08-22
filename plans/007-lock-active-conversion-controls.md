# Plan 007: Lock conversion-defining controls during an active run

> **Executor instructions**: Execute after approved plan 006 in the same worktree.
> Run every verification command. The reviewer maintains `plans/README.md`.
>
> **Drift check (run first)**:
> `git diff --stat 05384d3..HEAD -- src/main.rs`
> Plan 006 changes are expected. Stop if conversion or control callback seams changed
> for another reason.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plan 006
- **Category**: bug
- **Planned at**: commit `05384d3`, 2026-08-20

## Why this matters

Conversion snapshots its sources and settings, but controls remain live. A quality,
format, filter, selection, or path change can make the visible labels describe a
different job from the one writing files. Disabled styling alone is insufficient
because subscriptions, accessibility actions, drops, and callbacks are separate
mutation paths.

## Current state

- `src/main.rs:333-408` captures conversion sources and settings.
- Format, resize, slider subscription, filter subscription, selection, picker, and
  drop callbacks mutate conversion-defining state without a `converting` guard.
- The pinned button group, input, slider, checkbox, and buttons expose disabled APIs,
  but slider accessibility actions can still emit state changes.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test --locked` | existing and new lock tests pass |
| Lint | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/main.rs`

**Out of scope**:
- Canceling encodes or deleting output
- Background scanning
- New job abstractions or dependencies
- Comparison viewing and list/grid switching

## Git workflow

- Continue branch `improve/006-007-ui-state`.
- Use one focused imperative commit. Stage only `src/main.rs`; do not push, merge,
  or edit `plans/README.md`.

## Steps

### Step 1: Disable every conversion-defining control

While `converting` is true, disable format, resize, quality, filter, selection,
open-folder, and open-image controls. Keep comparison viewing and list/grid switching
available. Disabled controls must remain legible.

The pinned `ButtonGroup::children` path does not propagate the group's disabled flag,
so explicitly apply `disabled(converting)` to every segment child. The pinned slider
keeps accessibility increment/decrement handlers even when disabled; while converting,
render a small non-interactive static rail/thumb at the captured quality instead of a
`Slider`. Restore the real slider after completion. Do not fork the component.

### Step 2: Guard every mutation callback

At the mutation seam, return early while converting for format and resize button
groups, slider subscription, filter subscription, pointer and keyboard selection,
picker/path replacement, and drag/drop. Do not rely on presentation-level disabled
state. Non-mutating double-click comparison may remain available. The static quality
rail must expose no slider accessibility action while locked.

The active progress denominator must describe the captured conversion target count;
it must not be recomputed from mutable live selection/filter state.

**Verify**: focused tests invoke each callback seam while converting and prove
settings, path, selection, filter, and target/progress count stay unchanged. A GPUI
accessibility test dispatches increment against the locked quality presentation and
asserts both `Audit::quality` and `SliderState` stay unchanged. Assert segment child
disabled semantics, not only the group callback.

## Test plan

- Use one compact table-driven state test where pure callback guards share a seam,
  plus only the GPUI interaction coverage needed for subscription paths.
- Do not build a generic control-lock framework.

## Done criteria

- [ ] Every conversion-defining control is visibly disabled while converting.
- [ ] Every underlying mutation callback rejects changes while converting.
- [ ] Progress continues to use the captured target count.
- [ ] Comparison and list/grid switching remain usable.
- [ ] Tests, clippy, and format pass.
- [ ] Only `src/main.rs` differs from the plan-006 branch and the worktree is clean.
- [ ] The main reviewer captures an active real-app conversion and confirms disabled
  controls are clear and still legible.

## STOP conditions

- A pinned control lacks a disabled presentation and cannot be wrapped safely in
  `src/main.rs`.
- Correctness requires canceling or deleting files already written.
- Guarding a subscription requires a component fork.

## Maintenance notes

Disabled styling is affordance only. Every programmatic mutation seam must retain its
`converting` guard.
