//! How a running studio is found by `omuzaki mcp` (`agent-sessions.md` 10.2), and what each side remembers.
//!
//! - **The studio's side.** While its MCP port is open, the core writes `.omuzaki/studio.json` in the project: the
//!   port's URL, where a bridge asks for a credential, and the pairing key that lets it ask. The folder is the
//!   project's private one (git-ignored, owner-only where the OS has modes), so only the person's own processes read
//!   the key, and a credential it buys still waits for the person to pair it. The records of paired bridge
//!   credentials (hashes, never secrets) go in `.omuzaki/bridges.json`, so a paired client needs no click after a
//!   restart; a revoked one is dropped from it.
//! - **The bridge's side.** Each client's credential is kept in the user's local data folder, per project and client
//!   (`bridges/<project>.json`, owner-only), so the person pairs a client once per project.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use omuzaki_mcp::TokenRecord;
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};

/// The project's private folder (the one `ProjectLock` keeps).
const PRIVATE_DIR: &str = ".omuzaki";
/// Where a running studio says how to reach it.
const STUDIO_FILE: &str = "studio.json";
/// The records of bridge credentials the person paired.
const BRIDGES_FILE: &str = "bridges.json";
/// The format of [`STUDIO_FILE`].
const FORMAT: u32 = 1;

/// What a running studio publishes for `omuzaki mcp` to find it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rendezvous {
    /// The format version.
    pub format: u32,
    /// The MCP endpoint on the loopback port.
    pub url: String,
    /// Where a bridge asks for its credential.
    pub pair_url: String,
    /// The pairing key a bridge presents there. Secret: never logged.
    pub key: String,
    /// The studio's process, for the person reading the file.
    pub pid: u32,
}

impl Rendezvous {
    pub(crate) fn new(url: String, pair_url: String, key: String) -> Self {
        Self { format: FORMAT, url, pair_url, key, pid: std::process::id() }
    }
}

fn studio_file(project: &Path) -> PathBuf {
    project.join(PRIVATE_DIR).join(STUDIO_FILE)
}

fn bridges_file(project: &Path) -> PathBuf {
    project.join(PRIVATE_DIR).join(BRIDGES_FILE)
}

/// Writes `rendezvous` for the project in `project`.
pub(crate) fn publish(project: &Path, rendezvous: &Rendezvous) -> io::Result<()> {
    write_private(&studio_file(project), &to_json(rendezvous)?)
}

/// Removes the rendezvous: the port is closed.
pub(crate) fn withdraw(project: &Path) {
    if let Err(error) = std::fs::remove_file(studio_file(project))
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "couldn't remove the studio's rendezvous file");
    }
}

/// How to reach the studio that has `project` open, if it serves agents. `None` when no studio published one, or the
/// file is from a format this build doesn't read.
pub fn rendezvous(project: &Path) -> Option<Rendezvous> {
    let text = std::fs::read_to_string(studio_file(project)).ok()?;
    serde_json::from_str::<Rendezvous>(&text).ok().filter(|r| r.format == FORMAT)
}

/// The stored records of paired bridge credentials. Unreadable records are skipped (with a warning): the client is
/// asked to pair again.
pub(crate) fn paired_records(project: &Path) -> Vec<TokenRecord> {
    let Ok(text) = std::fs::read_to_string(bridges_file(project)) else { return Vec::new() };
    serde_json::from_str(&text).unwrap_or_else(|error| {
        tracing::warn!(%error, "the paired bridge credentials can't be read; clients will be asked to pair again");
        Vec::new()
    })
}

/// Remembers `record` (a credential the person just paired), replacing any record with the same hash.
pub(crate) fn remember_paired(project: &Path, record: TokenRecord) {
    update_records(project, |records| {
        records.retain(|r| r.hash != record.hash);
        records.push(record);
    });
}

/// Forgets the credential `tombstone` names (it was revoked), so it isn't admitted at the next start.
pub(crate) fn forget(project: &Path, tombstone: &TokenRecord) {
    update_records(project, |records| records.retain(|r| r.hash != tombstone.hash));
}

fn update_records(project: &Path, change: impl FnOnce(&mut Vec<TokenRecord>)) {
    let mut records = paired_records(project);
    change(&mut records);
    let written = to_json(&records).and_then(|text| write_private(&bridges_file(project), &text));
    if let Err(error) = written {
        tracing::warn!(%error, "couldn't store the paired bridge credentials; the change lasts until the studio closes");
    }
}

/// The credentials a bridge keeps for one project, by client name, in the user's local data folder.
pub(crate) struct Credentials {
    file: PathBuf,
    clients: BTreeMap<String, String>,
}

impl Credentials {
    /// The credentials kept for `project` under `data` (the user's local data folder).
    pub(crate) fn load(data: &Path, project: &Path) -> Self {
        let real = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
        let name = ContentHash::of(real.to_string_lossy().as_bytes()).to_string();
        let name: String = name.chars().filter(char::is_ascii_alphanumeric).take(32).collect();
        let file = data.join("bridges").join(format!("{name}.json"));
        let clients =
            std::fs::read_to_string(&file).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default();
        Self { file, clients }
    }

    pub(crate) fn get(&self, client: &str) -> Option<&str> {
        self.clients.get(client).map(String::as_str)
    }

    /// Keeps `secret` for `client`. A failure to write is logged: the bridge still works, and asks again next time.
    pub(crate) fn set(&mut self, client: &str, secret: String) {
        self.clients.insert(client.to_owned(), secret);
        let written = to_json(&self.clients).and_then(|text| write_private(&self.file, &text));
        if let Err(error) = written {
            tracing::warn!(%error, "couldn't keep the bridge credential; the client will be paired again next time");
        }
    }
}

fn to_json(value: &impl Serialize) -> io::Result<String> {
    serde_json::to_string_pretty(value).map_err(io::Error::other)
}

/// Writes `text` to `path` readable by its owner only, replacing the file in one step so no reader sees half of it.
fn write_private(path: &Path, text: &str) -> io::Result<()> {
    use std::io::Write;
    let dir = path.parent().ok_or_else(|| io::Error::other("no parent folder"))?;
    std::fs::create_dir_all(dir)?;
    let temp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rendezvous_round_trips_and_goes_when_withdrawn() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(rendezvous(tmp.path()), None);
        let published =
            Rendezvous::new("http://127.0.0.1:1/mcp".into(), "http://127.0.0.1:1/mcp/pair".into(), "k".into());
        publish(tmp.path(), &published).unwrap();
        assert_eq!(rendezvous(tmp.path()), Some(published));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(studio_file(tmp.path())).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "only the owner reads the key");
        }
        withdraw(tmp.path());
        assert_eq!(rendezvous(tmp.path()), None);
    }

    #[test]
    fn a_bridge_keeps_one_credential_per_project_and_client() {
        let (data, a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut kept = Credentials::load(data.path(), a.path());
        assert_eq!(kept.get("claude-code"), None);
        kept.set("claude-code", "secret-a".into());
        assert_eq!(Credentials::load(data.path(), a.path()).get("claude-code"), Some("secret-a"));
        assert_eq!(Credentials::load(data.path(), b.path()).get("claude-code"), None, "another project pairs anew");
    }
}
