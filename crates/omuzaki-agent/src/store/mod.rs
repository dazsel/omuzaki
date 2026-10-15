//! The conversation store (D7): the person's chats with the assistant, kept on their machine and never in the project.
//!
//! # Layout
//!
//! ```text
//! <root>/assistant/<project id>/
//!     blobs/<blake3 hex>.<ext>        images of tool results, shared by the project's conversations
//!     <conversation id>/log.jsonl     the header line, then one `{"at": ..., "kind": ...}` line per fact
//! ```
//!
//! Conversation ids are decimal `u64`s; a new one is the largest existing id plus one. D7 draws the blobs inside each
//! conversation's folder; they sit one level up instead, so that a render two conversations share is stored once and
//! survives until the second of them is deleted.
//!
//! # Time
//!
//! The store reads its [`Clock`] as **wall time: seconds since the Unix epoch**. The app passes a wall-clock adapter
//! (the `SystemClock` of `omuzaki-core` counts from its own start and would put every conversation in 1970); tests pass
//! a `FakeClock`. Timestamps are written as RFC 3339 UTC, to the second.
//!
//! # Safety of paths
//!
//! The project id becomes a folder name, so anything but ASCII letters, digits, `-` and `_` is refused (every call
//! then returns [`HostError::Store`]). Conversation ids are numbers and blob names are checked to be hashes.

mod blobs;
mod conversations;
mod export;
mod log;
mod rationale;
mod resume;
mod rfc3339;

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_agent_api::conversations::{ConversationId, ConversationInfo};
use omuzaki_agent_api::entry::{Entry, SessionNote, StoredBlock};
use omuzaki_agent_api::error::HostError;
use omuzaki_core::time::Clock;
use omuzaki_llm_api::message::ImageMime;
use omuzaki_ops_api::EndReason;

pub use rationale::cut_rationale;

use crate::sink::EntrySink;

/// Longest `first_line` of a [`ConversationInfo`], in characters.
const FIRST_LINE_CHARS: usize = 80;

/// A store failure that names what was being done and why, but never a path.
fn io_err(doing: &str, why: impl std::fmt::Display) -> HostError {
    HostError::Store(format!("could not {doing}: {why}"))
}

/// The conversations of one project under a data folder. Cheap to clone; clones share one lock, so their appends
/// never interleave.
#[derive(Clone)]
pub struct ConversationStore {
    shared: Arc<Shared>,
}

struct Shared {
    root: PathBuf,
    project: String,
    clock: Arc<dyn Clock>,
    /// Serialises every change to the folder: create, append, blob writes, delete, purge.
    lock: Mutex<()>,
    /// Blobs written whose entry has not been appended yet. A delete in the gap must not take a blob another
    /// conversation is about to name again.
    pinned: Mutex<HashSet<String>>,
}

impl std::fmt::Debug for ConversationStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConversationStore").field("project", &self.shared.project).finish_non_exhaustive()
    }
}

impl ConversationStore {
    /// The store of `project` under `root` (the app's data folder). Nothing is read or created until it is used.
    /// `clock` must return wall time in seconds since the Unix epoch (see the module docs). A `project` that is not
    /// a plain id makes every later call fail with [`HostError::Store`].
    pub fn open(root: &Path, project: &str, clock: Arc<dyn Clock>) -> Self {
        let shared = Shared {
            root: root.to_path_buf(),
            project: project.to_owned(),
            clock,
            lock: Mutex::new(()),
            pinned: Mutex::new(HashSet::new()),
        };
        Self { shared: Arc::new(shared) }
    }

    /// Starts a conversation for `model`: a new folder whose `log.jsonl` holds the header line.
    ///
    /// # Errors
    /// [`HostError::Store`] if the folder or file cannot be written.
    pub fn create(&self, model: &str) -> Result<ConversationId, HostError> {
        let _guard = self.guard();
        let project_dir = self.project_dir()?;
        let id = self.ids()?.last().map_or(1, |last| last + 1);
        let dir = project_dir.join(id.to_string());
        fs::create_dir_all(&dir).map_err(|e| io_err("create the conversation folder", e))?;
        let header = log::Header::new(&self.shared.project, &self.now_string(), model);
        log::create(&dir.join(LOG), &log::header_line(&header)?)?;
        Ok(ConversationId(id))
    }

    /// This project's conversations, newest first by their last entry (ties: the higher id first).
    ///
    /// # Errors
    /// [`HostError::Store`] if a log cannot be read.
    pub fn list(&self) -> Result<Vec<ConversationInfo>, HostError> {
        self.rows(false)
    }

    /// Every entry of one conversation, in order. A torn last line is dropped.
    ///
    /// # Errors
    /// [`HostError::Store`] for an unknown id, or a log with a corrupt line before its last.
    pub fn read(&self, id: ConversationId) -> Result<Vec<Entry>, HostError> {
        Ok(self.parse(id)?.into_entries())
    }

    /// Deletes the conversations whose last entry (or creation, if they have none) is more than `retention_days`
    /// days old, and the blobs only they named. Run at startup. Returns the ids deleted, ascending.
    ///
    /// # Errors
    /// [`HostError::Store`] if a log cannot be read or a delete fails.
    pub fn purge(&self, retention_days: u16) -> Result<Vec<ConversationId>, HostError> {
        let _guard = self.guard();
        let limit = i64::from(retention_days) * rfc3339::DAY;
        let now = self.now_secs();
        let mut expired = Vec::new();
        for id in self.ids()? {
            let parsed = self.parse(ConversationId(id))?;
            let last = rfc3339::parse(parsed.last_time())
                .ok_or_else(|| HostError::Store(format!("conversation {id} has an unreadable time")))?;
            if now - last > limit {
                expired.push(id);
            }
        }
        self.remove(&expired)?;
        Ok(expired.into_iter().map(ConversationId).collect())
    }

    /// Deletes one conversation and the blobs no other conversation names.
    ///
    /// # Errors
    /// [`HostError::Store`] for an unknown id or a failed delete.
    pub fn delete(&self, id: ConversationId) -> Result<(), HostError> {
        let _guard = self.guard();
        if !self.ids()?.contains(&id.0) {
            return Err(unknown(id));
        }
        self.remove(&[id.0])
    }

    /// Deletes every conversation of this project (and the blobs they named). Other projects are not touched.
    ///
    /// # Errors
    /// [`HostError::Store`] if a delete fails.
    pub fn delete_all(&self) -> Result<(), HostError> {
        let _guard = self.guard();
        let all = self.ids()?;
        self.remove(&all)
    }

    /// The sink that appends to conversation `id`, flushed per entry. The host receives one of these.
    pub fn sink(&self, id: ConversationId) -> StoreSink {
        StoreSink { store: self.clone(), id }
    }

    /// Stores an image of a tool result as a blob of this project and returns the block for the `ToolResult` entry
    /// that names it. `id` must be an existing conversation.
    ///
    /// # Errors
    /// [`HostError::Store`] for an unknown id or a failed write.
    pub fn put_blob(&self, id: ConversationId, bytes: &[u8], mime: ImageMime) -> Result<StoredBlock, HostError> {
        let _guard = self.guard();
        if !self.ids()?.contains(&id.0) {
            return Err(unknown(id));
        }
        let block = blobs::put(&self.project_dir()?, bytes, mime)?;
        if let StoredBlock::Image { blob, .. } = &block {
            self.pins().insert(blob.clone());
        }
        Ok(block)
    }

    // ----- internals shared with the sibling modules -----

    /// The list rows, newest first. With `skip_unreadable`, a conversation whose log cannot be read is left out
    /// instead of failing the whole list.
    fn rows(&self, skip_unreadable: bool) -> Result<Vec<ConversationInfo>, HostError> {
        let mut rows = Vec::new();
        for id in self.ids()? {
            match self.parse(ConversationId(id)) {
                Ok(parsed) => rows.push(info(ConversationId(id), &parsed)),
                Err(_) if skip_unreadable => {}
                Err(e) => return Err(e),
            }
        }
        // The fixed-width UTC format sorts as text in time order.
        rows.sort_by(|a, b| (&b.last_entry, b.id).cmp(&(&a.last_entry, a.id)));
        Ok(rows)
    }

    /// Takes the store's lock. A panic in another holder cannot leave the files half-changed in a way the lock
    /// protects (every change is one write or one rename), so a poisoned lock is simply taken.
    fn guard(&self) -> MutexGuard<'_, ()> {
        self.shared.lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whole seconds since the Unix epoch, from the clock.
    #[allow(clippy::cast_possible_truncation)] // a wall clock is far inside i64 seconds; NaN and negatives clamp to 0
    fn now_secs(&self) -> i64 {
        let now = self.shared.clock.now().0.floor();
        if now.is_finite() { now as i64 } else { 0 }
    }

    fn now_string(&self) -> String {
        rfc3339::format(self.now_secs())
    }

    /// `<root>/assistant/<project>`, after checking the project id is safe as a folder name.
    fn project_dir(&self) -> Result<PathBuf, HostError> {
        let project = &self.shared.project;
        let plain = !project.is_empty() && project.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        if !plain {
            return Err(HostError::Store("the project id is not a plain id".into()));
        }
        Ok(self.shared.root.join("assistant").join(project))
    }

    /// The conversation folder for `id`, which must exist.
    fn conversation_dir(&self, id: ConversationId) -> Result<PathBuf, HostError> {
        let dir = self.project_dir()?.join(id.0.to_string());
        if dir.join(LOG).is_file() { Ok(dir) } else { Err(unknown(id)) }
    }

    /// The existing conversation ids, ascending.
    fn ids(&self) -> Result<Vec<u64>, HostError> {
        let entries = match fs::read_dir(self.project_dir()?) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(io_err("list conversations", e)),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_err("list conversations", e))?;
            let id = entry.file_name().to_str().and_then(|name| name.parse::<u64>().ok());
            if let Some(id) = id.filter(|_| entry.path().is_dir()) {
                ids.push(id);
            }
        }
        ids.sort_unstable();
        Ok(ids)
    }

    /// Reads and parses one log.
    fn parse(&self, id: ConversationId) -> Result<log::Parsed, HostError> {
        let bytes = fs::read(self.conversation_dir(id)?.join(LOG)).map_err(|e| io_err("read the log", e))?;
        log::parse(&bytes).map_err(|e| match e {
            HostError::Store(why) => HostError::Store(format!("conversation {}: {why}", id.0)),
            other => other,
        })
    }

    /// Appends one entry stamped with the current time. Called by [`StoreSink`].
    fn append(&self, id: ConversationId, entry: &Entry) -> Result<(), HostError> {
        let line = log::entry_line(&self.now_string(), entry)?;
        let _guard = self.guard();
        log::append(&self.conversation_dir(id)?.join(LOG), &line)?;
        // The entry now names its blobs, so a delete can see them in a log and the pins are done.
        let mut pins = self.pins();
        for hash in blob_hashes(entry) {
            pins.remove(hash);
        }
        Ok(())
    }

    fn pins(&self) -> MutexGuard<'_, HashSet<String>> {
        self.shared.pinned.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Deletes the conversations `ids` and then the blobs that only they named. The caller holds the lock.
    fn remove(&self, ids: &[u64]) -> Result<(), HostError> {
        let project_dir = self.project_dir()?;
        let mut removed = Vec::new();
        for id in ids {
            let dir = project_dir.join(id.to_string());
            removed.push(read_log_text(&dir)?);
            fs::remove_dir_all(&dir).map_err(|e| io_err("delete a conversation", e))?;
        }
        // A log that cannot be read is not an empty one: treating it so would delete an image it still shows.
        let mut remaining = Vec::new();
        for id in self.ids()? {
            remaining.push(read_log_text(&project_dir.join(id.to_string()))?);
        }
        blobs::release(&project_dir, &removed, &remaining, &self.pins())
    }
}

/// The log file inside a conversation folder.
const LOG: &str = "log.jsonl";

/// A conversation's log as text for searching blob names; bytes that are not UTF-8 cannot hide a hash.
fn read_log_text(dir: &Path) -> Result<String, HostError> {
    let bytes = fs::read(dir.join(LOG)).map_err(|e| io_err("read a log to find the blobs it names", e))?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The hashes of the images `entry` names.
fn blob_hashes(entry: &Entry) -> Vec<&str> {
    fn image(block: &StoredBlock) -> Option<&str> {
        match block {
            StoredBlock::Image { blob, .. } => Some(blob.as_str()),
            _ => None,
        }
    }
    match entry {
        Entry::ToolResult { blocks, .. } => blocks.iter().filter_map(image).collect(),
        Entry::Compacted { render, .. } => render.iter().filter_map(image).collect(),
        _ => Vec::new(),
    }
}

fn unknown(id: ConversationId) -> HostError {
    HostError::Store(format!("there is no conversation {}", id.0))
}

/// One list row from a parsed log.
fn info(id: ConversationId, parsed: &log::Parsed) -> ConversationInfo {
    let first_line = parsed
        .entries
        .iter()
        .find_map(|s| match &s.entry {
            Entry::User { text, .. } => {
                Some(text.lines().next().unwrap_or("").chars().take(FIRST_LINE_CHARS).collect::<String>())
            }
            _ => None,
        })
        .unwrap_or_default();
    let revoked = parsed.entries.iter().rev().find_map(|s| match &s.entry {
        Entry::Session { note, .. } => Some(matches!(note, SessionNote::Ended(EndReason::Revoked { .. }))),
        _ => None,
    });
    ConversationInfo::new(
        id,
        parsed.header.created.as_str(),
        parsed.last_time(),
        parsed.header.model.as_str(),
        first_line,
        revoked.unwrap_or(false),
    )
}

/// Appends to one conversation of a [`ConversationStore`]: the [`EntrySink`] the host records into.
#[derive(Clone, Debug)]
pub struct StoreSink {
    store: ConversationStore,
    id: ConversationId,
}

impl EntrySink for StoreSink {
    fn append(&self, entry: &Entry) -> Result<(), HostError> {
        self.store.append(self.id, entry)
    }

    fn put_blob(&self, bytes: &[u8], mime: ImageMime) -> Result<StoredBlock, HostError> {
        self.store.put_blob(self.id, bytes, mime)
    }

    fn read_blob(&self, hash: &str, mime: ImageMime) -> Option<Vec<u8>> {
        let path = blobs::path(&self.store.project_dir().ok()?, hash, mime).ok()?;
        fs::read(path).ok()
    }
}
