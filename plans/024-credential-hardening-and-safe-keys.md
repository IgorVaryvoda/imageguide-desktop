# Plan 024: Store credentials 0600 and reject unsafe remote keys

> **Executor instructions**: Follow this plan step by step. Run every
> verification command and confirm the expected result before moving on. If a
> STOP condition occurs, stop and report. When done, update the status row for
> this plan in `plans/README.md`.
>
> **Drift check (run first)**: `git diff --stat 030aca8..HEAD -- src/sirv.rs src/main.rs`
> On a mismatch with "Current state", STOP — unless your reviewer said earlier
> batch plans already landed; then verify semantically.

## Status

- **Priority**: P1
- **Effort**: S
- **Risk**: LOW
- **Depends on**: plans/023-truthful-credential-save.md (it reshapes the same
  save functions; land that first)
- **Category**: security
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

Two small hardening gaps in the new sync feature:

1. The credentials file (CDN client secret, Studio API key) is written with
   default permissions, so on Linux with a typical umask it is readable by
   other local users.
2. Pull writes `root.join(key)` where `key` comes straight from the remote
   listing via `unpair_remote`. Nothing rejects `..` or absolute components,
   so a hostile or compromised remote name could escape the paired folder.

## Current state

`src/sirv.rs:568-585` — after plan 023 this returns `io::Result<()>` but the
body still writes via plain `std::fs::write` with no mode setting:

```rust
pub fn save_credentials_at(base: impl AsRef<Path>, credentials: &Credentials) -> std::io::Result<()> {
    let path = store_path_in(base);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    ...
    std::fs::write(path, format!(...))
}
```

`src/sirv.rs:144-148`:

```rust
pub fn unpair_remote(dir: &str, filename: &str) -> Option<String> {
    let dir = dir.trim_end_matches('/');
    let prefix = format!("{dir}/");
    filename.strip_prefix(&prefix).map(str::to_string)
}
```

`src/main.rs:1587-1596` — the pull write step:

```rust
Ok(bytes) => {
    let target = root.join(key);
    let dirs_ok = target
        .parent()
        .is_none_or(|parent| std::fs::create_dir_all(parent).is_ok());
    dirs_ok && std::fs::write(&target, bytes).is_ok()
}
```

`pull_plan` (`src/sirv.rs:151-157`) produces the keys `start_pull` iterates.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Fast check | `cargo check --locked` | exit 0 |
| Tests | `cargo test --bin imageguide --locked` | all pass, 1 ignored |
| Clippy | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/sirv.rs` (save permissions, a new `safe_key` helper, tests)
- `src/main.rs` (the pull write step only)

**Out of scope**:
- Push keys: `relative_key` derives them from scanned local paths via
  `strip_prefix`, so `..` cannot occur there. Do not touch push.
- Any change to what the credentials file contains.

## Git workflow

Commit on the reviewer's worktree branch, e.g.
`fix: tighten credential file permissions and validate remote keys`.

## Steps

### Step 1: Write the store 0600 (unix)

In `save_credentials_at`, replace `std::fs::write(path, body)` with a mode
setting write, keeping the same body string:

```rust
let body = format!(...unchanged...);
#[cfg(unix)]
{
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)?;
    file.write_all(body.as_bytes())?;
    // An older file may carry wider permissions from a previous version.
    let mut perms = std::fs::metadata(&path)?.permissions();
    use std::os::unix::fs::PermissionsExt as _;
    perms.set_mode(0o600);
    std::fs::set_permissions(&path, perms)?;
}
#[cfg(not(unix))]
std::fs::write(&path, body)?;
Ok(())
```

### Step 2: Add the key validator

Near `unpair_remote`:

```rust
/// A remote key is safe to join onto the local root only when every path
/// component is a normal name: no `..`, no absolute remainder.
pub fn safe_key(key: &str) -> bool {
    let path = Path::new(key);
    !key.is_empty()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}
```

### Step 3: Use it where remote keys become file writes

- In `pull_plan`, add `.filter(|key| safe_key(key))` after the existing
  `!local_keys.contains` filter.
- In `start_pull`'s write step (`src/main.rs:1589`), guard the join:

```rust
Ok(bytes) => {
    let written = sirv::safe_key(key) && {
        let target = root.join(key);
        ...unchanged...
    };
    written
}
```

(The belt-and-braces guard stays even though `pull_plan` filters, because the
plan and the write live in different async steps.)

### Step 4: Tests

In `src/sirv.rs` tests:

```rust
#[test]
fn unsafe_remote_keys_are_rejected() {
    assert!(safe_key("a.jpg"));
    assert!(safe_key("sub/a.jpg"));
    assert!(!safe_key("../evil.jpg"));
    assert!(!safe_key("sub/../../evil.jpg"));
    assert!(!safe_key("/abs/a.jpg"));
    assert!(!safe_key(""));
}

#[test]
fn the_pull_plan_skips_traversal_keys() {
    let remote = vec![node("/d/../evil.jpg"), node("/d/ok.jpg")];
    let local: HashSet<String> = [].into();
    assert_eq!(pull_plan(&remote, "/d", &local), vec!["ok.jpg".to_string()]);
}
```

Build `node(filename)` with a small local helper or inline the struct literal
like the existing `pull_plan` test does (`src/sirv.rs:680-703`).

Permissions test (unix only, follows the round-trip test's temp-dir pattern):

```rust
#[cfg(unix)]
#[test]
fn the_credential_store_is_private() {
    use std::os::unix::fs::PermissionsExt as _;
    let base = std::env::temp_dir().join(format!("imageguide-sirv-perm-{}", std::process::id()));
    save_credentials_at(&base, &Credentials { client_id: "i".into(), client_secret: "s".into(), studio_key: None }).unwrap();
    let path = store_path_in(&base);
    let mode = std::fs::metadata(&path).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let _ = std::fs::remove_dir_all(&base);
}
```

**Verify**: `cargo test --bin imageguide --locked` → all pass.

## Done criteria

- [ ] `rg -n "std::fs::write" src/sirv.rs` shows the write behind a 0600 mode (unix)
- [ ] `rg -n "safe_key" src/` shows the helper plus both use sites
- [ ] New tests pass; suite, clippy, fmt green
- [ ] No files outside `src/sirv.rs` and `src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- Plan 023 has not landed (save functions still return `()`), and your
  reviewer did not tell you to proceed anyway.
- `pull_plan` or the pull write step do not match the excerpts.

## Maintenance notes

If a future feature writes any other file next to the settings, apply the
same 0600 rule for anything secret-bearing. If Sirv ever normalises `..` in
filenames server-side, the validator is still correct — it only narrows.
