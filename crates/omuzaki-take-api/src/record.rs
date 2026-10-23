//! Decoded records and the cold records' JSON forms (design T1, T2).

use std::sync::Arc;

use omuzaki_anim_api::{ChannelBuf, ChannelMask, Cues, Layer, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_history_api::{HistoryEntry, Lsn};
use serde::{Deserialize, Deserializer, Serialize};

/// The journal entry an [`EventRecord`] carries. Build it with [`TakeEntry::from_history`], which keeps the take's
/// rules (T5) whatever entry it is given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TakeEntry {
    /// The entry in full, with an `Op`'s cause text cleared (T5). Never `Mark` or `Session`: [`FullEntry`] cannot
    /// hold them. Its own `entry` tag sits beside `form` in the JSON.
    Full(FullEntry),
    /// A bare stand-in for `Mark` and `Session`, so LSNs stay contiguous.
    Placeholder {
        /// What the entry was (`mark`, `session`).
        kind: String,
    },
    /// A `form` this version does not know (a newer minor). The entry is skipped; its LSN still counts.
    #[serde(other)]
    Unknown,
}

impl TakeEntry {
    /// The form a take carries `entry` in: `Mark` and `Session` as a [`Placeholder`](TakeEntry::Placeholder) (a
    /// `Mark` embeds the whole history), anything else in full with an `Op`'s cause text cleared, since a take never
    /// holds prompts or chat messages (T5).
    pub fn from_history(entry: HistoryEntry) -> Self {
        match FullEntry::new(entry) {
            Ok(full) => Self::Full(full),
            Err(kind) => Self::Placeholder { kind: kind.to_owned() },
        }
    }
}

/// A journal entry a take may carry in full: never `Mark` or `Session`, and an `Op` without its cause text (T5). Built
/// by [`TakeEntry::from_history`] and by reading, which both keep those rules, so no take can be written that breaks
/// them.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct FullEntry(HistoryEntry);

impl FullEntry {
    /// `entry` as a take carries it, or the placeholder kind (`mark`, `session`) of an entry it never carries in full.
    fn new(mut entry: HistoryEntry) -> Result<Self, &'static str> {
        match &mut entry {
            HistoryEntry::Mark { .. } => return Err("mark"),
            HistoryEntry::Session { .. } => return Err("session"),
            HistoryEntry::Op { record, .. } => {
                if record.cause.as_ref().is_some_and(|cause| cause.text.is_some())
                    && let Some(cause) = Arc::make_mut(record).cause.as_mut()
                {
                    cause.text = None;
                }
            }
            _ => {}
        }
        Ok(Self(entry))
    }

    /// The entry.
    pub fn entry(&self) -> &HistoryEntry {
        &self.0
    }

    /// The entry, by value.
    pub fn into_entry(self) -> HistoryEntry {
        self.0
    }
}

impl<'de> Deserialize<'de> for FullEntry {
    /// Reads the entry and applies the same rules: a cause text an older writer kept is dropped, and a `Mark` or
    /// `Session` in full is refused.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(HistoryEntry::deserialize(deserializer)?).map_err(|kind| {
            serde::de::Error::custom(format!("a take carries `{kind}` only as a placeholder, never in full (T5)"))
        })
    }
}

/// A journal entry during the take, with the frame it took effect at.
///
/// Later fields are added with `#[serde(default)]`, so an older reader keeps reading the record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct EventRecord {
    /// The entry's position in the project journal.
    pub lsn: Lsn,
    /// The first frame whose tick began after the executor appended the entry.
    pub applied_frame: i64,
    /// The entry.
    pub entry: TakeEntry,
}

impl EventRecord {
    /// An event record with its required fields. Later fields are set with `with_*`.
    pub fn new(lsn: Lsn, applied_frame: i64, entry: TakeEntry) -> Self {
        Self { lsn, applied_frame, entry }
    }
}

/// A hole in the event stream: the feed's queue overflowed. A reader refuses to cross it (`StateGap`).
///
/// Later fields are added with `#[serde(default)]`, so an older reader keeps reading the record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct Incomplete {
    /// The first entry that may be missing.
    pub from_lsn: Lsn,
    /// Why (`event_overflow`).
    pub cause: String,
}

impl Incomplete {
    /// A hole starting at `from_lsn`, for `cause`.
    pub fn new(from_lsn: Lsn, cause: impl Into<String>) -> Self {
        Self { from_lsn, cause: cause.into() }
    }
}

/// One record of a chunk, decoded. Hot kinds are decoded to values; cold kinds are handed over as their JSON bytes,
/// borrowed from the chunk, so a reader pays for them only when it wants them.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum Record<'a> {
    /// A tracker frame and its source index.
    Track(TrackFrame, u8),
    /// An audio-derived layer for a voice.
    Voice {
        /// The voice index.
        voice: u8,
        /// The performance time it was pushed with.
        perf_time: Seconds,
        /// The layer.
        layer: Layer,
    },
    /// An audio cue sample for a voice.
    Cue {
        /// The voice index.
        voice: u8,
        /// The performance time.
        perf_time: Seconds,
        /// Loudness, onset and laugh.
        cues: Cues,
    },
    /// A cast member's final channels after the life layer.
    Bus {
        /// The member index in the State's cast.
        member: u8,
        /// The channel values.
        buf: ChannelBuf,
        /// Which channels are set.
        mask: ChannelMask,
    },
    /// An [`EventRecord`] as JSON.
    Event(&'a [u8]),
    /// An entry resolved from the project journal by the reader.
    EventRef {
        /// The entry.
        lsn: Lsn,
        /// The frame it took effect at.
        applied_frame: i64,
    },
    /// Events are complete through this frame.
    Watermark(i64),
    /// A [`StateRecord`](crate::state::StateRecord) as JSON.
    State(&'a [u8]),
    /// A [`Withheld`](crate::state::Withheld) as JSON.
    Withheld(&'a [u8]),
    /// An [`Incomplete`] as JSON.
    Incomplete(&'a [u8]),
    /// The playout delay `D`, in seconds.
    Delay(f32),
    /// A kind this version does not know, skipped by its length and counted.
    Unknown {
        /// The kind byte.
        kind: u8,
        /// The body.
        body: &'a [u8],
    },
}
