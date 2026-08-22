# Plan 005: Report files that grow instead of calling them savings

> **Executor instructions**: Follow this plan in its isolated worktree, or after an
> approved plan 004 branch. Run every verification command. The reviewer maintains
> the plan index.
>
> **Drift check (run first)**:
> `git diff --stat 05384d3..HEAD -- src/main.rs`
> Stop if the summary state branches or per-row growth logic changed.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `05384d3`, 2026-08-20

## Why this matters

The real app at WebP q100 projects `70.7 KB now → ≈122.8 KB` but headlines that as
`0 B to save` with a green `−0%` tag and green meter. The comparison view correctly
reports `+82%`, and table rows already label grown outputs `larger`. The primary
summary must use the same truth: growth is an outcome, not a zero saving.

## Current state

- `src/main.rs:1460-1471`: completed totals use `saturating_sub`, always select a
  green tone, and derive the bar from `after / before`.
- `src/main.rs:1473-1485`: projected totals use the same saturating saving and green
  tone.
- `src/main.rs:1520-1525`: the tag clamps negative saving to zero, producing `−0%`.
- `src/main.rs:1978-1997`: per-row Result already branches on
  `converted > entry.bytes` and renders a warning `larger` tag. Match that language
  and semantic color.
- `src/main.rs:1103-1110`: comparison already renders positive growth with `+N%` in
  warning yellow.

## Commands you will need

| Purpose | Command | Expected on success |
|---------|---------|---------------------|
| Tests | `cargo test --locked` | exact saving/growth/unchanged tests pass |
| Lint | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |
| Build | `cargo build --release --locked` | exit 0 |

## Scope

**In scope**:
- `src/main.rs`

**Out of scope**:
- Encoder choices, quality defaults, estimate sampling, or conversion output
- Headless CLI reporting; its aggregate copy remains unchanged in this GUI-only plan
- Per-row Result redesign
- New charts, notifications, or recommendation engines
- README and screenshot harness changes

## Git workflow

- Branch: `improve/005-growth-summary` from the stated base.
- One focused imperative commit. Stage only `src/main.rs`; do not push, merge, or
  edit `plans/README.md`.

## Steps

### Step 1: Calculate signed size change once

Add one small pure presentation helper used by both projected and completed GUI
summary branches. For `before` and `after`, it must distinguish saving, growth, and
unchanged; return the absolute byte delta and an optional signed percentage. A zero
baseline has no percentage. A nonzero delta below 0.5% renders `+<1%` or `−<1%`,
never signed zero. Do not add a general statistics module or dependency.

**Verify**: table-driven tests cover 1000→750, 1000→1250, 1000→1000, 1000→999,
1000→1001, 0→0, and 0→100 with exact outcome, delta, and percentage-display
expectations.

### Step 2: Render each outcome in its real semantic state

- Projected saving: keep green `<bytes> to save`; completed saving keeps past-tense
  green `<bytes> saved`. Both use the green `−N%` tag and saving meter.
- Growth: show warning-yellow `<bytes> larger`, a `+N%` warning tag, and either a
  clearly labeled growth meter or no meter. For a zero baseline, omit the percentage
  tag. Never reuse the green success tag.
- Equal: show neutral `No size change`, no signed percentage tag, and no success
  meter.

Use the same rules for estimates and completed totals. Preserve detail text, Convert,
Clear, and Show output controls.

**Verify**: the pure presentation helper takes projected/completed phase and is used
by both branches. Assertions cover saving, growth, and unchanged for both phases,
including completed `saved`, projected `to save`, completed growth, tag variant, and
meter presence or suppression.

### Step 3: Prove both live states

Build release. The main reviewer launches the real app on a deterministic small
folder, captures WebP q100 growth and a lower-quality saving state through Hyprland +
`grim`, and inspects color, sign, headline, detail, meter, and button layout.

**Verify**: `cargo build --release --locked` -> exit 0.

## Test plan

- Keep pure helper tests in the existing `src/main.rs` test module.
- Exact cases: 1000→750, 1000→1250, 1000→1000, 1000→999, 1000→1001,
  0→0, and 0→100 across projected and completed phases.
- No snapshots or sleeps.

## Done criteria

- [ ] Projected and completed growth never render as zero savings.
- [ ] Saving, growth, and unchanged have correct sign, copy, semantic tone, and meter.
- [ ] Zero-byte inputs cannot divide by zero or produce NaN/negative zero.
- [ ] Tests, clippy, format, and release build pass.
- [ ] Only `src/main.rs` differs and the worktree is clean after commit.
- [ ] Reviewer screenshots prove one saving and one growth state in the real app.

## STOP conditions

- The plan would require changing estimate or conversion algorithms rather than
  presentation of their totals.
- A semantic warning tag cannot be rendered with the pinned component API.
- Summary structure no longer matches the stated base; stop for plan refresh instead
  of hand-merging.

## Maintenance notes

All GUI summary size comparisons must use the same signed helper. Future GUI
budget/spec features should not treat growth as a saturated zero saving.
