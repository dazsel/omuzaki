//! The file header's JSON (design T4).

use std::collections::BTreeMap;

use omuzaki_doc_api::DocVersion;
use omuzaki_history_api::{Lsn, SnapshotId};
use serde::{Deserialize, Serialize};

/// What the take belongs to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TakeKind {
    /// A recording of a whole session.
    Recording,
    /// A saved replay-buffer clip.
    Replay,
}

/// Which side of the take the pinned snapshot sits on (T6).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotAt {
    /// Taken at the start: entries replay forward.
    Start,
    /// Taken at the end: entries are inverted backward.
    End,
}

/// The header JSON of a `<stem>.input` file. Chunks keep their session frame numbers, so the file is the ring's bytes
/// unchanged and a file frame is `frame - first_frame`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TakeHeader {
    /// The take's stem.
    pub take: String,
    /// Recording or replay clip.
    pub kind: TakeKind,
    /// The frame rate as numerator and denominator.
    pub fps: (u32, u32),
    /// The session frame index of the first chunk.
    pub first_frame: i64,
    /// When the take was made, UTC, RFC 3339.
    pub created: String,
    /// The Omuzaki version that wrote it.
    pub app: String,
    /// The project snapshot pinned for re-rendering, if one was taken.
    pub snapshot: Option<SnapshotId>,
    /// The exact journal position the snapshot holds.
    pub snapshot_lsn: Lsn,
    /// Which side of the take the snapshot sits on.
    pub snapshot_at: SnapshotAt,
    /// The document version at the take's start.
    pub doc_version: DocVersion,
    /// The video file's name, if there is one.
    pub video: Option<String>,
    /// The WAV sidecar names by track.
    pub audio: BTreeMap<String, String>,
}
