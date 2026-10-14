//! Decoded records and the cold records' JSON forms (design T1, T2).

use omuzaki_anim_api::{ChannelBuf, ChannelMask, Cues, Layer, TrackFrame};
use omuzaki_core::time::Seconds;
use omuzaki_history_api::{HistoryEntry, Lsn};
use serde::{Deserialize, Serialize};

/// The journal entry an [`EventRecord`] carries.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "form", rename_all = "snake_case")]
pub enum TakeEntry {
    /// The entry in full, with an `Op`'s cause text cleared (T5). Never `Mark` or `Session`.
    Full {
        /// The journal entry.
        entry: HistoryEntry,
    },
    /// A bare stand-in for `Mark` and `Session`, so LSNs stay contiguous.
    Placeholder {
        /// What the entry was (`mark`, `session`).
        kind: String,
    },
}

/// A journal entry during the take, with the frame it took effect at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    /// The entry's position in the project journal.
    pub lsn: Lsn,
    /// The first frame whose tick began after the executor appended the entry.
    pub applied_frame: i64,
    /// The entry.
    pub entry: TakeEntry,
}

/// A hole in the event stream: the feed's queue overflowed. A reader refuses to cross it (`StateGap`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Incomplete {
    /// The first entry that may be missing.
    pub from_lsn: Lsn,
    /// Why (`event_overflow`).
    pub cause: String,
}

/// One record of a chunk, decoded. Hot kinds are decoded to values; cold kinds are handed over as their JSON bytes,
/// borrowed from the chunk, so a reader pays for them only when it wants them.
#[derive(Clone, Debug, PartialEq)]
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
    /// A `StateRecord` as JSON.
    State(&'a [u8]),
    /// A [`Withheld`](crate::Withheld) as JSON.
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
