# Plan 023: Report credential-save failures instead of "Saved."

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
- **Depends on**: none
- **Category**: bug
- **Planned at**: commit `030aca8`, 2026-08-22

## Why this matters

Both credential writers discard I/O errors (`let _ = std::fs::write`), and the
settings panel then shows a green "Saved." unconditionally. On a read-only
config directory or a full disk the user loses their credentials silently and
the next connect fails mysteriously. The repo's truthfulness rule ("Truthful
UI" in AGENTS.md) forbids reporting success that did not happen.

## Current state

`src/sirv.rs:560-585`:

```rust
// The settings panel writes credentials directly; the tests keep the file
// format from drifting.
pub fn save_credentials(credentials: &Credentials) {
    if let Some(path) = store_path() {
        save_credentials_at(path, credentials);
    }
}

pub fn save_credentials_at(base: impl AsRef<Path>, credentials: &Credentials) {
    let path = store_path_in(base);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let studio_line = credentials
        .studio_key
        .as_ref()
        .map(|key| format!("studio_key={key}\n"))
        .unwrap_or_default();
    let _ = std::fs::write(
        path,
        format!(
            "client_id={}\nclient_secret={}\n{}",
            credentials.client_id, credentials.client_secret, studio_line
        ),
    );
}
```

`src/main.rs:1426-1445` (`save_sirv_settings`), the success path:

```rust
let studio_key = sirv::load_credentials().and_then(|creds| creds.studio_key);
sirv::save_credentials(&sirv::Credentials { client_id, client_secret, studio_key });
panel.cdn_status = Some((true, "Saved.".into()));
cx.notify();
```

`src/main.rs:1480-1482` (`connect_studio` success arm) also writes:

```rust
let mut stored = sirv::load_credentials().unwrap_or(credentials);
stored.studio_key = Some(identity.api_key);
sirv::save_credentials(&stored);
```

Existing test `credentials_round_trip_through_the_store` (`src/sirv.rs:649+`)
calls `save_credentials_at(&base, &credentials);` without handling a result.

## Commands you will need

| Purpose | Command | Expected |
|---------|---------|----------|
| Fast check | `cargo check --locked` | exit 0 |
| Tests | `cargo test --bin imageguide --locked` | all pass, 1 ignored |
| Clippy | `cargo clippy --all-targets -- -D warnings` | exit 0 |
| Format | `cargo fmt --check` | exit 0 |

## Scope

**In scope**:
- `src/sirv.rs` (the two save functions + their test)
- `src/main.rs` (the two call sites named above)

**Out of scope**:
- `load_credentials` / `load_credentials_from`
- The file format (keys and layout stay byte-identical)

## Git workflow

Commit on the reviewer's worktree branch, e.g.
`fix: surface credential-save failures`.

## Steps

### Step 1: Return results from the writers

```rust
pub fn save_credentials(credentials: &Credentials) -> std::io::Result<()> {
    match store_path() {
        Some(path) => save_credentials_at(path, credentials),
        // No resolvable config home: report it rather than pretend to save.
        None => Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no config directory",
        )),
    }
}

pub fn save_credentials_at(base: impl AsRef<Path>, credentials: &Credentials) -> std::io::Result<()> {
    let path = store_path_in(base);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let studio_line = ...unchanged...;
    std::fs::write(path, format!(...unchanged...))
}
```

### Step 2: Propagate in the settings panel

`save_sirv_settings`:

```rust
match sirv::save_credentials(&sirv::Credentials { client_id, client_secret, studio_key }) {
    Ok(()) => panel.cdn_status = Some((true, "Saved.".into())),
    Err(error) => panel.cdn_status = Some((false, format!("Could not save: {error}"))),
}
```

`connect_studio` success arm:

```rust
stored.studio_key = Some(identity.api_key);
match sirv::save_credentials(&stored) {
    Ok(()) => panel.studio_status = Some((true, format!("Connected · {} · {} credits", identity.tier, identity.credits))),
    Err(error) => panel.studio_status = Some((false, format!("Connected, but the key could not be saved: {error}"))),
}
```

(The identity is still valid in memory; only persistence failed — say so.)

### Step 3: Update the existing test

In `credentials_round_trip_through_the_store`, both `save_credentials_at`
calls become `save_credentials_at(&base, &credentials).unwrap();` (and the
`linked` one likewise).

### Step 4: Add a failure test

```rust
#[test]
fn saving_into_an_unwritable_directory_reports_an_error() {
    let blocker = std::env::temp_dir().join(format!("imageguide-sirv-nodir-{}", std::process::id()));
    // A file where the directory should be makes create_dir_all fail.
    std::fs::write(&blocker, "not a directory").unwrap();
    let result = save_credentials_at(&blocker, &Credentials {
        client_id: "id".into(),
        client_secret: "secret".into(),
        studio_key: None,
    });
    assert!(result.is_err());
    let _ = std::fs::remove_file(&blocker);
}
```

**Verify**: `cargo test --bin imageguide --locked` → all pass including the
new test; `cargo clippy --all-targets -- -D warnings` → exit 0.

## Done criteria

- [ ] `rg -n "let _ = std::fs::write|let _ = std::fs::create_dir_all" src/sirv.rs` → no matches
- [ ] Both save functions return `std::io::Result<()>`
- [ ] Suite, clippy, fmt green
- [ ] No files outside `src/sirv.rs` and `src/main.rs` modified
- [ ] `plans/README.md` status row updated

## STOP conditions

- The save functions or call sites do not match the excerpts.
- `store_path()` returning `None` is already handled somewhere you cannot see.

## Maintenance notes

Any new writer of the credentials file must return and surface the result.
Reviewers should confirm the file format is unchanged (diff the format string).
