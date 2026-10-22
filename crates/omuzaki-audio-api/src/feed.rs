//! Feeds (design doc section 4; `ai-performer-voice-face.md` §3.2, §3.3): the producer's [`FeedWriter`] with its
//! [`FeedPlayhead`] and [`FeedCloser`], and the engine's [`FeedSource`], which both engines pull a block from the same
//! way, so a feed sounds and reports identically in the mock and in the real engine.
//!
//! - **The pull.** A feed declares its rate; the engine pulls each 48 kHz block from it by linear interpolation, two
//!   input frames and a position between them, so a feed at `24_000 × 2^(p/12)` Hz changes pitch by `p` semitones
//!   and nothing else needs to. A short ring plays silence (an underrun) and the pull carries on from where it was.
//! - **The playhead** is a bounded log of what each block consumed ([`PlayRecord`]), not a latest snapshot, so a
//!   reader that looks late can still place every sample it wrote. The audio thread is its only writer; a slot is
//!   published through a sequence lock, so a reader never accepts a torn record and the writer never waits.
//! - **The closer** raises a flag any thread may set. The audio thread reads it at the start of its next block and
//!   fades the feed out over [`CLOSE_FADE`] samples (2.5 ms), so a cut never clicks; then the feed is silent for good,
//!   and [`FeedPlayhead::cut_at_sample`] says where the silence began. That is the authority on what played.

use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};

use crate::node::{MAX_BLOCK, SAMPLE_RATE};
use crate::ring::{Consumer, Producer};

/// The close fade, in 48 kHz samples: 2.5 ms, the model node's crossfade.
pub const CLOSE_FADE: usize = 120;

/// How many blocks the playhead keeps: about 2.7 s at the smallest block (64 samples), longer at larger ones. That
/// covers a feed's 1 s of capacity plus a reader that was preempted for a while.
pub const PLAYHEAD_RECORDS: usize = 2048;

/// How many times a reader retries a slot the writer is rewriting before it reports the record lapped.
const READ_RETRIES: usize = 4;

/// What one block consumed from a feed.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct PlayRecord {
    /// 0, 1, 2, ...: one per recorded block, never reused.
    pub index: u64,
    /// The feed sample index (frames, at the feed's rate) of the first sample this block consumed.
    pub consumed: u64,
    /// How many feed samples this block consumed, at least 1. Fewer than the block wanted is an underrun inside it.
    pub n: u32,
    /// The engine sample (48 kHz) at which that first sample plays.
    pub at_sample: u64,
}

impl PlayRecord {
    /// Whether feed sample `sample` was consumed in this block.
    #[must_use]
    pub fn holds(&self, sample: u64) -> bool {
        (self.consumed..self.consumed + u64::from(self.n)).contains(&sample)
    }

    /// The engine sample at which feed sample `sample` plays, for a feed at `rate` Hz; `None` outside this block.
    /// Nothing is ever extrapolated across records, only inside one.
    #[must_use]
    pub fn place(&self, sample: u64, rate: u32) -> Option<u64> {
        if !self.holds(sample) || rate == 0 {
            return None;
        }
        #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        // Offsets inside one block of at most a few thousand samples.
        let offset = ((sample - self.consumed) as f64 * f64::from(SAMPLE_RATE) / f64::from(rate)).round() as u64;
        Some(self.at_sample + offset)
    }
}

/// Why a record can't be read.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Missing {
    /// Not published yet.
    NotYet,
    /// Its slot was already reused by a later block: the reader looked too late.
    Lapped,
}

struct Slot {
    seq: AtomicU64,
    index: AtomicU64,
    consumed: AtomicU64,
    n: AtomicU64,
    at_sample: AtomicU64,
}

struct Log {
    slots: Box<[Slot]>,
    /// How many records were published.
    head: AtomicU64,
    /// The engine sample where a closed feed went silent; `u64::MAX` while it plays.
    cut_at: AtomicU64,
    /// The feed's declared rate.
    rate: u32,
}

impl Log {
    fn new(rate: u32) -> Self {
        let slot = || Slot {
            seq: AtomicU64::new(0),
            index: AtomicU64::new(u64::MAX),
            consumed: AtomicU64::new(0),
            n: AtomicU64::new(0),
            at_sample: AtomicU64::new(0),
        };
        Self {
            slots: (0..PLAYHEAD_RECORDS).map(|_| slot()).collect(),
            head: AtomicU64::new(0),
            cut_at: AtomicU64::new(u64::MAX),
            rate,
        }
    }

    fn slot(&self, index: u64) -> &Slot {
        #[allow(clippy::cast_possible_truncation)] // the remainder is below PLAYHEAD_RECORDS
        &self.slots[(index % PLAYHEAD_RECORDS as u64) as usize]
    }

    /// The writer's side: wait-free, never retries.
    fn publish(&self, record: PlayRecord) {
        let slot = self.slot(record.index);
        let s = slot.seq.load(Ordering::Relaxed);
        slot.seq.store(s + 1, Ordering::Relaxed);
        fence(Ordering::Release);
        slot.index.store(record.index, Ordering::Relaxed);
        slot.consumed.store(record.consumed, Ordering::Relaxed);
        slot.n.store(u64::from(record.n), Ordering::Relaxed);
        slot.at_sample.store(record.at_sample, Ordering::Relaxed);
        slot.seq.store(s + 2, Ordering::Release);
        self.head.store(record.index + 1, Ordering::Release);
    }

    fn read(&self, index: u64) -> Result<PlayRecord, Missing> {
        for _ in 0..READ_RETRIES {
            let head = self.head.load(Ordering::Acquire);
            if index >= head {
                return Err(Missing::NotYet);
            }
            if index + PLAYHEAD_RECORDS as u64 <= head.saturating_sub(1) {
                return Err(Missing::Lapped);
            }
            let slot = self.slot(index);
            let a = slot.seq.load(Ordering::Acquire);
            if a % 2 == 1 {
                continue;
            }
            let record = PlayRecord {
                index: slot.index.load(Ordering::Relaxed),
                consumed: slot.consumed.load(Ordering::Relaxed),
                #[allow(clippy::cast_possible_truncation)] // written from a u32
                n: slot.n.load(Ordering::Relaxed) as u32,
                at_sample: slot.at_sample.load(Ordering::Relaxed),
            };
            fence(Ordering::Acquire);
            let b = slot.seq.load(Ordering::Relaxed);
            if a != b {
                continue;
            }
            if record.index != index {
                return Err(Missing::Lapped);
            }
            return Ok(record);
        }
        Err(Missing::Lapped)
    }
}

/// A read-only view of a feed's playhead. Cheap to clone; never blocks the audio thread.
#[derive(Clone)]
pub struct FeedPlayhead(Arc<Log>);

impl std::fmt::Debug for FeedPlayhead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedPlayhead").field("records", &self.head()).field("cut_at", &self.cut_at_sample()).finish()
    }
}

impl FeedPlayhead {
    /// How many records were published: the next record's index.
    #[must_use]
    pub fn head(&self) -> u64 {
        self.0.head.load(Ordering::Acquire)
    }

    /// The feed's declared rate, in Hz.
    #[must_use]
    pub fn rate(&self) -> u32 {
        self.0.rate
    }

    /// Record `index`.
    ///
    /// # Errors
    /// [`Missing`] when it isn't published yet or its slot was reused.
    pub fn get(&self, index: u64) -> Result<PlayRecord, Missing> {
        self.0.read(index)
    }

    /// The newest record, if any block consumed anything yet.
    #[must_use]
    pub fn latest(&self) -> Option<PlayRecord> {
        let head = self.head();
        (head > 0).then(|| self.get(head - 1).ok()).flatten()
    }

    /// The records from `cursor` on, oldest first. Records already reused are skipped and counted in
    /// [`Records::lost`]; keep [`Records::cursor`] for the next call.
    #[must_use]
    pub fn records_since(&self, cursor: u64) -> Records<'_> {
        let head = self.head();
        let oldest = head.saturating_sub(PLAYHEAD_RECORDS as u64 - 1);
        let start = cursor.max(oldest);
        Records { playhead: self, next: start, end: head, lost: start - cursor.min(start) }
    }

    /// The engine sample where the closed feed went silent, once the close fade ended; `u64::MAX` while it plays.
    #[must_use]
    pub fn cut_at_sample(&self) -> u64 {
        self.0.cut_at.load(Ordering::Acquire)
    }

    /// Where feed sample `sample` plays (an engine sample), looked up in the records from `cursor` on; `None` when it
    /// hasn't been consumed yet, or its record is lost.
    #[must_use]
    pub fn place(&self, sample: u64, cursor: u64) -> Option<u64> {
        let mut records = self.records_since(cursor);
        records.find_map(|r| r.place(sample, self.0.rate))
    }
}

/// [`FeedPlayhead::records_since`].
pub struct Records<'a> {
    playhead: &'a FeedPlayhead,
    next: u64,
    end: u64,
    lost: u64,
}

impl Records<'_> {
    /// Records that were reused before they were read: their samples can't be placed.
    #[must_use]
    pub fn lost(&self) -> u64 {
        self.lost
    }

    /// The cursor to pass next time.
    #[must_use]
    pub fn cursor(&self) -> u64 {
        self.next
    }
}

impl Iterator for Records<'_> {
    type Item = PlayRecord;

    fn next(&mut self) -> Option<PlayRecord> {
        while self.next < self.end {
            let index = self.next;
            self.next += 1;
            match self.playhead.get(index) {
                Ok(record) => return Some(record),
                Err(Missing::Lapped) => self.lost += 1,
                Err(Missing::NotYet) => return None,
            }
        }
        None
    }
}

/// Raises a feed's close flag. Cheap to clone, `Send`; any thread may close, and closing twice is closing once.
#[derive(Clone, Debug)]
pub struct FeedCloser(Arc<AtomicBool>);

impl FeedCloser {
    /// Asks the audio thread to fade the feed out at its next block and silence it for good. Never blocks.
    pub fn close(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether a close was asked for (the fade may not have ended yet: see [`FeedPlayhead::cut_at_sample`]).
    #[must_use]
    pub fn is_raised(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// The producer's end of a feed: the ring it writes (through `Deref` to [`Producer`]: `push`, `free`), its playhead
/// and its closer. Dropping it ends the feed once the ring is empty.
pub struct FeedWriter {
    producer: Producer,
    playhead: FeedPlayhead,
    closer: FeedCloser,
}

impl std::fmt::Debug for FeedWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedWriter").field("playhead", &self.playhead).field("closer", &self.closer).finish_non_exhaustive()
    }
}

impl FeedWriter {
    /// The feed's playhead.
    #[must_use]
    pub fn playhead(&self) -> FeedPlayhead {
        self.playhead.clone()
    }

    /// The feed's closer.
    #[must_use]
    pub fn closer(&self) -> FeedCloser {
        self.closer.clone()
    }

    /// The feed's declared rate, in Hz.
    #[must_use]
    pub fn rate(&self) -> u32 {
        self.playhead.rate()
    }
}

impl Deref for FeedWriter {
    type Target = Producer;

    fn deref(&self) -> &Producer {
        &self.producer
    }
}

impl DerefMut for FeedWriter {
    fn deref_mut(&mut self) -> &mut Producer {
        &mut self.producer
    }
}

/// The engine's end of a feed: the ring it reads, the pull resampler, the close fade and the playhead's writer. Made
/// on the control side ([`feed_channel`]), then moved to the audio thread, which only calls [`FeedSource::pull`].
pub struct FeedSource {
    ring: Consumer,
    stereo: bool,
    log: Arc<Log>,
    closer: Arc<AtomicBool>,
    /// Feed frames consumed so far.
    consumed: u64,
    /// The next record's index.
    records: u64,
    /// Linear pull state: the last two input frames and the position between them.
    last: [f32; 2],
    next: [f32; 2],
    position: f64,
    /// Close fade samples left; `None` before a close.
    fade: Option<usize>,
    cut: bool,
    /// Samples (frames times channels) the pull wanted and the ring didn't have.
    underruns: u64,
    staging: Vec<f32>,
}

impl std::fmt::Debug for FeedSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FeedSource").field("consumed", &self.consumed).field("cut", &self.cut).finish_non_exhaustive()
    }
}

/// A feed's two ends over `ring`'s halves, at `rate` Hz, `stereo` or mono (interleaved when stereo).
#[must_use]
pub fn feed_channel(producer: Producer, consumer: Consumer, rate: u32, stereo: bool) -> (FeedWriter, FeedSource) {
    let log = Arc::new(Log::new(rate));
    let closer = Arc::new(AtomicBool::new(false));
    let writer = FeedWriter {
        producer,
        playhead: FeedPlayhead(Arc::clone(&log)),
        closer: FeedCloser(Arc::clone(&closer)),
    };
    let source = FeedSource {
        ring: consumer,
        stereo,
        log,
        closer,
        consumed: 0,
        records: 0,
        last: [0.0; 2],
        next: [0.0; 2],
        position: 1.0,
        fade: None,
        cut: false,
        underruns: 0,
        staging: vec![0.0; 2 * MAX_BLOCK],
    };
    (writer, source)
}

impl FeedSource {
    /// The feed's declared rate, in Hz.
    #[must_use]
    pub fn rate(&self) -> u32 {
        self.log.rate
    }

    /// Whether the feed is stereo.
    #[must_use]
    pub fn stereo(&self) -> bool {
        self.stereo
    }

    /// Whether the close fade ended: the feed is silent for good.
    #[must_use]
    pub fn is_cut(&self) -> bool {
        self.cut
    }

    /// Samples (frames times channels) the pull wanted while the feed played and the ring didn't have: each played as
    /// silence.
    #[must_use]
    pub fn underruns(&self) -> u64 {
        self.underruns
    }

    /// Whether the engine can drop this feed: cut, or its writer gone and its ring empty.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.cut || (self.ring.writer_gone() && self.ring.available() == 0)
    }

    /// Fills `left` (and `right`, for a stereo feed; a mono feed leaves it alone) with the next `n` samples at 48 kHz,
    /// where the block's first sample is engine sample `block_start`; records what it consumed and applies the close
    /// fade. Returns how many feed frames it consumed: at 48 kHz, the first that many output samples are the feed's
    /// own. Real-time safe: no allocation, no lock, no wait.
    pub fn pull(&mut self, left: &mut [f32], right: &mut [f32], n: usize, block_start: u64) -> usize {
        let n = n.min(left.len()).min(MAX_BLOCK);
        if !self.cut && self.fade.is_none() && self.closer.load(Ordering::Acquire) {
            self.fade = Some(CLOSE_FADE);
        }
        // How many output samples still sound: all of them, what the fade has left, or none once cut.
        let sounding = if self.cut { 0 } else { self.fade.map_or(n, |left| left.min(n)) };
        let first_consumed = self.consumed;
        let at = if self.log.rate == SAMPLE_RATE {
            self.pull_direct(left, right, sounding)
        } else {
            self.pull_resampled(left, right, sounding)
        };
        left[sounding..n].fill(0.0);
        if self.stereo && right.len() >= n {
            right[sounding..n].fill(0.0);
        }
        if let Some(remaining) = self.fade.filter(|_| !self.cut) {
            for i in 0..sounding {
                // Linear to zero: the k-th faded sample has gain 1 - (k + 1) / CLOSE_FADE, so the last one is silent.
                #[allow(clippy::cast_precision_loss)] // below CLOSE_FADE
                let gain = (remaining - i - 1) as f32 / CLOSE_FADE as f32;
                left[i] *= gain;
                if self.stereo && i < right.len() {
                    right[i] *= gain;
                }
            }
            let remaining = remaining - sounding;
            self.fade = Some(remaining);
            if remaining == 0 {
                self.cut = true;
                self.log.cut_at.store(block_start + sounding as u64, Ordering::Release);
            }
        }
        let consumed = self.consumed - first_consumed;
        if consumed > 0
            && let Some(at) = at
        {
            #[allow(clippy::cast_possible_truncation)] // at most a block's frames
            let record = PlayRecord { index: self.records, consumed: first_consumed, n: consumed as u32, at_sample: block_start + at };
            self.log.publish(record);
            self.records += 1;
        }
        #[allow(clippy::cast_possible_truncation)] // at most a block's frames
        let consumed = consumed as usize;
        consumed
    }

    /// The 48 kHz path: frames are played as they are read. Returns the block offset of the first frame read.
    fn pull_direct(&mut self, left: &mut [f32], right: &mut [f32], n: usize) -> Option<u64> {
        let got = if self.stereo {
            let got = self.ring.pop_into(&mut self.staging[..2 * n]) / 2;
            for i in 0..got {
                left[i] = self.staging[2 * i];
                if i < right.len() {
                    right[i] = self.staging[2 * i + 1];
                }
            }
            got
        } else {
            self.ring.pop_into(&mut left[..n])
        };
        left[got..n].fill(0.0);
        if self.stereo && right.len() >= n {
            right[got..n].fill(0.0);
        }
        self.consumed += got as u64;
        self.underruns += ((n - got) * if self.stereo { 2 } else { 1 }) as u64;
        (got > 0).then_some(0)
    }

    /// The resampled path: linear interpolation between the last two frames read. Returns the block offset (rounded)
    /// at which the first frame read this block plays: frame `s` plays where the position between frames `s - 1`
    /// and `s` reaches 1.
    fn pull_resampled(&mut self, left: &mut [f32], right: &mut [f32], n: usize) -> Option<u64> {
        let channels = if self.stereo { 2 } else { 1 };
        let step = f64::from(self.log.rate) / f64::from(SAMPLE_RATE);
        let mut first: Option<f64> = None;
        for i in 0..n {
            let mut popped = 0u32;
            while self.position >= 1.0 {
                self.position -= 1.0;
                let mut frame = [0.0f32; 2];
                if self.ring.pop_into(&mut frame[..channels]) < channels {
                    frame = [0.0; 2];
                    self.underruns += channels as u64;
                } else {
                    self.consumed += 1;
                    popped += 1;
                }
                self.last = self.next;
                self.next = frame;
            }
            if popped > 0 && first.is_none() {
                // The first of `popped` frames read at this sample sits `popped - 1` frames before the newest one,
                // which plays (1 - position) / step samples from here.
                #[allow(clippy::cast_precision_loss)]
                let i = i as f64;
                first = Some(i + (2.0 - f64::from(popped) - self.position) / step);
            }
            #[allow(clippy::cast_possible_truncation)] // a fraction in [0, 1)
            let t = self.position as f32;
            left[i] = self.last[0] + (self.next[0] - self.last[0]) * t;
            if self.stereo && i < right.len() {
                right[i] = self.last[1] + (self.next[1] - self.last[1]) * t;
            }
            self.position += step;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a block offset, at least 0
        first.map(|at| at.round().max(0.0) as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::ring;
    use crate::tap::DropPolicy;

    fn mono(rate: u32) -> (FeedWriter, FeedSource) {
        let (p, c) = ring(48_000, DropPolicy::DropNewest, 0);
        feed_channel(p, c, rate, false)
    }

    /// Pulls `frames` samples in blocks of `block`, the first at engine sample `from`.
    fn run(source: &mut FeedSource, from: u64, frames: usize, block: usize) -> Vec<f32> {
        let mut out = Vec::new();
        let (mut l, mut r) = (vec![0.0; block], vec![0.0; block]);
        while out.len() < frames {
            let n = block.min(frames - out.len());
            source.pull(&mut l, &mut r, n, from + out.len() as u64);
            out.extend_from_slice(&l[..n]);
        }
        out
    }

    #[test]
    fn a_48k_feed_records_every_block_it_consumed() {
        let (mut writer, mut source) = mono(48_000);
        writer.push(&[0.5; 1000]);
        let out = run(&mut source, 0, 2048, 256);
        assert!(out[..1000].iter().all(|&s| (s - 0.5).abs() < 1e-9) && out[1000..].iter().all(|&s| s.abs() < 1e-9));
        let playhead = writer.playhead();
        let records: Vec<_> = playhead.records_since(0).collect();
        assert_eq!(records.len(), 4, "blocks with nothing to consume record nothing");
        assert_eq!(records[3], PlayRecord { index: 3, consumed: 768, n: 232, at_sample: 768 });
        assert_eq!(playhead.place(999, 0), Some(999));
        assert_eq!(playhead.place(1000, 0), None);
    }

    #[test]
    fn a_closed_feed_fades_over_120_samples_then_stays_silent() {
        let (mut writer, mut source) = mono(48_000);
        writer.push(&[1.0; 4000]);
        let mut out = run(&mut source, 0, 256, 256);
        writer.closer().close();
        out.extend(run(&mut source, 256, 512, 64));
        let fade = &out[256..256 + CLOSE_FADE];
        assert!((fade[0] - 119.0 / 120.0).abs() < 1e-6 && fade[CLOSE_FADE - 1] == 0.0);
        assert!(fade.windows(2).all(|w| w[1] < w[0]));
        assert!(out[256 + CLOSE_FADE..].iter().all(|&s| s == 0.0));
        let playhead = writer.playhead();
        assert_eq!(playhead.cut_at_sample(), 256 + CLOSE_FADE as u64);
        let last = playhead.latest().unwrap();
        assert_eq!(last.consumed + u64::from(last.n), 256 + CLOSE_FADE as u64, "the last record ends at the cut");
        assert!(source.is_cut() && source.is_finished());
    }

    #[test]
    fn a_24k_feed_places_an_impulse_where_it_plays() {
        for block in [64, 256, 1024] {
            let (mut writer, mut source) = mono(24_000);
            let mut pcm = vec![0.0f32; 2000];
            pcm[777] = 1.0;
            writer.push(&pcm);
            let out = run(&mut source, 0, 5000, block);
            let peak = out.iter().enumerate().fold((0, 0.0f32), |m, (i, &s)| if s > m.1 { (i, s) } else { m }).0;
            assert_eq!(writer.playhead().place(777, 0), Some(peak as u64), "block {block}");
        }
    }

    #[test]
    fn a_reader_that_falls_a_lap_behind_counts_what_it_lost() {
        let (mut writer, mut source) = mono(48_000);
        let total = PLAYHEAD_RECORDS + 10;
        let mut l = [0.0f32; 1];
        let mut r = [0.0f32; 1];
        for i in 0..total {
            writer.push(&[0.1]);
            source.pull(&mut l, &mut r, 1, i as u64);
        }
        let playhead = writer.playhead();
        assert_eq!(playhead.get(0), Err(Missing::Lapped));
        let mut records = playhead.records_since(0);
        let first = records.next().unwrap();
        assert_eq!(first.index, 11);
        assert_eq!(records.lost(), 11);
        assert_eq!(playhead.get(total as u64), Err(Missing::NotYet));
    }
}
