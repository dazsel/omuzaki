//! The scripted audio source: plays back runs of samples with gaps and flags.

use std::collections::VecDeque;

use omuzaki_audio_api::ring::TapRead;

use crate::timeline::AudioSource;

/// An [`AudioSource`] that plays back scripted runs in order, then returns empty runs.
///
/// A run's `len` is set from its samples when it is read, so a script states only the position and flags. A run longer
/// than the caller's buffer is read in pieces: the first piece carries the run's gap and flags, the rest carry neither.
/// A read into an empty buffer returns an empty result and leaves the pending run, its gap included, as it was.
pub struct ScriptedSource {
    /// The pending runs; the front one has its first `offset` samples already returned.
    runs: VecDeque<(TapRead, Vec<f32>)>,
    /// Samples of the front run already returned, so a piece costs its own length and not the run's.
    offset: usize,
    /// The stream position just after the last sample returned.
    pos: u64,
}

impl ScriptedSource {
    /// A source that plays `runs` in order.
    pub fn new(runs: Vec<(TapRead, Vec<f32>)>) -> Self {
        let pos = runs.first().map_or(0, |(run, _)| run.start_sample);
        Self { runs: runs.into(), offset: 0, pos }
    }

    /// A source with one run of `frames` samples from stream position `start_sample`. Each sample holds its own stream
    /// position, so a test can read off where every sample sits.
    #[allow(clippy::cast_precision_loss)] // Positions stay far below 2^24, where `f32` holds them exactly.
    pub fn ramp(start_sample: u64, frames: usize) -> Self {
        let samples = (0..frames).map(|i| (start_sample + i as u64) as f32).collect();
        Self::new(vec![(TapRead { start_sample, ..TapRead::default() }, samples)])
    }
}

impl AudioSource for ScriptedSource {
    fn read(&mut self, out: &mut [f32]) -> TapRead {
        let Some((run, samples)) = self.runs.front_mut() else {
            return TapRead { start_sample: self.pos, ..TapRead::default() };
        };
        if out.is_empty() {
            return TapRead { start_sample: self.pos, ..TapRead::default() };
        }
        let rest = &samples[self.offset..];
        let len = out.len().min(rest.len());
        out[..len].copy_from_slice(&rest[..len]);
        let read = TapRead {
            len,
            start_sample: run.start_sample,
            gap_before: run.gap_before,
            muted: run.muted,
            private: run.private,
        };
        if len == rest.len() {
            self.runs.pop_front();
            self.offset = 0;
        } else {
            self.offset += len;
            run.start_sample += len as u64;
            run.gap_before = 0;
        }
        self.pos = read.start_sample + len as u64;
        read
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::cast_precision_loss, clippy::float_cmp)] // Ramp values are exact integers in `f32`.
    fn a_ramp_reads_back_in_contiguous_chunks() {
        let mut source = ScriptedSource::ramp(0, 480);
        let mut buf = [0.0; 100];
        let (mut next, mut total) = (0, 0);
        loop {
            let run = source.read(&mut buf);
            if run.len == 0 {
                break;
            }
            assert_eq!(run.start_sample, next);
            assert_eq!((run.gap_before, run.muted, run.private), (0, false, false));
            for (i, sample) in buf[..run.len].iter().enumerate() {
                assert_eq!(*sample, (run.start_sample + i as u64) as f32);
            }
            next = run.start_sample + run.len as u64;
            total += run.len;
        }
        assert_eq!(total, 480);
    }

    #[test]
    fn scripted_gaps_and_flags_come_back_exactly() {
        let first = (TapRead { start_sample: 0, ..TapRead::default() }, vec![0.5; 96]);
        let second = (
            TapRead { start_sample: 192, gap_before: 96, muted: true, private: false, ..TapRead::default() },
            vec![0.0; 32],
        );
        let mut source = ScriptedSource::new(vec![first, second]);
        let mut buf = [0.0; 1000];
        let a = source.read(&mut buf);
        assert_eq!((a.len, a.start_sample, a.gap_before, a.muted, a.private), (96, 0, 0, false, false));
        let b = source.read(&mut buf);
        assert_eq!((b.len, b.start_sample, b.gap_before, b.muted, b.private), (32, 192, 96, true, false));
        assert_eq!(source.read(&mut buf).len, 0);
    }

    #[test]
    fn a_read_into_an_empty_buffer_leaves_the_pending_run_untouched() {
        let run = (TapRead { start_sample: 10, gap_before: 4, muted: true, ..TapRead::default() }, vec![1.0; 5]);
        let mut source = ScriptedSource::new(vec![run]);
        let empty = source.read(&mut []);
        assert_eq!((empty.len, empty.gap_before, empty.muted), (0, 0, false));
        let mut buf = [0.0; 8];
        let a = source.read(&mut buf);
        assert_eq!((a.len, a.start_sample, a.gap_before, a.muted), (5, 10, 4, true));
        assert_eq!(source.read(&mut []).len, 0);
    }

    #[test]
    fn a_run_longer_than_the_buffer_is_read_in_pieces_and_the_gap_is_reported_once() {
        let run = (TapRead { start_sample: 10, gap_before: 4, private: true, ..TapRead::default() }, vec![1.0; 5]);
        let mut source = ScriptedSource::new(vec![run]);
        let mut buf = [0.0; 3];
        let a = source.read(&mut buf);
        assert_eq!((a.len, a.start_sample, a.gap_before, a.private), (3, 10, 4, true));
        let b = source.read(&mut buf);
        assert_eq!((b.len, b.start_sample, b.gap_before, b.private), (2, 13, 0, true));
    }
}
