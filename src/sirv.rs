//! Sirv REST access for folder sync.
//!
//! The smallest surface the sync feature needs: one token cache, one directory
//! read, and the pure helpers the diff view classifies with. Everything runs
//! blocking and belongs on a background executor; nothing here touches gpui.
//!
//! Secrets live in their own file next to the window settings, because the
//! window settings are rewritten on every viewport change and must never be
//! the thing that silently drops a credential.

use serde::Deserialize;
use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const API: &str = "https://api.sirv.com";
/// The AI Studio platform, a separate service with its own key material.
const STUDIO_API: &str = "https://www.sirv.studio";
/// Tokens live 20 minutes on the server. Refresh a minute early so an upload
/// started at minute 19 does not die mid-flight.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);
const TIMEOUT: Duration = Duration::from_secs(30);
/// File transfers get their own, much looser ceiling: a photo shoot folder
/// holds files that legitimately take minutes.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(600);
#[derive(Clone, Debug, PartialEq)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
    /// The AI Studio API key, minted by exchanging the CDN credentials. Absent
    /// until the user connects Studio.
    pub studio_key: Option<String>,
}

/// A hard cap on one transfer, so a confused server cannot grow memory forever.
const MAX_TRANSFER: u64 = 512 * 1024 * 1024;
/// A walk that finds more files than this is treated as an error rather than
/// listed forever.
const WALK_LIMIT: usize = 20_000;
/// What Studio reports about the account behind an API key.
#[derive(Clone, Debug, Deserialize)]
pub struct StudioIdentity {
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub credits: u64,
    #[serde(default)]
    pub tier: String,
}

/// One entry as Sirv reports it from readdir. Unknown fields are ignored, so a
/// server-side addition never breaks the parse.
#[derive(Clone, Debug, Deserialize)]
pub struct Node {
    #[serde(default)]
    pub filename: String,
    /// `"file"`, `"folder"` or `"symlink"`.
    #[serde(default)]
    pub r#type: String,
    #[serde(default)]
    pub size: u64,
}

impl Node {
    pub fn is_folder(&self) -> bool {
        self.r#type == "folder"
    }
}

/// An upstream failure with its status and body kept intact. "Sirv said 403"
/// is debuggable; "request failed" is not.
#[derive(Debug)]
pub struct Error {
    pub status: u16,
    pub message: String,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sirv {}: {}", self.status, self.message)
    }
}

impl std::error::Error for Error {}

/// Percent-encode a path for a Sirv query string. Everything outside the
/// unreserved set escapes, including `/` as `%2F`, which is what the API docs
/// show for `filename` and `dirname` parameters.
pub fn encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for byte in path.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// How a local file stands against the paired remote folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// No file with this relative path on Sirv.
    OnlyLocal,
    /// Same relative path and byte size.
    Same,
    /// Same path, different size.
    Changed,
}

/// Classify one local file against the remote listing. Size is the only
/// comparator on purpose: local and server clocks disagree often enough that
/// mtime comparison would report lies as changes.
pub fn classify(local_size: u64, remote: Option<&Node>) -> SyncState {
    match remote {
        None => SyncState::OnlyLocal,
        Some(node) if node.size == local_size => SyncState::Same,
        Some(_) => SyncState::Changed,
    }
}

/// The key a local file carries inside the paired folder: its path below
/// `root`, forward-slashed, so `/photos/a.jpg` under `/photos` becomes
/// `a.jpg`. `None` when the file sits outside the root, which cannot happen
/// for scanned entries but keeps the function total.
pub fn relative_key(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let mut key = String::new();
    for component in relative.components() {
        if !key.is_empty() {
            key.push('/');
        }
        key.push_str(&component.as_os_str().to_string_lossy());
    }
    Some(key)
}

/// Strip the paired folder off a remote filename so both sides of the diff
/// speak the same key language. `/photos/a.jpg` paired at `/photos` is
/// `a.jpg`; anything outside the pair is skipped by the caller via `None`.
pub fn unpair_remote(dir: &str, filename: &str) -> Option<String> {
    let dir = dir.trim_end_matches('/');
    let prefix = format!("{dir}/");
    filename.strip_prefix(&prefix).map(str::to_string)
}

/// True when a remote key is safe to join onto a local folder.
///
/// A pull turns a name the server chose into a path this machine writes to. Rust's
/// `Path::join` replaces the whole path when handed an absolute one, so a listing
/// entry of `/etc/cron.d/x` would have written to `/etc/cron.d/x`, and `..` would have
/// climbed out of the paired folder. Neither is something a Sirv account is expected
/// to contain; both are cheap to refuse, and this is the boundary to refuse them at.
pub fn safe_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('/')
        && !key.starts_with('\\')
        // A Windows drive or UNC prefix is absolute too, and `..` at any depth climbs.
        && !Path::new(key).has_root()
        && Path::new(key)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

/// Relative keys on Sirv that no local file claims: the pull list.
pub fn pull_plan(remote: &[Node], dir: &str, local_keys: &HashSet<String>) -> Vec<String> {
    remote
        .iter()
        .filter_map(|node| unpair_remote(dir, &node.filename))
        .filter(|key| !local_keys.contains(key))
        .filter(|key| safe_key(key))
        .collect()
}

/// The ancestor folders a relative key needs, in creation order:
/// `sub/deep/a.jpg` gives `["sub", "sub/deep"]`.
pub fn ancestor_dirs(key: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = String::new();
    let parts: Vec<&str> = key.split('/').collect();
    for part in &parts[..parts.len().saturating_sub(1)] {
        if !seen.is_empty() {
            seen.push('/');
        }
        seen.push_str(part);
        out.push(seen.clone());
    }
    out
}

/// Every folder a push needs, each named once, shallowest first.
///
/// Ensuring a key's ancestors inside its own upload task meant one round trip per
/// ancestor per file: 2,000 photos two levels deep spent 4,000 requests re-creating
/// the same two folders.
pub fn push_folders(keys: impl IntoIterator<Item = impl AsRef<str>>) -> Vec<String> {
    let mut folders = Vec::new();
    let mut seen = HashSet::new();
    for key in keys {
        for ancestor in ancestor_dirs(key.as_ref()) {
            if seen.insert(ancestor.clone()) {
                folders.push(ancestor);
            }
        }
    }
    // A parent has to exist before its child, and `ancestor_dirs` already emits each
    // chain in order; sorting by depth keeps that true once chains interleave.
    folders.sort_by_key(|folder| folder.matches('/').count());
    folders
}

/// The Content-Type an upload declares. Sirv sniffs images anyway; declaring
/// correctly keeps the API honest about what it stored.
pub fn content_type(key: &str) -> &'static str {
    match key
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "gif" => "image/gif",
        _ => "application/octet-stream",
    }
}

pub struct Client {
    credentials: Credentials,
    token: Option<(String, Instant)>,
    agent: ureq::Agent,
}

impl Client {
    pub fn new(credentials: Credentials) -> Self {
        Self {
            credentials,
            token: None,
            agent: ureq::AgentBuilder::new().timeout(TIMEOUT).build(),
        }
    }

    /// A valid token, fetching or refreshing one when needed.
    fn token(&mut self) -> Result<String, Error> {
        if let Some((token, fetched_at)) = &self.token
            && fetched_at.elapsed() < Duration::from_secs(19 * 60) - TOKEN_MARGIN
        {
            return Ok(token.clone());
        }
        self.fetch_token()
    }

    fn fetch_token(&mut self) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct Issued {
            token: String,
            #[serde(rename = "expiresIn", default = "default_expiry")]
            expires_in: u64,
        }
        fn default_expiry() -> u64 {
            1200
        }

        let response = self
            .agent
            .post(&format!("{API}/v2/token"))
            .send_json(serde_json::json!({
                "clientId": self.credentials.client_id,
                "clientSecret": self.credentials.client_secret,
            }))
            .map_err(sirv_error("token request"))?;
        let issued: Issued = response.into_json().map_err(|error| Error {
            status: 0,
            message: format!("token body: {error}"),
        })?;
        self.token = Some((
            issued.token.clone(),
            Instant::now() + Duration::from_secs(issued.expires_in),
        ));
        Ok(issued.token)
    }

    /// Run one authenticated call. A token that expired between check and use
    /// is routine: refresh once and try again rather than surfacing a login
    /// error.
    fn authenticated<T>(
        &mut self,
        call: impl Fn(&mut Self) -> Result<T, Error>,
    ) -> Result<T, Error> {
        match call(self) {
            Err(Error { status: 401, .. }) => {
                self.token = None;
                call(self)
            }
            other => other,
        }
    }

    fn bearer(&mut self) -> Result<String, Error> {
        Ok(format!("Bearer {}", self.token()?))
    }

    /// One directory listing. Folder names come back absolute
    /// (`/photos/sub`); files carry their byte size.
    pub fn readdir(&mut self, dirname: &str) -> Result<Vec<Node>, Error> {
        let url = format!("{API}/v2/files/readdir?dirname={}", encode_path(dirname));
        self.authenticated(|client| {
            let authorization = client.bearer()?;
            let response = client
                .agent
                .get(&url)
                .set("Authorization", &authorization)
                .call()
                .map_err(|error| match error {
                    ureq::Error::Status(status, _) => Error {
                        status,
                        message: "readdir rejected".into(),
                    },
                    other => sirv_error("readdir")(other),
                })?;
            response.into_json().map_err(|error| Error {
                status: 0,
                message: format!("readdir body: {error}"),
            })
        })
    }

    /// Every file below `dir`, flattened, folders walked depth-first. Bounded:
    /// a tree that exceeds `WALK_LIMIT` files is an error, not an endless walk.
    pub fn walk(&mut self, dir: &str) -> Result<Vec<Node>, Error> {
        let mut all = Vec::new();
        let mut stack = vec![dir.to_string()];
        while let Some(current) = stack.pop() {
            for node in self.readdir(&current)? {
                if node.is_folder() {
                    stack.push(node.filename.clone());
                } else {
                    all.push(node);
                }
            }
            if all.len() > WALK_LIMIT {
                return Err(Error {
                    status: 0,
                    message: format!("folder holds more than {WALK_LIMIT} files; sync it in parts"),
                });
            }
        }
        Ok(all)
    }

    /// One file's bytes.
    pub fn download(&mut self, filename: &str) -> Result<Vec<u8>, Error> {
        let url = format!("{API}/v2/files/download?filename={}", encode_path(filename));
        self.authenticated(|client| {
            let authorization = client.bearer()?;
            let response = client
                .agent
                .get(&url)
                .set("Authorization", &authorization)
                .timeout(TRANSFER_TIMEOUT)
                .call()
                .map_err(|error| match error {
                    ureq::Error::Status(status, _) => Error {
                        status,
                        message: "download rejected".into(),
                    },
                    other => sirv_error("download")(other),
                })?;
            let mut bytes = Vec::new();
            response
                .into_reader()
                .take(MAX_TRANSFER)
                .read_to_end(&mut bytes)
                .map_err(|error| Error {
                    status: 0,
                    message: format!("download body: {error}"),
                })?;
            Ok(bytes)
        })
    }

    /// Put bytes at `filename`, creating nothing on the way — the caller makes
    /// folders explicitly so a partial push is visible in the listing.
    pub fn upload(
        &mut self,
        filename: &str,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<(), Error> {
        let url = format!("{API}/v2/files/upload?filename={}", encode_path(filename));
        let body = bytes.to_vec();
        self.authenticated(|client| {
            let authorization = client.bearer()?;
            client
                .agent
                .post(&url)
                .set("Authorization", &authorization)
                .set("Content-Type", content_type)
                .timeout(TRANSFER_TIMEOUT)
                .send_bytes(&body)
                .map_err(|error| match error {
                    ureq::Error::Status(status, _) => Error {
                        status,
                        message: "upload rejected".into(),
                    },
                    other => sirv_error("upload")(other),
                })?;
            Ok(())
        })
    }

    /// Create a folder. The one that already exists is success, not conflict:
    /// pushes re-check ancestors for every file.
    pub fn mkdir(&mut self, dirname: &str) -> Result<(), Error> {
        let url = format!("{API}/v2/files/mkdir?dirname={}", encode_path(dirname));
        self.authenticated(|client| {
            let authorization = client.bearer()?;
            match client
                .agent
                .post(&url)
                .set("Authorization", &authorization)
                .call()
            {
                Ok(_) => Ok(()),
                Err(ureq::Error::Status(409, _)) => Ok(()),
                Err(ureq::Error::Status(status, _)) => Err(Error {
                    status,
                    message: "mkdir rejected".into(),
                }),
                Err(other) => Err(sirv_error("mkdir")(other)),
            }
        })
    }

    /// The account alias this credential pair belongs to (`myaccount` for
    /// `myaccount.sirv.com`). Studio's exchange wants it; making a human type
    /// it was wrong, because the API hands it back for free.
    pub fn account_alias(&mut self) -> Result<String, Error> {
        #[derive(Deserialize)]
        struct Account {
            #[serde(default)]
            alias: String,
        }
        let account: Account = self.authenticated(|client| {
            let authorization = client.bearer()?;
            let response = client
                .agent
                .get(&format!("{API}/v2/account"))
                .set("Authorization", &authorization)
                .call()
                .map_err(|error| match error {
                    ureq::Error::Status(status, _) => Error {
                        status,
                        message: "account rejected".into(),
                    },
                    other => sirv_error("account")(other),
                })?;
            response.into_json().map_err(|error| Error {
                status: 0,
                message: format!("account body: {error}"),
            })
        })?;
        Ok(account.alias)
    }

    /// Trade the CDN credentials for an AI Studio API key. The account alias
    /// comes from the CDN API, not the user. Studio creates or links the
    /// account behind `email`, which is why the email is required.
    pub fn exchange_studio_key(&mut self, email: &str) -> Result<StudioIdentity, Error> {
        let account_alias = self.account_alias()?;
        let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
        let response = agent
            .post(&format!("{STUDIO_API}/api/auth/wordpress"))
            .send_json(serde_json::json!({
                "email": email,
                "clientId": self.credentials.client_id,
                "clientSecret": self.credentials.client_secret,
                "accountAlias": account_alias,
            }))
            .map_err(sirv_error("studio connect"))?;
        response.into_json().map_err(|error| Error {
            status: 0,
            message: format!("studio body: {error}"),
        })
    }

    /// Confirm a stored key still works, and what it carries.
    pub fn studio_me(api_key: &str) -> Result<StudioIdentity, Error> {
        let agent = ureq::AgentBuilder::new().timeout(TIMEOUT).build();
        let response = agent
            .get(&format!("{STUDIO_API}/api/zapier/me"))
            .set("Authorization", &format!("Bearer {api_key}"))
            .call()
            .map_err(|error| match error {
                ureq::Error::Status(status, _) => Error {
                    status,
                    message: "studio check rejected".into(),
                },
                other => sirv_error("studio check")(other),
            })?;
        response.into_json().map_err(|error| Error {
            status: 0,
            message: format!("studio body: {error}"),
        })
    }
}

fn sirv_error(stage: &'static str) -> impl Fn(ureq::Error) -> Error {
    move |error| match error {
        ureq::Error::Status(status, response) => {
            // Bodies arrive pretty-printed; the first line alone would be a
            // bare "{". Keep everything, capped.
            let mut message = response.into_string().unwrap_or_default();
            message = message.trim().to_string();
            if message.chars().count() > 200 {
                message = message.chars().take(200).collect::<String>() + "…";
            }
            Error {
                status,
                message: if message.is_empty() {
                    stage.to_string()
                } else {
                    format!("{stage}: {message}")
                },
            }
        }
        ureq::Error::Transport(transport) => Error {
            status: 0,
            message: format!("{stage}: {transport}"),
        },
    }
}

// ── Credential store ────────────────────────────────────────────────────────

/// The file a user edits to add credentials, named in errors.
pub fn credentials_path() -> Option<PathBuf> {
    store_path()
}

/// Where the Sirv credentials live, resolved like the window settings file.
/// `IMAGEGUIDE_CONFIG_DIR` overrides the platform base, which is how tests
/// keep their hands off a real credentials file.
fn store_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("IMAGEGUIDE_CONFIG_DIR") {
        return Some(store_path_in(PathBuf::from(dir)));
    }
    let base = if cfg!(target_os = "windows") {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    }?;
    Some(store_path_in(&base))
}

fn store_path_in(base: impl AsRef<Path>) -> PathBuf {
    base.as_ref().join("imageguide").join("sirv")
}

pub fn load_credentials() -> Option<Credentials> {
    load_credentials_from(store_path().as_deref())
}

pub fn load_credentials_from(path: Option<&Path>) -> Option<Credentials> {
    parse_credentials(&std::fs::read_to_string(path?).ok()?)
}

fn parse_credentials(text: &str) -> Option<Credentials> {
    let mut client_id = None;
    let mut client_secret = None;
    let mut studio_key = None;
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "client_id" => client_id = Some(value.trim().to_string()),
            "client_secret" => client_secret = Some(value.trim().to_string()),
            "studio_key" => studio_key = Some(value.trim().to_string()),
            _ => {}
        }
    }
    Some(Credentials {
        client_id: client_id?,
        client_secret: client_secret?,
        studio_key,
    })
}

// The settings panel writes credentials directly; the tests keep the file
// format from drifting.
/// Store the credentials, or say why not.
///
/// A `Result`, because the window used to report "Saved." whether or not anything
/// reached the disk. A full disk or a read-only config directory looked exactly like
/// success, and the user found out the next time the app started with no keys.
pub fn save_credentials(credentials: &Credentials) -> Result<(), String> {
    let Some(path) = store_path() else {
        return Err("no config directory on this system".into());
    };
    write_credentials(&path, credentials)
}

/// Write the credentials file itself.
///
/// Both callers used to go through `save_credentials_at`, which takes a *base* and
/// joins `imageguide/sirv` onto it — but `save_credentials` handed it `store_path()`,
/// which is already the file. Credentials landed in
/// `<base>/imageguide/sirv/imageguide/sirv` while `load_credentials` read
/// `<base>/imageguide/sirv`, so the settings panel said "Saved." and the keys were
/// gone by the next launch. Taking the finished path here leaves nowhere for the two
/// to disagree.
fn write_credentials(path: &Path, credentials: &Credentials) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let studio_line = credentials
        .studio_key
        .as_ref()
        .map(|key| format!("studio_key={key}\n"))
        .unwrap_or_default();
    let body = format!(
        "client_id={}\nclient_secret={}\n{}",
        credentials.client_id, credentials.client_secret, studio_line
    );
    std::fs::write(path, body).map_err(|error| error.to_string())?;
    owner_only(path);
    Ok(())
}

/// Take the group and world bits off a file. An API secret written under the usual
/// umask is 0644, which every other account on the machine can read.
#[cfg(unix)]
fn owner_only(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// Windows and macOS keep their own defaults. Windows has no mode bits, and a macOS
/// home directory is already private to its owner.
#[cfg(not(unix))]
fn owner_only(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_escape_for_query_strings() {
        assert_eq!(encode_path("/a b/c.jpg"), "%2Fa%20b%2Fc.jpg");
        assert_eq!(encode_path("/plain/file.webp"), "%2Fplain%2Ffile.webp");
        assert_eq!(encode_path("-_.~"), "-_.~");
    }

    #[test]
    fn classification_covers_the_three_states() {
        let node = Node {
            filename: "/d/a.png".into(),
            r#type: "file".into(),
            size: 100,
        };
        assert_eq!(classify(100, Some(&node)), SyncState::Same);
        assert_eq!(classify(101, Some(&node)), SyncState::Changed);
        assert_eq!(classify(100, None), SyncState::OnlyLocal);
    }

    #[test]
    fn relative_keys_use_forward_slashes() {
        let root = Path::new("/photos");
        assert_eq!(
            relative_key(root, Path::new("/photos/sub/a.jpg")),
            Some("sub/a.jpg".into())
        );
        assert_eq!(relative_key(root, Path::new("/elsewhere/a.jpg")), None);
    }

    #[test]
    fn remote_names_unpair_against_the_folder() {
        assert_eq!(
            unpair_remote("/photos", "/photos/sub/a.jpg"),
            Some("sub/a.jpg".into())
        );
        assert_eq!(
            unpair_remote("/photos/", "/photos/a.jpg"),
            Some("a.jpg".into())
        );
        assert_eq!(unpair_remote("/photos", "/other/a.jpg"), None);
    }

    #[test]
    fn readdir_parses_files_and_folders_leniently() {
        let nodes: Vec<Node> = serde_json::from_str(
            r#"[
                {"type":"folder","filename":"/photos/sub","mtime":"2026-01-01T00:00:00Z"},
                {"type":"file","filename":"/photos/a.jpg","size":1234,"width":80,"height":60},
                {"type":"file","filename":"/photos/b.png","size":0}
            ]"#,
        )
        .unwrap();
        assert_eq!(nodes.len(), 3);
        assert!(nodes[0].is_folder());
        assert_eq!(nodes[1].size, 1234);
        assert_eq!(nodes[2].filename, "/photos/b.png");
    }

    #[test]
    fn credentials_round_trip_through_the_store() {
        // The resolver is environment-shaped; the round trip runs against a
        // temp base so a developer's real credentials file is never touched.
        let base =
            std::env::temp_dir().join(format!("imageguide-sirv-test-{}", std::process::id()));
        let path = store_path_in(&base);

        assert_eq!(load_credentials_from(Some(&path)), None);
        let credentials = Credentials {
            client_id: "an id with spaces".into(),
            client_secret: "s3cret/with:colons".into(),
            studio_key: None,
        };
        write_credentials(&path, &credentials).expect("the store is writable");
        assert_eq!(
            load_credentials_from(Some(&path)),
            Some(credentials.clone())
        );

        // A minted Studio key survives its credential file.
        let linked = Credentials {
            studio_key: Some("sk_live_abc".into()),
            ..credentials.clone()
        };
        write_credentials(&path, &linked).expect("the store is writable");
        assert_eq!(load_credentials_from(Some(&path)), Some(linked));

        // The file holds an API secret, so it must not be readable by anyone else on
        // the machine. The usual umask would have written it 0644.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "group or world can read {path:?}");
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The pair the old test never put together. `save_credentials_at` and
    /// `load_credentials_from` agreed with each other while `save_credentials` and
    /// `load_credentials` — the two the window actually calls — did not: the save
    /// joined `imageguide/sirv` on twice and the load looked at the shorter path, so
    /// the panel said "Saved." and the keys were gone by the next launch.
    ///
    /// This is the only test that touches `IMAGEGUIDE_CONFIG_DIR`. Keep it that way:
    /// the variable is process-wide and the test harness runs threads.
    #[test]
    fn what_the_window_saves_is_what_the_window_loads() {
        let dir = std::env::temp_dir().join(format!("imageguide-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // SAFETY: no other test reads or writes this variable, and nothing else in the
        // process is reading credentials while it runs.
        unsafe { std::env::set_var("IMAGEGUIDE_CONFIG_DIR", &dir) };

        let credentials = Credentials {
            client_id: "id".into(),
            client_secret: "secret".into(),
            studio_key: None,
        };
        save_credentials(&credentials).expect("a fresh temp directory is writable");

        assert_eq!(
            load_credentials(),
            Some(credentials),
            "saved credentials must come back; they were landing one folder deeper"
        );
        assert!(
            credentials_path().is_some_and(|path| path.is_file()),
            "the path the window reports is the file that exists"
        );

        unsafe { std::env::remove_var("IMAGEGUIDE_CONFIG_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_push_names_each_folder_once_and_parents_before_children() {
        let folders = push_folders([
            "top.jpg",
            "a/one.jpg",
            "a/two.jpg",
            "a/deep/three.jpg",
            "b/four.jpg",
        ]);

        assert_eq!(folders, ["a", "b", "a/deep"], "no folder is created twice");
        // Order matters upstream: `a` has to exist before `a/deep`.
        let depth: Vec<usize> = folders.iter().map(|f| f.matches('/').count()).collect();
        assert!(depth.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(
            push_folders(["flat.jpg"]).is_empty(),
            "a flat folder needs none"
        );
    }

    /// A pull turns a name the server chose into a local path. Absolute keys and `..`
    /// must never reach `root.join`.
    #[test]
    fn a_remote_key_that_escapes_the_folder_is_not_pulled() {
        assert!(safe_key("a.jpg"));
        assert!(safe_key("sub/deep/a.jpg"));
        assert!(!safe_key(""));
        assert!(!safe_key("/etc/cron.d/x"), "Path::join would take the lot");
        assert!(!safe_key("../../.bashrc"));
        assert!(!safe_key("sub/../../escape.jpg"));
        assert!(!safe_key("./a.jpg"), "a bare dot is not a name either");

        let remote = vec![
            Node {
                filename: "/d/ok.jpg".into(),
                r#type: "file".into(),
                size: 1,
            },
            Node {
                filename: "/d/../../.bashrc".into(),
                r#type: "file".into(),
                size: 1,
            },
        ];
        assert_eq!(
            pull_plan(&remote, "/d", &HashSet::new()),
            vec!["ok.jpg".to_string()],
            "the escaping key is left out of the plan entirely"
        );
    }

    #[test]
    fn pull_plan_lists_only_keys_the_local_side_lacks() {
        let remote = vec![
            Node {
                filename: "/d/a.jpg".into(),
                r#type: "file".into(),
                size: 1,
            },
            Node {
                filename: "/d/b.jpg".into(),
                r#type: "file".into(),
                size: 2,
            },
            Node {
                filename: "/d/sub/c.jpg".into(),
                r#type: "file".into(),
                size: 3,
            },
        ];
        let local: HashSet<String> = ["a.jpg".into(), "b.jpg".into()].into();
        assert_eq!(
            pull_plan(&remote, "/d", &local),
            vec!["sub/c.jpg".to_string()]
        );
    }

    #[test]
    fn ancestor_dirs_walk_from_the_top() {
        assert_eq!(ancestor_dirs("a.jpg"), Vec::<String>::new());
        assert_eq!(ancestor_dirs("sub/a.jpg"), vec!["sub".to_string()]);
        assert_eq!(
            ancestor_dirs("sub/deep/a.jpg"),
            vec!["sub".to_string(), "sub/deep".to_string()]
        );
    }

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type("a.JPG"), "image/jpeg");
        assert_eq!(content_type("b.png"), "image/png");
        assert_eq!(content_type("c.webp"), "image/webp");
        assert_eq!(content_type("d.avif"), "image/avif");
        assert_eq!(content_type("e.tif"), "application/octet-stream");
    }
}
