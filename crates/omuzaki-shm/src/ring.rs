//! [`SampleRing`]: audio from the audio worker to the host (`docs/design/supervisor.md` §4.3, §10).
//!
//! A single-producer, single-consumer ring of interleaved `f32` frames, sized for about 2 s, with what recording needs
//! carried alongside: per run, the stream position of its first frame, the positions lost before it, and the `muted`
//! and `private` flags (the fields of `omuzaki-audio-api`'s `TapRead`). **A muted or private run crosses as its
//! length and flags only:** no sample value of it is written to shared memory. The header also carries the
//! generation's clock mapping, a seqlock read with the same bound as [`Latest`](crate::Latest).
//!
//! `push` and `pop` are wait-free and allocate nothing.
//!
//! Layout: the tag, the channel count, the capacity in frames, the run table's size, the producer's frame and run
//! counts, the consumer's frame and run counts, the clock seqlock (sequence, sample, nanoseconds, seconds per
//! sample), then the run table (start position, frames, ring position, flags per run), then the samples.

use std::sync::atomic::{AtomicU64, Ordering, fence};

use crate::header::{self, WORD};
use crate::latest::LATEST_READ_TRIES;
use crate::region::Region;
use crate::{Share, ShmError};

const KIND: u8 = 3;
const SAMPLE_HEAD: usize = 4;
const SAMPLE_TAIL: usize = 5;
const RUN_HEAD: usize = 6;
const RUN_TAIL: usize = 7;
const CLOCK_SEQ: usize = 8;
const CLOCK_SAMPLE: usize = 9;
const CLOCK_AT: usize = 10;
const CLOCK_RATE: usize = 11;
const HEADER_WORDS: usize = 12;
/// One run's entry: start position, frames, ring position, flags.
const RUN: usize = 4 * WORD;
const MUTED: u64 = 1;
const PRIVATE: u64 = 2;
/// The run's frames are not in the ring (muted or private).
const WITHHELD: u64 = 4;
/// Samples a push or pop converts at a time, on the stack.
const CHUNK: usize = 256;

/// Where a generation's audio sits in time: sample `sample` was at `at_ns` on [`host_now`](crate::host_now)'s clock.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ClockMapping {
    /// A stream position.
    pub sample: u64,
    /// Its time on the host clock, in nanoseconds.
    pub at_ns: u64,
    /// The length of one sample in seconds.
    pub seconds_per_sample: f64,
}

/// What a pop returned besides the samples.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RunInfo {
    /// How many frames were written to the front of the buffer (each `channels` samples); 0 when nothing waits.
    pub len: usize,
    /// The stream position of the first frame.
    pub start_sample: u64,
    /// The stream positions lost between the previous run and this one; 0 when the stream is contiguous.
    pub gap_before: u64,
    /// The run was silenced by a mute: its frames are zeros.
    pub muted: bool,
    /// The run was private (`MicMode::CohostOnly`): its frames are zeros.
    pub private: bool,
}

/// The consumer's place, kept in this end only.
#[derive(Debug, Default)]
struct Reader {
    /// Runs fully read.
    run_tail: AtomicU64,
    /// Frames of the current run already read.
    offset: AtomicU64,
    /// The stream position after the last run read, plus one (0: none yet).
    end: AtomicU64,
}

/// One sample ring.
#[derive(Debug)]
pub struct SampleRing {
    region: Region,
    channels: u16,
    frames: u64,
    runs: u64,
    run_table: usize,
    samples: usize,
    reader: Reader,
}

impl SampleRing {
    /// A ring of `frames` frames of `channels` channels (the producer's end) and the [`Share`] for the host.
    ///
    /// # Errors
    /// [`ShmError`] for no channels or no frames, or when the OS refuses.
    pub fn create(channels: u16, frames: usize) -> Result<(Self, Share), ShmError> {
        if channels == 0 || frames == 0 {
            return Err(ShmError::new("create", format!("{channels} channels of {frames} frames")));
        }
        // One run per push; an engine pushes once per block, so a run per 16 frames is plenty for 2 s.
        let runs = (frames / 16).clamp(64, 65_536);
        let (run_table, samples, len) = Self::layout(channels, frames, runs)?;
        let (region, share) = Region::create(len)?;
        header::write(&region, KIND, &[u64::from(channels), frames as u64, runs as u64])?;
        Ok((Self::with(region, channels, frames, runs, run_table, samples), share))
    }

    /// The host's end of the ring `share` hands over.
    ///
    /// # Errors
    /// [`ShmError`] when the object is not a sample ring, or its header does not fit its size.
    pub fn open(share: Share) -> Result<Self, ShmError> {
        let region = Region::open(share)?;
        let geometry = header::read(&region, KIND, 3)?;
        let channels = u16::try_from(geometry[0]).map_err(|_| ShmError::new("open", "channel count"))?;
        let frames = header::size(geometry[1], "the frame count")?;
        let runs = header::size(geometry[2], "the run count")?;
        if channels == 0 || frames == 0 || runs == 0 {
            return Err(ShmError::new("open", "the ring header is not valid"));
        }
        let (run_table, samples, len) = Self::layout(channels, frames, runs)?;
        if len > region.len() {
            return Err(ShmError::new("open", "the ring is bigger than the object"));
        }
        let ring = Self::with(region, channels, frames, runs, run_table, samples);
        let tail = ring.word(RUN_TAIL)?.load(Ordering::Acquire);
        ring.reader.run_tail.store(tail, Ordering::Relaxed);
        Ok(ring)
    }

    fn with(region: Region, channels: u16, frames: usize, runs: usize, run_table: usize, samples: usize) -> Self {
        Self {
            region,
            channels,
            frames: frames as u64,
            runs: runs as u64,
            run_table,
            samples,
            reader: Reader::default(),
        }
    }

    fn layout(channels: u16, frames: usize, runs: usize) -> Result<(usize, usize, usize), ShmError> {
        let too_big = || ShmError::new("create", "the ring does not fit this machine's address space");
        let run_table = header::align(HEADER_WORDS * WORD).ok_or_else(too_big)?;
        let samples = runs.checked_mul(RUN).and_then(|n| header::align(run_table + n)).ok_or_else(too_big)?;
        let bytes = frames.checked_mul(usize::from(channels)).and_then(|n| n.checked_mul(4)).ok_or_else(too_big)?;
        Ok((run_table, samples, samples.checked_add(bytes).ok_or_else(too_big)?))
    }

    /// The channel count.
    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// The capacity in frames.
    pub fn frames(&self) -> usize {
        usize::try_from(self.frames).unwrap_or(usize::MAX)
    }

    fn word(&self, i: usize) -> Result<&AtomicU64, ShmError> {
        header::word(&self.region, i)
    }

    fn run_word(&self, run: u64, field: usize) -> Option<&AtomicU64> {
        let at = usize::try_from(run % self.runs).ok()?.checked_mul(RUN)?;
        self.region.atomic(self.run_table + at + field * WORD)
    }

    /// Writes `values` into the ring starting at frame position `pos`, wrapping.
    fn put_samples(&self, pos: u64, values: &[f32]) -> Result<(), ShmError> {
        let ch = usize::from(self.channels);
        let mut buf = [0_u8; CHUNK * 4];
        let mut frame = pos;
        for chunk in values.chunks(CHUNK / ch * ch) {
            for (i, v) in chunk.iter().enumerate() {
                buf[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
            }
            self.put_bytes(frame, &buf[..chunk.len() * 4])?;
            frame += (chunk.len() / ch) as u64;
        }
        Ok(())
    }

    fn put_bytes(&self, frame: u64, bytes: &[u8]) -> Result<(), ShmError> {
        let frame_bytes = usize::from(self.channels) * 4;
        let start = usize::try_from(frame % self.frames).map_err(|_| ShmError::new("ring", "position"))? * frame_bytes;
        let room = self.frames() * frame_bytes - start;
        let first = bytes.len().min(room);
        self.region.copy_in(self.samples + start, &bytes[..first])?;
        self.region.copy_in(self.samples, &bytes[first..])
    }

    fn get_samples(&self, frame: u64, out: &mut [f32]) -> Result<(), ShmError> {
        let ch = usize::from(self.channels);
        let frame_bytes = ch * 4;
        let mut buf = [0_u8; CHUNK * 4];
        let mut at = frame;
        for chunk in out.chunks_mut(CHUNK / ch * ch) {
            let bytes = &mut buf[..chunk.len() * 4];
            let start = usize::try_from(at % self.frames).map_err(|_| ShmError::new("ring", "position"))? * frame_bytes;
            let first = bytes.len().min(self.frames() * frame_bytes - start);
            let (a, b) = bytes.split_at_mut(first);
            self.region.copy_out(self.samples + start, a)?;
            self.region.copy_out(self.samples, b)?;
            for (i, v) in chunk.iter_mut().enumerate() {
                *v = f32::from_le_bytes([bytes[i * 4], bytes[i * 4 + 1], bytes[i * 4 + 2], bytes[i * 4 + 3]]);
            }
            at += (chunk.len() / ch) as u64;
        }
        Ok(())
    }

    /// The producer's counts and how much room is left: `(sample_head, run_head, free frames, free runs)`.
    fn room(&self) -> Option<(u64, u64, u64, u64)> {
        let sample_head = self.word(SAMPLE_HEAD).ok()?.load(Ordering::Relaxed);
        let run_head = self.word(RUN_HEAD).ok()?.load(Ordering::Relaxed);
        let sample_tail = self.word(SAMPLE_TAIL).ok()?.load(Ordering::Acquire);
        let run_tail = self.word(RUN_TAIL).ok()?.load(Ordering::Acquire);
        let used_frames = sample_head.checked_sub(sample_tail).filter(|n| *n <= self.frames)?;
        let used_runs = run_head.checked_sub(run_tail).filter(|n| *n <= self.runs)?;
        Some((sample_head, run_head, self.frames - used_frames, self.runs - used_runs))
    }

    /// Publishes a run entry and advances the producer's counts.
    fn commit(&self, run_head: u64, entry: [u64; 4], sample_head: u64) -> Option<()> {
        for (field, value) in entry.into_iter().enumerate() {
            self.run_word(run_head, field)?.store(value, Ordering::Relaxed);
        }
        self.word(SAMPLE_HEAD).ok()?.store(sample_head, Ordering::Relaxed);
        self.word(RUN_HEAD).ok()?.store(run_head + 1, Ordering::Release);
        Some(())
    }

    /// The producer pushes a run of interleaved samples whose first frame is stream position `start_sample`; returns
    /// the frames written (fewer than given when the ring is nearly full, 0 when it is full).
    pub fn push(&self, start_sample: u64, interleaved: &[f32]) -> usize {
        let ch = usize::from(self.channels);
        let Some((sample_head, run_head, free, free_runs)) = self.room() else { return 0 };
        let n = ((interleaved.len() / ch) as u64).min(free);
        if n == 0 || free_runs == 0 {
            return 0;
        }
        let Ok(count) = usize::try_from(n) else { return 0 };
        if self.put_samples(sample_head, &interleaved[..count * ch]).is_err() {
            return 0;
        }
        match self.commit(run_head, [start_sample, n, sample_head, 0], sample_head + n) {
            Some(()) => count,
            None => 0,
        }
    }

    /// The producer pushes a muted or private run of `frames` frames: its position, length and flags only, no sample
    /// value. Returns `frames`, or 0 when the run table is full.
    pub fn push_withheld(&self, start_sample: u64, frames: usize, muted: bool, private: bool) -> usize {
        let Some((sample_head, run_head, _, free_runs)) = self.room() else { return 0 };
        if frames == 0 || free_runs == 0 {
            return 0;
        }
        let flags = WITHHELD | if muted { MUTED } else { 0 } | if private { PRIVATE } else { 0 };
        match self.commit(run_head, [start_sample, frames as u64, sample_head, flags], sample_head) {
            Some(()) => frames,
            None => 0,
        }
    }

    /// The consumer pops one contiguous run (or the front of one) into `out`, interleaved; withheld runs come back as
    /// zeros with their flags set. `len` 0 when nothing waits, or when the producer's entries cannot be right.
    pub fn pop(&self, out: &mut [f32]) -> RunInfo {
        self.pop_checked(out).unwrap_or_default()
    }

    fn pop_checked(&self, out: &mut [f32]) -> Option<RunInfo> {
        let ch = usize::from(self.channels);
        let run_tail = self.reader.run_tail.load(Ordering::Relaxed);
        let run_head = self.word(RUN_HEAD).ok()?.load(Ordering::Acquire);
        if run_head <= run_tail || run_head - run_tail > self.runs {
            return None;
        }
        let field = |i| self.run_word(run_tail, i).map(|w| w.load(Ordering::Relaxed));
        let (start, frames, pos, flags) = (field(0)?, field(1)?, field(2)?, field(3)?);
        let withheld = flags & WITHHELD != 0;
        if frames == 0 || (!withheld && frames > self.frames) {
            return None;
        }
        let offset = self.reader.offset.load(Ordering::Relaxed);
        let take = (frames - offset).min((out.len() / ch) as u64);
        let take_usize = usize::try_from(take).ok()?;
        if take_usize == 0 {
            return None;
        }
        let dest = &mut out[..take_usize * ch];
        if withheld {
            dest.fill(0.0);
        } else {
            self.get_samples(pos + offset, dest).ok()?;
        }
        let end = self.reader.end.load(Ordering::Relaxed);
        let gap_before = if offset == 0 && end > 0 { (start + offset).saturating_sub(end - 1) } else { 0 };
        let info = RunInfo {
            len: take_usize,
            start_sample: start + offset,
            gap_before,
            muted: flags & MUTED != 0,
            private: flags & PRIVATE != 0,
        };
        if offset + take == frames {
            self.reader.offset.store(0, Ordering::Relaxed);
            self.reader.run_tail.store(run_tail + 1, Ordering::Relaxed);
            self.reader.end.store(start + frames + 1, Ordering::Relaxed);
            if !withheld {
                self.word(SAMPLE_TAIL).ok()?.store(pos + frames, Ordering::Release);
            }
            self.word(RUN_TAIL).ok()?.store(run_tail + 1, Ordering::Release);
        } else {
            self.reader.offset.store(offset + take, Ordering::Relaxed);
            self.reader.end.store(start + offset + take + 1, Ordering::Relaxed);
        }
        Some(info)
    }

    /// The producer publishes the generation's clock mapping (at most once per block).
    pub fn publish_clock(&self, mapping: ClockMapping) {
        let (Ok(seq), Ok(sample), Ok(at), Ok(rate)) =
            (self.word(CLOCK_SEQ), self.word(CLOCK_SAMPLE), self.word(CLOCK_AT), self.word(CLOCK_RATE))
        else {
            return;
        };
        let start = seq.load(Ordering::Relaxed) | 1;
        seq.store(start, Ordering::Relaxed);
        fence(Ordering::Release);
        sample.store(mapping.sample, Ordering::Relaxed);
        at.store(mapping.at_ns, Ordering::Relaxed);
        rate.store(mapping.seconds_per_sample.to_bits(), Ordering::Relaxed);
        seq.store(start.wrapping_add(1), Ordering::Release);
    }

    /// The consumer reads the clock mapping: `None` before the first one, or when every one of the bounded tries met
    /// a write in progress (a producer killed mid-write leaves it `None` for good).
    pub fn clock(&self) -> Option<ClockMapping> {
        let (seq, sample, at, rate) = (
            self.word(CLOCK_SEQ).ok()?,
            self.word(CLOCK_SAMPLE).ok()?,
            self.word(CLOCK_AT).ok()?,
            self.word(CLOCK_RATE).ok()?,
        );
        for _ in 0..LATEST_READ_TRIES {
            let before = seq.load(Ordering::Acquire);
            if before == 0 {
                return None;
            }
            if before % 2 == 1 {
                continue;
            }
            let mapping = ClockMapping {
                sample: sample.load(Ordering::Relaxed),
                at_ns: at.load(Ordering::Relaxed),
                seconds_per_sample: f64::from_bits(rate.load(Ordering::Relaxed)),
            };
            fence(Ordering::Acquire);
            if seq.load(Ordering::Relaxed) == before {
                return Some(mapping);
            }
        }
        None
    }

    /// Starts publishing a clock mapping and stops halfway, as a producer killed mid-write would (the crash tests).
    #[cfg(feature = "testing")]
    pub fn publish_clock_torn(&self, mapping: ClockMapping) {
        if let (Ok(seq), Ok(sample)) = (self.word(CLOCK_SEQ), self.word(CLOCK_SAMPLE)) {
            seq.store(seq.load(Ordering::Relaxed) | 1, Ordering::Release);
            sample.store(mapping.sample, Ordering::Relaxed);
        }
    }

    /// Every byte of the object (the privacy test scans it).
    #[cfg(feature = "testing")]
    pub fn snapshot(&self) -> Vec<u8> {
        self.region.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_cross_with_positions_gaps_and_flags() -> Result<(), ShmError> {
        let (audio, share) = SampleRing::create(2, 64)?;
        let host = SampleRing::open(share)?;
        let ramp: Vec<f32> = (0..20_u16).map(f32::from).collect();
        assert_eq!(audio.push(100, &ramp), 10);
        assert_eq!(audio.push_withheld(110, 5, true, false), 5);
        assert_eq!(audio.push(120, &ramp[..8]), 4);
        let mut out = [9.0_f32; 40];
        let first = host.pop(&mut out);
        assert_eq!(first, RunInfo { len: 10, start_sample: 100, ..RunInfo::default() });
        assert_eq!(&out[..20], &ramp[..]);
        let muted = host.pop(&mut out);
        assert_eq!(muted, RunInfo { len: 5, start_sample: 110, muted: true, ..RunInfo::default() });
        assert!(out[..10].iter().all(|v| *v == 0.0));
        let after_gap = host.pop(&mut out);
        assert_eq!(after_gap, RunInfo { len: 4, start_sample: 120, gap_before: 5, ..RunInfo::default() });
        assert_eq!(host.pop(&mut out).len, 0);
        Ok(())
    }

    #[test]
    fn a_short_buffer_reads_a_run_in_pieces_and_the_ring_wraps() -> Result<(), ShmError> {
        let (audio, share) = SampleRing::create(1, 16)?;
        let host = SampleRing::open(share)?;
        let mut next = 0_u64;
        let mut read = Vec::new();
        for round in 0..20_u16 {
            let values: Vec<f32> = (0..12_u16).map(|i| f32::from(round * 12 + i)).collect();
            assert_eq!(audio.push(next, &values), 12);
            next += 12;
            let mut out = [0.0_f32; 5];
            loop {
                let info = host.pop(&mut out);
                if info.len == 0 {
                    break;
                }
                assert_eq!(info.gap_before, 0);
                read.extend_from_slice(&out[..info.len]);
            }
        }
        let want: Vec<f32> = (0..240_u16).map(f32::from).collect();
        assert_eq!(read, want);
        Ok(())
    }

    #[test]
    fn a_full_ring_refuses_and_the_clock_crosses() -> Result<(), ShmError> {
        let (audio, share) = SampleRing::create(1, 8)?;
        let host = SampleRing::open(share)?;
        assert_eq!(audio.push(0, &[1.0; 6]), 6);
        assert_eq!(audio.push(6, &[1.0; 6]), 2);
        assert_eq!(audio.push(8, &[1.0; 6]), 0);
        assert_eq!(host.clock(), None);
        let mapping = ClockMapping { sample: 480, at_ns: 123_456_789, seconds_per_sample: 1.0 / 48_000.0 };
        audio.publish_clock(mapping);
        assert_eq!(host.clock(), Some(mapping));
        assert_eq!((host.channels(), host.frames()), (1, 8));
        Ok(())
    }
}
