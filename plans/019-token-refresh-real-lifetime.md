# Plan 019: Refresh the Sirv token from its real lifetime

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving to the
> next step. If any STOP condition occurs, stop and report — do not improvise.
> When done, update the status row for this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 030aca8..HEAD -- src/sirv.rs`
> On a mismatch with "Current state", STOP.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: none
- **Category**: perf / bug
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

The client stores `Instant::now() + expires_in` as the token's second tuple
element but compares it with `.elapsed()`. Elapsed time on a *future* instant
saturates to zero, so `elapsed() < 19 min` is always true and the proactive
refresh branch can never fire. Every request made after the server's ~20
minute token window pays a wasted 401 round trip plus a re-login — doubling
call latency for exactly the long push/pull runs this feature exists for.

## Current state

`src/sirv.rs:194-247` (excerpted):

```rust
pub struct Client {
    credentials: Credentials,
    token: Option<(String, Instant)>,
    agent: ureq::Agent,
}

impl Client {
    /// A valid token, fetching or refreshing one when needed.
    fn token(&mut self) -> Result<String, Error> {
        if let Some((token, fetched_at)) = &self.token
            && fetched_at.elapsed() < Duration::from_secs(19 * 60) - TOKEN_MARGIN
        {
            return Ok(token.clone());
        }
        self.fetch_token()
    }
    ...
    fn fetch_token(&mut self) -> Result<String, Error> {
        ...
        self.token = Some((
            issued.token.clone(),
            Instant::now() + Duration::from_secs(issued.expires_in),  // <-- future instant stored as "fetched_at"
        ));
```

`TOKEN_MARGIN` is `Duration::from_secs(60)` (`src/sirv.rs:22`). The 401 retry
in `authenticated` (`src/sirv.rs:252-263`) currently masks the bug.

Conventions: comments explain why; parking_lot is the house mutex; no config
crates. Tests live in `#[cfg(test)] mod tests` at the bottom of `src/sirv.rs`
(`src/sirv.rs:587+`) and are plain `#[test]` fns.

## Commands you will need

| Purpose | Command                                    | Expected on success |
|---------|--------------------------------------------|---------------------|
| Fast check | `cargo check --locked`                   | exit 0              |
| Tests   | `cargo test --bin imageguide --locked`     | all pass, 1 ignored |
| Clippy  | `cargo clippy --all-targets -- -D warnings` | exit 0             |
| Format  | `cargo fmt --check`                         | exit 0            |

## Scope

**In scope**:
- `src/sirv.rs`

**Out of scope**:
- The `authenticated` 401 retry — it stays; it covers expiry between check and use.
- Anything network-related beyond the two functions named here.

## Git workflow

Commit on the reviewer's worktree branch. Style: conventional subject, e.g.
`fix: refresh the Sirv token before it expires`.

## Steps

### Step 1: Store the real lifetime next to the fetch time

Change the field to carry the issued lifetime:

```rust
token: Option<(String, Instant, Duration)>,  // token, fetched at, server lifetime
```

In `fetch_token`, store `(issued.token.clone(), Instant::now(), Duration::from_secs(issued.expires_in))`.

### Step 2: Extract and fix the freshness test

Add a free function:

```rust
/// True while a token fetched at `fetched_at` with lifetime `ttl` may still
/// be used, keeping TOKEN_MARGIN of headroom.
fn token_fresh(fetched_at: Instant, ttl: Duration) -> bool {
    fetched_at.elapsed() < ttl.saturating_sub(TOKEN_MARGIN)
}
```

`saturating_sub` replaces the panicking-prone `-` on Durations. Use it in
`Client::token`:

```rust
if let Some((token, fetched_at, ttl)) = &self.token
    && token_fresh(*fetched_at, *ttl)
{
    return Ok(token.clone());
}
```

Note: `Instant::now() + ttl` must no longer appear anywhere in the file.

**Verify**: `cargo check --locked` → exit 0.

### Step 3: Test the seam

In `mod tests`, add:

```rust
#[test]
fn a_token_from_the_past_expires_its_margin_early() {
    let stale = Instant::now() - Duration::from_secs(20 * 60);
    assert!(!token_fresh(stale, Duration::from_secs(1200)));
    assert!(token_fresh(Instant::now(), Duration::from_secs(1200)));
}

#[test]
fn a_lifetime_shorter_than_the_margin_is_never_fresh() {
    assert!(!token_fresh(Instant::now(), Duration::from_secs(30)));
}
```

This is the regression test: under the old code the first assertion was
impossible to reach because the stored instant was in the future.

**Verify**: `cargo test --bin imageguide --locked` → both new tests pass.

## Done criteria

- [ ] `rg -n "Instant::now\(\) \+ Duration" src/sirv.rs` returns nothing
- [ ] Both new tests pass; full suite green
- [ ] clippy and fmt clean
- [ ] No files outside `src/sirv.rs` modified

## STOP conditions

- `fetch_token` or `token` do not match the excerpts.
- The `Issued` struct has no `expires_in` field.

## Maintenance notes

If Sirv changes its default token lifetime, nothing needs editing — the
server value drives refresh now. Reviewers should confirm the 401 retry path
still exists untouched; both layers are intentional.
