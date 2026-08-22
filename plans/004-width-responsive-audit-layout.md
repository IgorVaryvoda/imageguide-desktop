# Plan 004: Make the audit table and gallery respond to window width

> **Executor instructions**: Follow this plan in its isolated worktree. Run every
> verification command. Do not attempt Linux headless screenshots; that inherited
> gate is documented in `plans/README.md`. The reviewer owns live visual proof.
>
> **Drift check (run first)**:
> `git diff --stat 05384d3..HEAD -- src/main.rs`
> Stop if table column construction, gallery virtualization, or window sizing changed.

## Status

- **Priority**: P1
- **Effort**: M
- **Risk**: MED
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `05384d3`, 2026-08-20

## Why this matters

The app restores arbitrary window sizes, but table width is measured only once and
the gallery always renders five fixed tiles. Live proof at 873 px showed the list
clipping after `bpp`, hiding Weight and Result. A responsive mapping is smaller and
more reliable than adding a new layout system: use current viewport width to choose
columns, name width, and gallery count.

## Current state

- `src/main.rs:69-72`: gallery tiles are fixed at 168 px and five columns.
- `src/main.rs:1657-1747`: `AuditTable` stores one `name_width` measured at creation.
- `src/main.rs:1766-1779`: column count only adds/removes the final Result column;
  the 140 px decorative weight bar is always present.
- `src/main.rs:2023-2045`: render already reads current viewport dimensions.
- `src/main.rs:2252-2264`: gallery row math uses fixed `TILE_COLUMNS`.
- `src/main.rs:2650-2658`: remembered dimensions are restored without a minimum.
- Existing convention: a few file-local constants and direct arithmetic; preserve it.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test --locked` | layout helper tests and existing tests pass |
| Lint | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |
| Build | `cargo build --release --locked` | exit 0, release binary produced |

## Scope

**In scope**:
- `src/main.rs`

**Out of scope**:
- New dependencies or a layout abstraction/module
- Changing row/tile content, color palette, typography, or conversion behavior
- Pixel baselines or screenshot renderer work
- A mobile-sized desktop layout below the declared minimum

## Git workflow

- Branch: `improve/004-responsive-layout`
- Use one focused imperative commit. Stage only `src/main.rs`; do not push, merge,
  or edit `plans/README.md`.

## Steps

### Step 1: Define the supported width and pure layout mapping

Use one explicit supported minimum of 760×560 px. Clamp both restored dimensions
before opening the window, and set `WindowOptions`' native `window_min_size` from
the same width and height constants. Do not change the existing 900×640 defaults.

Add small pure functions that map viewport width and result visibility to:

- the ordered table columns;
- the remaining Name width;
- gallery columns from 1 through the existing maximum of 5.

At constrained widths, omit the decorative Bar column first; keep the numeric Weight
and Result columns because they carry the decision. At supported widths no
information-bearing column may disappear. Return columns in the current order.

**Verify**: table-driven unit tests cover the 760×560 minimum, current 900×640
default, 1100×720, and a wide window, both before and after results appear. Assert
the exact restored-size clamp, column lists, and gallery counts, not only that values
are nonzero.

### Step 2: Make the table query current width

Remove the one-time stored `name_width`. Let `AuditTable` read the current viewport
width from `Audit` (store the latest width during render if the delegate cannot access
`Window`) and derive its visible column list consistently in `columns_count`,
`column`, and `render_td`. No method may index the old full vector after another
method reported a compact count.

Name width must subtract the Result column only when results are shown and must never
drop below the existing 140 px minimum.

`TableState` caches its column groups. Store the last `(width band, results visible)`
layout signature in `Audit`; when render observes a transition, update that signature
and defer a borrow-safe `TableState::refresh(cx)` until after the current render. This
must cover both resize transitions and the first result arriving. Do not repeatedly
refresh when the signature is unchanged.

**Verify**: focused tests assert that `columns_count`, column specs, and cell mapping
use the same ordered list at compact and wide widths. A GPUI state test must exercise
an actual `TableState` through compact→wide and no-results→results transitions and
prove its cached columns are rebuilt.

### Step 3: Derive gallery bands from the same viewport

Replace fixed `TILE_COLUMNS` row/band math with the pure gallery column count. Keep
fixed tile dimensions so `uniform_list` retains stable row height and virtualization.
Recompute band count and indices from the chosen columns on every relevant render.

Do not make tile width fluid and do not remove virtualization. This is a pre-alpha
design: exact current pixel placement is not a compatibility contract, so adjust
spacing and wrapping inside `header`, `controls`, and `summary` when live compact
screenshots show clipping or illegible crowding.

**Verify**: unit tests prove every visible index appears exactly once for representative
entry counts at 1, 3, and 5 columns, including an incomplete final band.

### Step 4: Prove the release window at compact and wide widths

Run the release build. The executor records that browser/headless proof was skipped
because the known Linux renderer failure is outside scope. The main reviewer will
launch the real GPUI binary, control it through Hyprland, resize it to the declared
760×560 minimum, the current failing 873×720 size, and 1100×720, inspect list/grid
before and after results, and capture each state with `grim`.

**Verify**: `cargo build --release --locked` -> exit 0.

## Test plan

- Add pure layout tests in the existing `src/main.rs` test module.
- Assertions must include exact column identity/order and non-empty gallery bands.
- Do not use pixel snapshots or sleeps.

## Done criteria

- [ ] The native window minimum and restored-size clamp use one constant source.
- [ ] At minimum width, Name, Format, Size, bpp, Weight, and Result remain reachable.
- [ ] At wide width, the current decorative weight bar remains visible.
- [ ] Resizing recomputes Name width and the column set.
- [ ] Gallery columns adapt from 1 to 5 without duplicate or missing entries.
- [ ] `cargo test --locked`, clippy, format, and release build pass.
- [ ] Relative to the executor worktree's starting state, only `src/main.rs` differs;
  after its scoped commit, `git status --short` is empty.
- [ ] Reviewer screenshots at minimum, 873 px, and 1100 px show no clipped controls
  or information columns in list or grid.

## STOP conditions

- `DataTable` cannot safely change column count after construction.
- Correct resizing requires a fork or private access to gpui-component.
- Current controls do not fit at 760 px without a broader header/toolbar redesign.
- A compositor ignores native minimum size and the content still cannot remain
  usable through compact columns at that width.

## Maintenance notes

Information columns outrank the decorative bar. Any new table column must declare
its width priority and be added to the exact mapping tests. Keep gallery tile height
fixed unless virtualization is deliberately redesigned.
