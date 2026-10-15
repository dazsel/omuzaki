//! Encoded packets and the byte meter.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;

/// A stream within one muxer, assigned when the stream is added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct StreamId(pub u8);

/// What a [`SideData`] chunk is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SideKind {
    /// Matroska `BlockAdditional` with this id: where `FFmpeg`'s VP9 encoder puts the encoded alpha plane.
    BlockAdditional {
        /// The `BlockAdditional` id.
        id: u64,
    },
}

/// A chunk that travels with a packet (alpha for VP9) and must reach the muxer unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SideData {
    /// What the chunk is.
    pub kind: SideKind,
    /// The bytes; cloning shares them.
    pub data: Bytes,
}

/// One encoded unit, timed in its stream's [`Timebase`](crate::time::Timebase).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Packet {
    /// The stream it belongs to.
    pub stream: StreamId,
    /// Presentation time.
    pub pts: i64,
    /// Decode time.
    pub dts: i64,
    /// Length of the unit.
    pub duration: i64,
    /// Whether decoding can start here.
    pub keyframe: bool,
    /// The encoded bytes; cloning shares them.
    pub data: Bytes,
    /// Side data; empty (and unallocated) for most encoders.
    pub side_data: Vec<SideData>,
}

/// A shared count of bytes kept alive, so payload types in any crate meter the same cap.
#[derive(Clone, Debug, Default)]
pub struct ByteMeter(Arc<AtomicU64>);

impl ByteMeter {
    /// A meter at zero.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `n` bytes.
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    /// Removes `n` bytes, stopping at zero.
    pub fn sub(&self, n: u64) {
        // `try_update` only fails if the closure returns `None`, which this one never does.
        let _ = self.0.try_update(Ordering::Relaxed, Ordering::Relaxed, |v| Some(v.saturating_sub(n)));
    }

    /// The current count.
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}
