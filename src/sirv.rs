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
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const API: &str = "https://api.sirv.com";
/// Tokens live 20 minutes on the server. Refresh a minute early so an upload
/// started at minute 19 does not die mid-flight.
const TOKEN_MARGIN: Duration = Duration::from_secs(60);
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq)]
pub struct Credentials {
    pub client_id: String,
    pub client_secret: String,
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

    /// One directory listing. Folder names come back absolute
    /// (`/photos/sub`); files carry their byte size.
    pub fn readdir(&mut self, dirname: &str) -> Result<Vec<Node>, Error> {
        let url = format!("{API}/v2/files/readdir?dirname={}", encode_path(dirname));
        let read = |client: &mut Self| -> Result<Vec<Node>, Error> {
            let token = client.token()?;
            let response = client
                .agent
                .get(&url)
                .set("Authorization", &format!("Bearer {token}"))
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
        };

        match read(self) {
            // A token that expired between check and use is routine; refresh
            // once and try again rather than surfacing a login error.
            Err(Error { status: 401, .. }) => {
                self.token = None;
                read(self)
            }
            other => other,
        }
    }
}

fn sirv_error(stage: &'static str) -> impl Fn(ureq::Error) -> Error {
    move |error| match error {
        ureq::Error::Status(status, response) => {
            let message = response.into_string().unwrap_or_default();
            let message = message.lines().next().unwrap_or_default();
            Error {
                status,
                message: format!("{stage}: {message}"),
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
fn store_path() -> Option<PathBuf> {
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
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "client_id" => client_id = Some(value.trim().to_string()),
            "client_secret" => client_secret = Some(value.trim().to_string()),
            _ => {}
        }
    }
    Some(Credentials {
        client_id: client_id?,
        client_secret: client_secret?,
    })
}

// The writing half gains its caller with the in-app connect form (the push
// phase); the tests exercise it now so the file format cannot drift.
#[allow(dead_code)]
pub fn save_credentials_at(base: impl AsRef<Path>, credentials: &Credentials) {
    let path = store_path_in(base);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(
        path,
        format!(
            "client_id={}\nclient_secret={}\n",
            credentials.client_id, credentials.client_secret
        ),
    );
}

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
        };
        save_credentials_at(&base, &credentials);
        assert_eq!(load_credentials_from(Some(&path)), Some(credentials));
        let _ = std::fs::remove_dir_all(&base);
    }
}
