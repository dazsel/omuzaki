//! The lock-free single-producer single-consumer ring of `f32` samples that carries taps out of the graph and feeds into
//! it (design doc section 4.1). It is the ring of the speech host, generalised and moved here so consumers in any
//! crate can read it.
//!
//! Pushing never allocates, locks or blocks, so it is safe in a real-time callback. Samples are stored as the bits of an
//! `f32` in `AtomicU32` slots, which keeps the whole thing in safe Rust: the producer writes slots with relaxed stores
//! and publishes them with a release store of the write counter, and the consumer acquires the counter before reading.
//!
//! # Drop policy
//!
//! - [`DropPolicy::DropNewest`] for contiguous streams (recognition, recording, the clap test): when the consumer falls
//!   behind and the ring is full, the *newest* samples that do not fit are refused and counted. What is already queued
//!   stays contiguous.
//! - [`DropPolicy::Overwrite`] for latest-window readers (lip sync, cues): the producer never waits and never refuses.
//!   When the consumer falls behind, the oldest samples are overwritten and the consumer skips ahead.
//!
//! # Gaps, stream positions and flags
//!
//! Every sample has a **stream position**: its index on the timeline the ring was created with (`origin`), counting
//! samples that were refused, overwritten, or never captured ([`Producer::push_gap`]) as well as delivered ones.
//! [`Consumer::read`] returns a run of samples that is contiguous on that timeline, with the stream position of its
//! first sample and the number of positions lost just before it ([`TapRead::gap_before`]), so a consumer never splices
//! across a loss without knowing. A run also never mixes [`TapFlags`] (a mute that started mid-block ends the run).
//!
//! Each slot has a second `AtomicU32` beside the sample: the producer's running total of lost positions (30 bits,
//! wrapping) and the two flags. The consumer subtracts totals, so a gap is exact whatever was skipped in between. Totals
//! wrap, so a run is exact as long as fewer than 2^30 positions (6 hours at 48 kHz) were lost between two reads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};

use crate::tap::{DropPolicy, TapSink};

const GAP_MASK: u32 = (1 << 30) - 1;
const MUTED_BIT: u32 = 1 << 30;
const PRIVATE_BIT: u32 = 1 << 31;

/// What the producer says about the samples it writes next.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TapFlags {
    /// The signal is silenced because the person muted: nobody hears it, the AI included.
    pub muted: bool,
    /// The turn is in `MicMode::CohostOnly`: the person believes it is private.
    pub private: bool,
}

impl TapFlags {
    const fn bits(self) -> u32 {
        (if self.muted { MUTED_BIT } else { 0 }) | (if self.private { PRIVATE_BIT } else { 0 })
    }

    const fn from_meta(meta: u32) -> Self {
        Self { muted: meta & MUTED_BIT != 0, private: meta & PRIVATE_BIT != 0 }
    }
}

/// What [`Consumer::read`] returned besides the samples.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct TapRead {
    /// How many samples were written to the front of the buffer.
    pub len: usize,
    /// The stream position of the first sample (an engine-clock index at the tap's rate, when the ring was created
    /// with the tap's first sample as its origin).
    pub start_sample: u64,
    /// How many stream positions were lost between the previous run and this one: refused, overwritten, or never
    /// captured. Zero when the stream is contiguous.
    pub gap_before: u64,
    /// The run was silenced by a mute.
    pub muted: bool,
    /// The run was captured in `MicMode::CohostOnly`.
    pub private: bool,
}

struct Shared {
    samples: Box<[AtomicU32]>,
    /// Per slot: the producer's lost-position total (low 30 bits) and the flags (top two bits).
    meta: Box<[AtomicU32]>,
    /// Total samples ever written. Only the producer stores to it.
    written: AtomicU64,
    /// Overwrite only: the end of the batch the producer is writing, stored before its slots are touched, so a reader
    /// can tell which slots may have been overwritten while it copied them.
    claimed: AtomicU64,
    /// `DropNewest` only: total samples ever read. Only the consumer stores to it.
    read: AtomicU64,
    /// Total samples refused because the ring was full.
    dropped: AtomicU64,
    policy: DropPolicy,
    /// The stream position of the first sample.
    origin: u64,
    /// Set when the consumer is dropped: the tap or feed is closed. A flag the other end polls, never a wait.
    reader_gone: AtomicBool,
    /// Set when the producer is dropped.
    writer_gone: AtomicBool,
}

/// The write end. Move it into the audio callback.
pub struct Producer {
    shared: Arc<Shared>,
    /// Lost positions so far (30-bit wrapping): stamped on every sample written.
    lost: u32,
    flags: TapFlags,
}

/// The read end. Owned by the consumer's thread.
pub struct Consumer {
    shared: Arc<Shared>,
    /// Accepted-sample index just after the last run returned.
    pos: u64,
    /// Stream position just after the last run returned.
    stream: u64,
    /// Lost-position total of the last sample returned.
    lost_total: u32,
    /// Samples skipped because the producer overwrote them (Overwrite only).
    overwritten: u64,
}

/// Creates a ring that holds `capacity` samples (at least one). `origin` is the stream position of the first sample
/// the producer will write.
#[must_use]
pub fn ring(capacity: usize, policy: DropPolicy, origin: u64) -> (Producer, Consumer) {
    let capacity = capacity.max(1);
    let shared = Arc::new(Shared {
        samples: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
        meta: (0..capacity).map(|_| AtomicU32::new(0)).collect(),
        written: AtomicU64::new(0),
        claimed: AtomicU64::new(0),
        read: AtomicU64::new(0),
        dropped: AtomicU64::new(0),
        policy,
        origin,
        reader_gone: AtomicBool::new(false),
        writer_gone: AtomicBool::new(false),
    });
    (
        Producer { shared: Arc::clone(&shared), lost: 0, flags: TapFlags::default() },
        Consumer { shared, pos: 0, stream: origin, lost_total: 0, overwritten: 0 },
    )
}

impl Shared {
    fn capacity(&self) -> u64 {
        self.samples.len() as u64
    }

    fn slot_index(&self, index: u64) -> usize {
        // The modulo result is below `samples.len()`, so it fits `usize`.
        #[allow(clippy::cast_possible_truncation)]
        {
            (index % self.capacity()) as usize
        }
    }
}

impl Producer {
    /// Appends `samples` and returns how many were accepted.
    ///
    /// With `DropNewest`, what does not fit is refused and counted ([`dropped`](Self::dropped)); with `Overwrite`,
    /// everything is accepted.
    pub fn push(&mut self, samples: &[f32]) -> usize {
        match self.shared.policy {
            DropPolicy::DropNewest => self.push_drop_newest(samples),
            DropPolicy::Overwrite => self.push_overwrite(samples),
        }
    }

    fn meta(&self) -> u32 {
        (self.lost & GAP_MASK) | self.flags.bits()
    }

    fn push_drop_newest(&mut self, samples: &[f32]) -> usize {
        let shared = &*self.shared;
        let written = shared.written.load(Ordering::Relaxed);
        let read = shared.read.load(Ordering::Acquire);
        let free = shared.capacity() - (written - read);
        let accepted = samples.len().min(usize::try_from(free).unwrap_or(usize::MAX));
        let meta = self.meta();
        for (offset, sample) in samples[..accepted].iter().enumerate() {
            let slot = shared.slot_index(written + offset as u64);
            shared.samples[slot].store(sample.to_bits(), Ordering::Relaxed);
            shared.meta[slot].store(meta, Ordering::Relaxed);
        }
        shared.written.store(written + accepted as u64, Ordering::Release);
        let refused = samples.len() - accepted;
        if refused > 0 {
            shared.dropped.fetch_add(refused as u64, Ordering::Relaxed);
            self.push_gap(refused as u64);
        }
        accepted
    }

    fn push_overwrite(&mut self, samples: &[f32]) -> usize {
        let shared = &*self.shared;
        let written = shared.written.load(Ordering::Relaxed);
        let end = written + samples.len() as u64;
        // Announce the batch before touching slots, so a reader that sees any new slot value also sees this claim.
        shared.claimed.store(end, Ordering::Relaxed);
        fence(Ordering::Release);
        // A batch longer than the ring only keeps its tail; the rest would be overwritten at once.
        let keep = samples.len().min(usize::try_from(shared.capacity()).unwrap_or(usize::MAX));
        let first = samples.len() - keep;
        let meta = self.meta();
        for (offset, sample) in samples[first..].iter().enumerate() {
            let slot = shared.slot_index(written + (first + offset) as u64);
            shared.samples[slot].store(sample.to_bits(), Ordering::Relaxed);
            shared.meta[slot].store(meta, Ordering::Relaxed);
        }
        shared.written.store(end, Ordering::Release);
        samples.len()
    }

    /// Records `count` stream positions that were never captured (a lost device): the next sample is stamped as coming
    /// after them, and the consumer's next run reports them in `gap_before`.
    pub fn push_gap(&mut self, count: u64) {
        // Truncation is the wrap the format documents.
        #[allow(clippy::cast_possible_truncation)]
        let count = (count & u64::from(GAP_MASK)) as u32;
        self.lost = self.lost.wrapping_add(count) & GAP_MASK;
    }

    /// Sets the flags stamped on every sample pushed from now on.
    pub fn set_flags(&mut self, flags: TapFlags) {
        self.flags = flags;
    }

    /// Samples that fit right now. A feeder that must not lose audio (a file played faster than real time) waits until
    /// this covers its block before pushing. With `Overwrite` it is always the capacity.
    #[must_use]
    pub fn free(&self) -> usize {
        let shared = &*self.shared;
        match shared.policy {
            DropPolicy::Overwrite => usize::try_from(shared.capacity()).unwrap_or(usize::MAX),
            DropPolicy::DropNewest => {
                let in_use = shared.written.load(Ordering::Relaxed) - shared.read.load(Ordering::Acquire);
                usize::try_from(shared.capacity() - in_use).unwrap_or(usize::MAX)
            }
        }
    }

    /// Total samples accepted so far.
    #[must_use]
    pub fn written(&self) -> u64 {
        self.shared.written.load(Ordering::Relaxed)
    }

    /// Total samples refused so far because the ring was full (`DropNewest` only).
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }
}

impl TapSink for Producer {
    fn push(&mut self, samples: &[f32]) -> usize {
        Producer::push(self, samples)
    }

    fn flags(&mut self, flags: TapFlags) {
        self.set_flags(flags);
    }
}

impl Producer {
    /// Whether the consumer was dropped, which closes a tap (design doc section 3.7). The audio thread reads the flag at
    /// each block and never waits for it.
    #[must_use]
    pub fn reader_gone(&self) -> bool {
        self.shared.reader_gone.load(Ordering::Relaxed)
    }
}

impl Drop for Producer {
    fn drop(&mut self) {
        self.shared.writer_gone.store(true, Ordering::Relaxed);
    }
}

impl Drop for Consumer {
    fn drop(&mut self) {
        self.shared.reader_gone.store(true, Ordering::Relaxed);
    }
}

/// How many lost positions lie between two running totals.
fn lost_between(earlier: u32, later: u32) -> u64 {
    u64::from(later.wrapping_sub(earlier) & GAP_MASK)
}

impl Consumer {
    /// Moves the next run of samples into the front of `out` and says where it sits on the stream.
    ///
    /// The run is contiguous on the stream, has one set of flags, and ends early at a gap or a flag change, so call
    /// again for the rest. With `Overwrite`, a consumer that fell more than a capacity behind skips ahead to the oldest
    /// intact sample and reports the skipped positions in `gap_before`.
    pub fn read(&mut self, out: &mut [f32]) -> TapRead {
        let shared = &*self.shared;
        let mut start = self.pos;
        // A writer lapping the reader mid-copy forces a retry; it can only do so a bounded number of times in practice,
        // and a persistent lapper leaves the run empty rather than torn.
        for _ in 0..8 {
            let written = shared.written.load(Ordering::Acquire);
            if shared.policy == DropPolicy::Overwrite {
                start = start.max(written.saturating_sub(shared.capacity()));
            }
            let (len, first_meta) = copy_run(shared, start, written, out);
            if shared.policy == DropPolicy::Overwrite {
                fence(Ordering::Acquire);
                let intact_from = shared.claimed.load(Ordering::Relaxed).saturating_sub(shared.capacity());
                if intact_from > start {
                    start = intact_from;
                    continue;
                }
            }
            return self.finish(start, len, first_meta);
        }
        self.finish(start, 0, 0)
    }

    /// Fills `out` from successive runs, ignoring gaps and flags, and returns how many samples were read. For a
    /// consumer that only wants audio (the engine reading a feed), not positions.
    pub fn pop_into(&mut self, out: &mut [f32]) -> usize {
        let mut filled = 0;
        while filled < out.len() {
            let run = self.read(&mut out[filled..]);
            if run.len == 0 {
                break;
            }
            filled += run.len;
        }
        filled
    }

    fn finish(&mut self, start: u64, len: usize, first_meta: u32) -> TapRead {
        if len == 0 {
            // Nothing readable: change no state, so a skip-ahead is reported by the next run that has samples.
            return TapRead { len: 0, start_sample: self.stream, ..TapRead::default() };
        }
        let skipped = start - self.pos;
        let gap_before = skipped + lost_between(self.lost_total, first_meta & GAP_MASK);
        let start_sample = self.stream + gap_before;
        let flags = TapFlags::from_meta(first_meta);
        self.overwritten += skipped;
        self.pos = start + len as u64;
        self.stream = start_sample + len as u64;
        self.lost_total = first_meta & GAP_MASK;
        if self.shared.policy == DropPolicy::DropNewest {
            self.shared.read.store(self.pos, Ordering::Release);
        }
        TapRead { len, start_sample, gap_before, muted: flags.muted, private: flags.private }
    }

    /// Samples waiting to be read (for `Overwrite`, at most the capacity).
    #[must_use]
    pub fn available(&self) -> usize {
        let written = self.shared.written.load(Ordering::Acquire);
        let waiting = written - self.pos;
        usize::try_from(waiting.min(self.shared.capacity())).unwrap_or(usize::MAX)
    }

    /// Total samples the producer refused so far (`DropNewest`).
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// Total samples this consumer skipped because the producer overwrote them (`Overwrite`).
    #[must_use]
    pub fn overwritten(&self) -> u64 {
        self.overwritten
    }

    /// Whether the producer was dropped, which closes a feed (design doc section 3.7): the source then plays silence.
    #[must_use]
    pub fn writer_gone(&self) -> bool {
        self.shared.writer_gone.load(Ordering::Relaxed)
    }

    /// The stream position of the first sample the producer wrote.
    #[must_use]
    pub fn origin(&self) -> u64 {
        self.shared.origin
    }
}

/// Copies the run that starts at accepted index `start` into `out`. Returns its length and the meta word of its first
/// sample. The run ends before the first sample whose lost total or flags differ from the first one's.
fn copy_run(shared: &Shared, start: u64, written: u64, out: &mut [f32]) -> (usize, u32) {
    let available = usize::try_from(written.saturating_sub(start)).unwrap_or(usize::MAX);
    let wanted = out.len().min(available);
    let mut first_meta = 0;
    let mut len = 0;
    for (offset, slot_out) in out[..wanted].iter_mut().enumerate() {
        let slot = shared.slot_index(start + offset as u64);
        let meta = shared.meta[slot].load(Ordering::Relaxed);
        if offset == 0 {
            first_meta = meta;
        } else if meta != first_meta {
            break;
        }
        *slot_out = f32::from_bits(shared.samples[slot].load(Ordering::Relaxed));
        len += 1;
    }
    (len, first_meta)
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // positions in tests stay far below 2^24
mod tests {
    use super::*;

    fn ramp(from: u32, to: u32) -> Vec<f32> {
        (from..to).map(|v| v as f32).collect()
    }

    #[test]
    fn samples_come_out_in_order_across_the_wrap() {
        let (mut tx, mut rx) = ring(8, DropPolicy::DropNewest, 0);
        let mut out = [0.0; 8];
        let mut next = 0u32;
        for _ in 0..10 {
            let block = ramp(next, next + 5);
            next += 5;
            assert_eq!(tx.push(&block), 5);
            assert_eq!(rx.pop_into(&mut out[..5]), 5);
            assert_eq!(&out[..5], block.as_slice());
        }
    }

    #[test]
    fn a_full_ring_refuses_the_newest_samples_and_counts_them() {
        let (mut tx, mut rx) = ring(4, DropPolicy::DropNewest, 0);
        assert_eq!(tx.push(&[1.0, 2.0, 3.0]), 3);
        assert_eq!(tx.push(&[4.0, 5.0, 6.0]), 1);
        assert_eq!((tx.dropped(), rx.dropped()), (2, 2));
        assert_eq!((tx.written(), tx.free(), rx.available()), (4, 0, 4));
        let mut out = [0.0; 8];
        assert_eq!(rx.pop_into(&mut out), 4);
        assert_eq!(&out[..4], &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!(rx.pop_into(&mut out), 0);
    }

    #[test]
    fn a_refused_tail_is_reported_as_a_gap_before_the_next_run() {
        let (mut tx, mut rx) = ring(4, DropPolicy::DropNewest, 1000);
        tx.push(&[0.0, 1.0, 2.0]);
        tx.push(&[3.0, 4.0, 5.0]); // 3.0 fits; 4.0 and 5.0 are refused
        let mut out = [0.0; 8];
        let first = rx.read(&mut out);
        assert_eq!((first.len, first.start_sample, first.gap_before), (4, 1000, 0), "{first:?}");
        // The stream is now at position 1006 (positions 1004 and 1005 were refused).
        tx.push(&[6.0, 7.0]);
        let second = rx.read(&mut out);
        assert_eq!((second.len, second.start_sample, second.gap_before), (2, 1006, 2), "{second:?}");
        assert_eq!(&out[..2], &[6.0, 7.0]);
    }

    #[test]
    fn a_never_captured_gap_shifts_the_stream_position() {
        let (mut tx, mut rx) = ring(16, DropPolicy::DropNewest, 0);
        tx.push(&[1.0, 1.0]);
        tx.push_gap(480);
        tx.push(&[2.0, 2.0, 2.0]);
        let mut out = [0.0; 16];
        let a = rx.read(&mut out);
        let b = rx.read(&mut out);
        assert_eq!((a.len, a.start_sample, a.gap_before), (2, 0, 0));
        assert_eq!((b.len, b.start_sample, b.gap_before), (3, 482, 480));
        assert_eq!(rx.read(&mut out).len, 0);
    }

    #[test]
    fn flags_end_a_run_and_ride_with_the_samples() {
        let (mut tx, mut rx) = ring(16, DropPolicy::DropNewest, 0);
        tx.push(&[1.0, 1.0]);
        tx.set_flags(TapFlags { muted: true, private: false });
        tx.push(&[0.0, 0.0, 0.0]);
        tx.set_flags(TapFlags { muted: true, private: true });
        tx.push(&[0.0]);
        let mut out = [9.0; 16];
        let runs: Vec<TapRead> = (0..4).map(|_| rx.read(&mut out)).collect();
        let shape: Vec<(usize, u64, bool, bool)> =
            runs.iter().map(|r| (r.len, r.start_sample, r.muted, r.private)).collect();
        assert_eq!(shape, vec![(2, 0, false, false), (3, 2, true, false), (1, 5, true, true), (0, 6, false, false)]);
        assert!(runs.iter().all(|r| r.gap_before == 0));
    }

    #[test]
    fn overwrite_never_refuses_and_the_reader_skips_ahead() {
        let (mut tx, mut rx) = ring(8, DropPolicy::Overwrite, 100);
        assert_eq!(tx.push(&ramp(0, 20)), 20);
        assert_eq!((tx.dropped(), tx.free()), (0, 8));
        let mut out = [0.0; 32];
        let run = rx.read(&mut out);
        // 20 written, 8 kept: the reader starts at sample 12, 12 positions after the origin.
        assert_eq!((run.len, run.start_sample, run.gap_before), (8, 112, 12), "{run:?}");
        assert_eq!(&out[..8], ramp(12, 20).as_slice());
        assert_eq!(rx.overwritten(), 12);
        tx.push(&ramp(20, 23));
        let next = rx.read(&mut out);
        assert_eq!((next.len, next.start_sample, next.gap_before), (3, 120, 0));
    }

    #[test]
    fn overwrite_gaps_before_skipped_samples_are_still_counted() {
        let (mut tx, mut rx) = ring(4, DropPolicy::Overwrite, 0);
        tx.push(&ramp(0, 2));
        tx.push_gap(10);
        tx.push(&ramp(2, 10)); // laps the ring: the reader sees only 6..10
        let mut out = [0.0; 16];
        let run = rx.read(&mut out);
        // Stream: 0,1 | 10 lost | 2..9 at positions 12..19; the reader returns samples 6..9 at positions 16..19.
        assert_eq!((run.start_sample, run.len), (16, 4), "{run:?}");
        assert_eq!(run.gap_before, 16);
    }

    #[test]
    fn dropping_either_end_sets_a_flag_the_other_end_can_poll() {
        let (tx, rx) = ring(4, DropPolicy::DropNewest, 0);
        assert!(!tx.reader_gone() && !rx.writer_gone());
        drop(rx);
        assert!(tx.reader_gone());
        let (tx, rx) = ring(4, DropPolicy::Overwrite, 0);
        drop(tx);
        assert!(rx.writer_gone());
    }

    #[test]
    fn a_batch_longer_than_the_ring_keeps_its_tail() {
        let (mut tx, mut rx) = ring(4, DropPolicy::Overwrite, 0);
        assert_eq!(tx.push(&ramp(0, 10)), 10);
        let mut out = [0.0; 8];
        let run = rx.read(&mut out);
        assert_eq!((run.len, run.start_sample, run.gap_before), (4, 6, 6));
        assert_eq!(&out[..4], &[6.0, 7.0, 8.0, 9.0]);
    }

    /// A sample's value is its stream position, so every run can be checked against its own stamp.
    fn assert_run_matches_stamp(run: &TapRead, samples: &[f32]) {
        for (i, v) in samples[..run.len].iter().enumerate() {
            assert_eq!(*v, (run.start_sample + i as u64) as f32, "a sample carries the wrong position: {run:?}");
        }
    }

    #[test]
    fn a_lossless_feeder_gets_every_sample_in_order_with_no_gap() {
        let (mut tx, mut rx) = ring(64, DropPolicy::DropNewest, 0);
        let total = 200_000u32;
        let producer = std::thread::spawn(move || {
            let mut sent = 0u32;
            while sent < total {
                let end = (sent + 37).min(total);
                // The documented pattern for a feeder that must not lose audio: wait until the block fits.
                if tx.free() < (end - sent) as usize {
                    std::thread::yield_now();
                    continue;
                }
                assert_eq!(tx.push(&ramp(sent, end)), (end - sent) as usize);
                sent = end;
            }
        });
        let mut buf = [0.0f32; 50];
        let mut seen = 0u64;
        while seen < u64::from(total) {
            let run = rx.read(&mut buf);
            if run.len == 0 {
                std::thread::yield_now();
                continue;
            }
            assert_run_matches_stamp(&run, &buf);
            assert_eq!((run.start_sample, run.gap_before), (seen, 0), "{run:?}");
            seen += run.len as u64;
        }
        producer.join().unwrap();
        assert_eq!(rx.dropped(), 0);
    }

    #[test]
    fn a_lossy_feeder_across_threads_accounts_for_every_sample() {
        let (mut tx, mut rx) = ring(64, DropPolicy::DropNewest, 0);
        let total = 200_000u64;
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let producer = {
            let done = Arc::clone(&done);
            std::thread::spawn(move || {
                let mut sent = 0u32;
                while u64::from(sent) < total {
                    let end = (sent + 37).min(total as u32);
                    tx.push(&ramp(sent, end)); // what does not fit is refused: its positions become a gap
                    sent = end;
                }
                done.store(true, Ordering::Release);
            })
        };
        let (mut buf, mut delivered, mut gaps, mut reads) = ([0.0f32; 50], 0u64, 0u64, 0u32);
        loop {
            let finished = done.load(Ordering::Acquire);
            let run = rx.read(&mut buf);
            if run.len == 0 {
                if finished {
                    break;
                }
                std::thread::yield_now();
                continue;
            }
            assert_run_matches_stamp(&run, &buf);
            delivered += run.len as u64;
            gaps += run.gap_before;
            reads += 1;
            if reads % 3 == 0 {
                std::thread::yield_now(); // a slow consumer, so the ring fills now and then
            }
        }
        producer.join().unwrap();
        assert_eq!(delivered + rx.dropped(), total, "every offered sample is delivered or counted as refused");
        assert!(gaps <= rx.dropped(), "reported gaps {gaps} exceed refusals {}", rx.dropped());
    }

    #[test]
    fn overwrite_across_threads_never_returns_a_torn_run() {
        let (mut tx, mut rx) = ring(64, DropPolicy::Overwrite, 0);
        let total = 400_000u32;
        let producer = std::thread::spawn(move || {
            let mut sent = 0u32;
            while sent < total {
                let end = (sent + 37).min(total);
                tx.push(&ramp(sent, end));
                sent = end;
            }
        });
        let mut skipped = 0;
        let mut buf = [0.0f32; 50];
        let mut seen_end = 0u64;
        // The reader is slower than the writer on purpose: it must skip ahead, and every run it does return must be
        // exactly the samples its stamp claims, never a mixture of old and new.
        while seen_end < u64::from(total) {
            let run = rx.read(&mut buf);
            if run.len == 0 {
                std::thread::yield_now();
                continue;
            }
            assert_run_matches_stamp(&run, &buf);
            assert_eq!(run.start_sample, seen_end + run.gap_before, "{run:?}");
            skipped += run.gap_before;
            seen_end = run.start_sample + run.len as u64;
            if seen_end.is_multiple_of(7) {
                std::thread::yield_now();
            }
        }
        producer.join().unwrap();
        assert_eq!(skipped, rx.overwritten(), "every skipped position is an overwritten sample here");
    }
}
