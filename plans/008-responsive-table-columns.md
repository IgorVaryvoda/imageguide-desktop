# Plan 008: Keep decision columns usable at every supported width

> **Executor instructions**: Follow this plan in its isolated worktree. Run every
> command. The reviewer maintains `plans/README.md`.
>
> **Drift check**: `git diff --stat 05384d3..HEAD -- src/main.rs`; stop if table
> delegate or summary-result visibility changed.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: UI
- **Planned at**: `05384d3`, 2026-08-20

## Why this matters

The real app clips Weight and Result in a tiled 873 px window. `AuditTable` stores a
one-time Name width and `TableState` caches column groups, so resize and first-result
transitions do not rebuild the actual table.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test --locked` | existing and new table tests pass |
| Lint | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |
| Build | `cargo build --release --locked` | exit 0 |

## Scope

**In scope**: `src/main.rs`

**Out of scope**: gallery bands, window minimums, component forks, new dependencies.

## Git workflow

- Branch: `improve/008-responsive-table`.
- Commit only `src/main.rs`; do not push or edit `plans/README.md`. The executor
  worktree starts clean and must finish clean.

## Steps

1. Add pure helpers that map the exact current viewport width plus result visibility
   to the ordered columns and Name width. Keep Name, Format, Size, bpp, Weight, and
   Result; omit decorative Bar first at constrained widths. Name stays at least 140px.
2. Remove the stored one-time Name width. `columns_count`, `column`, and `render_td`
   must use the same mapping.
3. `TableState` caches widths and groups. Store the last exact rounded viewport width
   and result-visibility signature. When either value changes, update the signature,
   clone the `Entity<TableState<AuditTable>>`, and use `Context<Audit>::defer` (not
   `defer_in`) so the callback runs with only `&mut App` after the current Audit lease
   ends. In that callback update the table entity, call `TableState::refresh`, then
   `cx.notify()`. This avoids re-reading a mutably leased Audit and guarantees a
   repaint. Do not refresh when the exact signature is unchanged.
4. Tests must render an actual `TableState` through compact→wide, wide→compact,
   no-results→results, results→no-results, and two widths inside the same column band.
   Add stable debug selectors to rendered headers and assert their presence and
   geometry after every transition, including a draw after the final transition with
   no unrelated notification. Do not claim cache coverage through `delegate()` alone;
   the cache is private.

## Done criteria

- [ ] Decision columns remain present at supported compact width.
- [ ] Name width follows every viewport-width change, including within one band.
- [ ] Cached groups rebuild in both directions for width and result transitions.
- [ ] Tests, clippy, format, and release build pass; only `src/main.rs` changed.
- [ ] Main reviewer captures list mode at 760×560, 873×720, and 1100×720 before and
  after results, with no clipped decision columns.

## STOP conditions

- `TableState::refresh` cannot safely run through a deferred entity update.
- Correct columns require changing gpui-component.

## Maintenance notes

Table refresh identity includes exact width because Name width is continuous, not
only the discrete column band.
