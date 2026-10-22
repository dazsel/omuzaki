//! The rolling buffer (`docs/design/record.md` D3, D4): the last seconds of encoded chunks, in keyframe groups, under a
//! byte cap that counts every byte the buffer keeps alive.
//!
//! Pure and single-threaded: the replay buffer's encoder thread owns it. The payload is generic, so memory
//! ([`MemoryPayload`]) and disk ([`Slice`](super::segment::Slice)) share one buffer, and so does the input track (a chunk
//! kind, D8).
//!
//! **Groups.** A group starts at a video chunk with `keyframe: true` and runs, in push order, to the next keyframe. The
//! caller pushes the chunks of one frame together, video first. A chunk pushed before the first keyframe is `Skipped`,
//! so a clip always starts decodable.
//!
//! **Admission comes before the bytes exist** (D4). [`RollingBuffer::push_with`] takes the chunk's stored length first,
//! evicts whole groups, oldest first, until the shared [`ByteMeter`] plus that length fits the cap, charges the meter,
//! and only then asks the caller to make the payload from the [`Charge`]. The payload keeps the charge until its last
//! clone drops, so bytes a save still holds after the ring evicted them keep counting: the meter is at or under the cap
//! at every instant, not only after each push. When even evicting every complete group leaves no room (a save pins
//! nearly the whole cap), the chunk is never made, the group in progress is dropped (a clip never spans the hole), the
//! refusal is counted and the buffer skips to the next keyframe; the caller asks its encoder for one.
//!
//! **Span.** After each push, while the buffer would still hold at least `max_span` seconds without its oldest group, that
//! group goes. `span` is the end of the newest video chunk minus the oldest keyframe's `pts`.
//!
//! **Selection.** [`RollingBuffer::select`] starts at the newest keyframe at or before `end - length` (else the oldest),
//! clones payload handles only, never bytes, and reports how much earlier or shorter than asked the clip is.
//!
//! **Spares.** A buffer told to [`keep_spares`](RollingBuffer::keep_spares) hands each payload it evicts to
//! [`Payload::retire`], which gives its bytes back to the meter at once and returns it when nothing else holds it; up
//! to that many retired payloads wait for [`take_spare`](RollingBuffer::take_spare), so a warm buffer's owner can make
//! the next payloads in their allocations instead of new ones (#1019: no allocation once warm).

use std::collections::VecDeque;

use bytes::Bytes;
use omuzaki_core::time::Seconds;
use omuzaki_stream_api::packet::{ByteMeter, Packet};
use std::sync::Arc;

/// The shortest span a buffer may be asked to hold.
pub const MIN_SPAN: Seconds = Seconds(5.0);
/// The longest span a buffer may be asked to hold.
pub const MAX_SPAN: Seconds = Seconds(300.0);
/// The smallest byte cap.
pub const MIN_BYTES: u64 = 1 << 20;
/// Chunks per second the buffer reserves room for up front: video at 120 fps plus as many audio and input chunks.
const RESERVE_PER_SECOND: f64 = 240.0;
/// The most chunks reserved up front, whatever the span.
const RESERVE_MAX: usize = 1 << 17;

/// Something the buffer can hold: it knows the bytes it occupies in its backing, and cloning shares it.
pub trait Payload: Clone + Send + 'static {
    /// The bytes this payload occupies, as charged to the meter when it was made.
    fn len(&self) -> u64;

    /// Whether it occupies no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Called on a payload the buffer evicts while it keeps spares: give the bytes back to the meter now and return the
    /// payload if its allocation can be reused (nothing else holds it). The default reuses nothing.
    fn retire(self) -> Option<Self> {
        None
    }
}

/// What a chunk carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ChunkKind {
    /// A video packet; `keyframe` starts a group.
    Video {
        /// Whether decoding can start here.
        keyframe: bool,
    },
    /// Audio samples of one track.
    Audio {
        /// The track's index.
        track: u8,
    },
    /// Tracker input (D8).
    Input,
}

impl ChunkKind {
    const fn is_key(self) -> bool {
        matches!(self, Self::Video { keyframe: true })
    }

    const fn is_video(self) -> bool {
        matches!(self, Self::Video { .. })
    }
}

/// One chunk: what it is, when it plays and its payload.
#[derive(Clone, Debug)]
pub struct Chunk<D> {
    /// What it carries.
    pub kind: ChunkKind,
    /// When it starts, on the buffer's timeline.
    pub pts: Seconds,
    /// How long it lasts.
    pub duration: Seconds,
    /// The payload.
    pub data: D,
}

impl<D> Chunk<D> {
    fn end(&self) -> f64 {
        self.pts.0 + self.duration.0
    }
}

/// The buffer's limits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caps {
    /// How many seconds to keep: [`MIN_SPAN`] to [`MAX_SPAN`].
    pub max_span: Seconds,
    /// The most bytes kept alive, held by the buffer or by a save: at least [`MIN_BYTES`].
    pub max_bytes: u64,
}

/// Why a buffer can't be made.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum RingError {
    /// The span is outside 5 s to 300 s.
    #[error("the replay length must be 5 to 300 seconds, not {0} s")]
    Span(f64),
    /// The byte cap is under 1 MiB.
    #[error("the replay cap must be at least 1 MiB, not {0} bytes")]
    Bytes(u64),
}

/// Bytes charged to a [`ByteMeter`], given back when this drops. A payload holds the charge for the bytes it occupies.
#[derive(Debug)]
pub struct Charge {
    meter: ByteMeter,
    bytes: u64,
}

impl Charge {
    /// Charges `bytes` to `meter` now.
    pub fn new(meter: &ByteMeter, bytes: u64) -> Self {
        meter.add(bytes);
        Self { meter: meter.clone(), bytes }
    }

    /// The bytes charged.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Takes over `other`'s bytes, so they are given back when this drops.
    pub fn absorb(&mut self, mut other: Self) {
        self.bytes += std::mem::take(&mut other.bytes);
    }

    /// Gives the bytes back now; the charge then holds none.
    pub fn release(&mut self) {
        self.meter.sub(std::mem::take(&mut self.bytes));
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        self.meter.sub(self.bytes);
    }
}

/// A payload in memory: an encoded packet (its data and side data share their bytes with the encoder's output) and the
/// charge for them.
#[derive(Clone, Debug)]
pub struct MemoryPayload(Arc<(Packet, Charge)>);

impl MemoryPayload {
    /// The bytes a packet occupies in memory: its data and every side datum.
    pub fn stored_len(packet: &Packet) -> u64 {
        let side: usize = packet.side_data.iter().map(|s| s.data.len()).sum();
        (packet.data.len() + side) as u64
    }

    /// Keeps `packet` under `charge`, which [`stored_len`](Self::stored_len) sized.
    pub fn new(packet: Packet, charge: Charge) -> Self {
        Self(Arc::new((packet, charge)))
    }

    /// The packet.
    pub fn packet(&self) -> &Packet {
        &self.0.0
    }

    /// Keeps `packet` under `charge` in this payload's allocation when nothing else holds it, returning the old packet;
    /// else gives both back.
    ///
    /// # Errors
    /// The packet and the charge, untouched, when a clone of this payload is still alive.
    pub fn refill(&mut self, packet: Packet, charge: Charge) -> Result<Packet, (Packet, Charge)> {
        match Arc::get_mut(&mut self.0) {
            Some(inner) => Ok(std::mem::replace(inner, (packet, charge)).0),
            None => Err((packet, charge)),
        }
    }
}

impl Payload for MemoryPayload {
    fn len(&self) -> u64 {
        self.0.1.bytes()
    }

    fn retire(mut self) -> Option<Self> {
        let (packet, charge) = Arc::get_mut(&mut self.0)?;
        // The bytes go now, not when the spare is reused: the cap counts every byte kept alive.
        packet.data = Bytes::new();
        packet.side_data.clear();
        charge.release();
        Some(self)
    }
}

/// What a push did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Push {
    /// Kept, after evicting this many whole groups and their bytes from the buffer.
    Kept {
        /// Groups evicted.
        evicted_groups: u32,
        /// Bytes those groups held in the buffer.
        evicted_bytes: u64,
    },
    /// Not kept: before the first keyframe, or while skipping to the next one after a refusal.
    Skipped,
    /// Not kept, and the group in progress was dropped: no room under the cap even with every complete group gone.
    /// Ask the encoder for a keyframe.
    Refused,
}

/// A clip's chunks and how its window compares with what was asked.
#[derive(Clone, Debug)]
pub struct Selection<D> {
    /// The video chunks from the start keyframe on, and the other chunks that end after it, in push order.
    pub chunks: Vec<Chunk<D>>,
    /// The start keyframe's `pts`.
    pub start: Seconds,
    /// The end of the newest video chunk.
    pub end: Seconds,
    /// The length asked for.
    pub requested: Seconds,
    /// `end - start`.
    pub actual: Seconds,
    /// How much earlier than `end - requested` the clip starts (at most one keyframe interval).
    pub early_by: Seconds,
    /// How much shorter than asked the clip is, when the buffer holds less.
    pub short_by: Seconds,
}

impl<D> Selection<D> {
    /// Whether the selection holds nothing.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }
}

/// What the buffer holds and has done.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Stats {
    /// Bytes of the chunks in the buffer.
    pub bytes: u64,
    /// Chunks in the buffer.
    pub chunks: usize,
    /// Seconds from the oldest keyframe to the end of the newest chunk.
    pub span: Seconds,
    /// Groups evicted since the start.
    pub groups_evicted: u64,
    /// Chunks refused for want of room.
    pub refused: u64,
    /// Chunks skipped before a keyframe.
    pub skipped: u64,
    /// The most bytes the buffer has held.
    pub high_water_bytes: u64,
}

/// The rolling buffer. See the module docs for its rules.
#[derive(Debug)]
pub struct RollingBuffer<D: Payload> {
    caps: Caps,
    meter: ByteMeter,
    chunks: VecDeque<Chunk<D>>,
    /// The sequence number of every keyframe chunk in the buffer, with its `pts`, oldest first.
    keys: VecDeque<(u64, f64)>,
    /// The sequence number of the front chunk; the next chunk pushed gets `front_seq + chunks.len()`.
    front_seq: u64,
    bytes: u64,
    /// The end of the newest video chunk pushed: audio and input never lengthen a clip.
    end: f64,
    /// Skipping to the next keyframe: nothing kept yet, or a refusal dropped the group in progress.
    skipping: bool,
    stats: Stats,
    /// Retired payloads waiting to be reused; never grows past the capacity `keep_spares` gave it.
    spares: Vec<D>,
}

impl<D: Payload> RollingBuffer<D> {
    /// An empty buffer with its own meter.
    ///
    /// # Errors
    /// [`RingError`] when a cap is out of range.
    pub fn new(caps: Caps) -> Result<Self, RingError> {
        Self::with_meter(caps, ByteMeter::new())
    }

    /// An empty buffer charging `meter`, which payloads made elsewhere (a disk store) may share.
    ///
    /// # Errors
    /// [`RingError`] when a cap is out of range.
    pub fn with_meter(caps: Caps, meter: ByteMeter) -> Result<Self, RingError> {
        if !(MIN_SPAN.0..=MAX_SPAN.0).contains(&caps.max_span.0) {
            return Err(RingError::Span(caps.max_span.0));
        }
        if caps.max_bytes < MIN_BYTES {
            return Err(RingError::Bytes(caps.max_bytes));
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // At most 300 s × 240, far under usize.
        let reserve = ((caps.max_span.0 * RESERVE_PER_SECOND) as usize).min(RESERVE_MAX);
        Ok(Self {
            caps,
            meter,
            chunks: VecDeque::with_capacity(reserve),
            keys: VecDeque::with_capacity(reserve / 8),
            front_seq: 0,
            bytes: 0,
            end: 0.0,
            skipping: true,
            stats: Stats::default(),
            spares: Vec::new(),
        })
    }

    /// Keeps up to `n` evicted payloads for reuse (see the module docs). 0, the default, drops each one.
    pub fn keep_spares(&mut self, n: usize) {
        self.spares = Vec::with_capacity(n);
    }

    /// A retired payload to reuse, newest first.
    pub fn take_spare(&mut self) -> Option<D> {
        self.spares.pop()
    }

    /// Lets go of an evicted payload: kept as a spare when there is room and it can be reused, else dropped.
    fn discard(&mut self, data: D) {
        if self.spares.len() < self.spares.capacity()
            && let Some(spare) = data.retire()
        {
            self.spares.push(spare);
        }
    }

    /// The caps.
    pub fn caps(&self) -> Caps {
        self.caps
    }

    /// The meter: every byte this buffer's payloads keep alive, in the buffer or in a save.
    pub fn meter(&self) -> &ByteMeter {
        &self.meter
    }

    /// Pushes a chunk of `len` stored bytes: admits it under the cap, then calls `make` with its [`Charge`] to make the
    /// payload. `make` runs only when the chunk is kept; its error is returned with nothing pushed and the charge given
    /// back.
    ///
    /// # Errors
    /// `make`'s error.
    pub fn push_with<E>(
        &mut self,
        kind: ChunkKind,
        pts: Seconds,
        duration: Seconds,
        len: u64,
        make: impl FnOnce(Charge) -> Result<D, E>,
    ) -> Result<Push, E> {
        let key = kind.is_key();
        if self.skipping && !key {
            self.stats.skipped += 1;
            return Ok(Push::Skipped);
        }
        let mut evicted = (0u32, 0u64);
        // A keyframe closes the group in progress, so every group may go; otherwise the newest is still filling.
        let keep = usize::from(!key);
        while self.meter.get().saturating_add(len) > self.caps.max_bytes && self.keys.len() > keep {
            self.evict_oldest(&mut evicted);
        }
        if self.meter.get().saturating_add(len) > self.caps.max_bytes {
            self.drop_newest_group();
            self.stats.refused += 1;
            self.skipping = true;
            return Ok(Push::Refused);
        }
        let data = make(Charge::new(&self.meter, len))?;
        if key {
            self.skipping = false;
            self.keys.push_back((self.next_seq(), pts.0));
        }
        let chunk = Chunk { kind, pts, duration, data };
        if kind.is_video() {
            self.end = self.end.max(chunk.end());
        }
        self.chunks.push_back(chunk);
        self.bytes += len;
        self.stats.high_water_bytes = self.stats.high_water_bytes.max(self.bytes);
        while self.keys.len() >= 2 && self.end - self.keys[1].1 >= self.caps.max_span.0 {
            self.evict_oldest(&mut evicted);
        }
        Ok(Push::Kept { evicted_groups: evicted.0, evicted_bytes: evicted.1 })
    }

    fn next_seq(&self) -> u64 {
        self.front_seq + self.chunks.len() as u64
    }

    /// Evicts the oldest group: every chunk before the second keyframe, in push order.
    fn evict_oldest(&mut self, evicted: &mut (u32, u64)) {
        if self.keys.pop_front().is_none() {
            return;
        }
        let until = self.keys.front().map_or_else(|| self.next_seq(), |&(seq, _)| seq);
        while self.front_seq < until {
            let Some(chunk) = self.chunks.pop_front() else { break };
            self.front_seq += 1;
            self.bytes -= chunk.data.len();
            evicted.1 += chunk.data.len();
            self.discard(chunk.data);
        }
        evicted.0 += 1;
        self.stats.groups_evicted += 1;
    }

    /// Drops the group in progress (every chunk from the newest keyframe on), after a refusal.
    fn drop_newest_group(&mut self) {
        let Some((seq, _)) = self.keys.pop_back() else { return };
        while self.next_seq() > seq {
            let Some(chunk) = self.chunks.pop_back() else { break };
            self.bytes -= chunk.data.len();
            self.discard(chunk.data);
        }
        if self.chunks.is_empty() {
            self.front_seq = seq;
        }
    }

    /// The newest `length` seconds, from the keyframe at or before `end - length`. Clones payload handles only.
    pub fn select(&self, length: Seconds) -> Selection<D> {
        let end = self.end;
        let target = end - length.0;
        let start_key = self.keys.iter().rev().find(|&&(_, pts)| pts <= target).or(self.keys.front()).copied();
        let Some((start_seq, start)) = start_key else {
            return Selection {
                chunks: Vec::new(),
                start: Seconds(end),
                end: Seconds(end),
                requested: length,
                actual: Seconds(0.0),
                early_by: Seconds(0.0),
                short_by: length,
            };
        };
        let mut chunks = Vec::with_capacity(self.chunks.len());
        for (i, chunk) in self.chunks.iter().enumerate() {
            let seq = self.front_seq + i as u64;
            let wanted = if chunk.kind.is_video() { seq >= start_seq } else { chunk.end() > start };
            if wanted {
                chunks.push(chunk.clone());
            }
        }
        let actual = end - start;
        Selection {
            chunks,
            start: Seconds(start),
            end: Seconds(end),
            requested: length,
            actual: Seconds(actual),
            early_by: Seconds((target - start).max(0.0)),
            short_by: Seconds((length.0 - actual).max(0.0)),
        }
    }

    /// What the buffer holds and has done.
    pub fn stats(&self) -> Stats {
        let span = self.keys.front().map_or(0.0, |&(_, pts)| self.end - pts);
        Stats { bytes: self.bytes, chunks: self.chunks.len(), span: Seconds(span), ..self.stats }
    }

    /// Whether the buffer holds no chunk.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Empties the buffer; the next chunk kept is a keyframe.
    pub fn clear(&mut self) {
        self.front_seq = self.next_seq();
        self.chunks.clear();
        self.keys.clear();
        self.bytes = 0;
        self.skipping = true;
    }
}
