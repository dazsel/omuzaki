//! Turns a stream of reads of the shared texture into a [`ReceiverReport`]: which frames arrived, which were skipped, torn
//! or repeated, how old each was, and the hashes of sampled ones. Both receivers feed it, so their numbers mean the same.

use crate::clock::Clock;
use crate::frame_tag::{self, FrameTag, TagError};
use crate::report::{ReceiverReport, fnv1a};
use crate::scene;
use crate::stats::Summary;

/// Rows of tear lines kept in the report.
const KEEP_TEAR_ROWS: usize = 16;
/// Latency samples at the start of a run that are left out of the summaries: the connection is still being set up.
const WARMUP_READS: usize = 5;

/// What one read turned out to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Observation {
    /// A frame not seen before.
    New,
    /// A frame already counted (a second look at the same one, or an older one).
    Repeat,
}

/// Accumulates what the reads showed.
pub struct Tally {
    report: ReceiverReport,
    hash_every: u32,
    next_expected: u32,
    seen_ms: Vec<f64>,
    read_ms: Vec<f64>,
    interval_ms: Vec<f64>,
    previous_read: Option<u64>,
}

impl Tally {
    /// A tally that records the hash of every frame whose number is a multiple of `hash_every` (0 = none).
    #[must_use]
    pub fn new(hash_every: u32) -> Self {
        Self {
            report: ReceiverReport {
                received: 0,
                last_frame: 0,
                skipped: 0,
                repeats: 0,
                polls: 0,
                torn_polls: 0,
                tear_rows: Vec::new(),
                torn: 0,
                patch_failures: 0,
                seen_ms: Summary::default(),
                read_ms: Summary::default(),
                interval_ms: Summary::default(),
                sender_fps: 0.0,
                hashes: Vec::new(),
            },
            hash_every,
            next_expected: 0,
            seen_ms: Vec::new(),
            read_ms: Vec::new(),
            interval_ms: Vec::new(),
            previous_read: None,
        }
    }

    /// Records one read of a `width` x `height` BGRA frame. `seen` is when the receiver noticed a frame was there and
    /// `read` when the pixels were in memory, both in `clock` ticks.
    ///
    /// # Errors
    /// Fails when the frame is too small to carry a tag.
    pub fn observe(
        &mut self,
        clock: &Clock,
        pixels: &[u8],
        (width, height): (u32, u32),
        (seen, read): (u64, u64),
    ) -> Result<Observation, TagError> {
        let decoded = frame_tag::decode(pixels, width, height)?;
        let FrameTag { frame, ticks } = decoded.tag;
        self.report.polls += 1;
        let torn = !decoded.header_ok || !decoded.footer_ok;
        if torn {
            self.report.torn_polls += 1;
            if self.report.tear_rows.len() < KEEP_TEAR_ROWS
                && let Some(row) = frame_tag::tear_row(pixels, width, height)
            {
                self.report.tear_rows.push(row);
            }
        }
        if frame < self.next_expected {
            self.report.repeats += 1;
            return Ok(Observation::Repeat);
        }
        self.report.torn += u32::from(torn);
        self.report.patch_failures += u32::from(scene::check_patches(pixels, width, height).is_err());
        self.report.skipped += frame - self.next_expected;
        self.next_expected = frame + 1;
        self.report.received += 1;
        self.report.last_frame = frame;
        self.seen_ms.push(clock.ms_between(ticks, seen));
        self.read_ms.push(clock.ms_between(ticks, read));
        if let Some(previous) = self.previous_read {
            self.interval_ms.push(clock.ms_between(previous, read));
        }
        self.previous_read = Some(read);
        if self.hash_every > 0 && frame % self.hash_every == 0 {
            self.report.hashes.push((frame, fnv1a(pixels)));
        }
        Ok(Observation::New)
    }

    /// Frames counted so far.
    #[must_use]
    pub fn received(&self) -> u32 {
        self.report.received
    }

    /// The highest frame number seen so far.
    #[must_use]
    pub fn last_frame(&self) -> u32 {
        self.report.last_frame
    }

    /// Closes the tally into a report.
    #[must_use]
    pub fn finish(mut self, sender_fps: f64) -> ReceiverReport {
        let trim = |samples: &[f64]| Summary::of(samples.get(WARMUP_READS..).unwrap_or(samples));
        self.report.seen_ms = trim(&self.seen_ms);
        self.report.read_ms = trim(&self.read_ms);
        self.report.interval_ms = trim(&self.interval_ms);
        self.report.sender_fps = sender_fps;
        self.report
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::{PATCHES, patch_centre};

    const SIZE: (u32, u32) = (1280, 720);

    /// A frame carrying `frame`, with correct patches (so only tag problems show).
    fn frame(frame: u32) -> Vec<u8> {
        let mut pixels = vec![0u8; (SIZE.0 * SIZE.1 * 4) as usize];
        frame_tag::encode(FrameTag { frame, ticks: u64::from(frame) * 1000 }, &mut pixels, SIZE.0, SIZE.1).unwrap();
        for index in 0..PATCHES.len() {
            let (x, y) = patch_centre(index, SIZE.1);
            let at = ((y * SIZE.0 + x) * 4) as usize;
            pixels[at..at + 4].copy_from_slice(&scene::expected_patch(index));
        }
        pixels
    }

    fn observe(tally: &mut Tally, pixels: &[u8]) -> Observation {
        tally.observe(&Clock::new(), pixels, SIZE, (0, 0)).unwrap()
    }

    #[test]
    fn counts_new_skipped_and_repeated_frames() {
        let mut tally = Tally::new(0);
        let sequence = [0u32, 1, 1, 3, 2, 4];
        let seen: Vec<_> = sequence.iter().map(|&n| observe(&mut tally, &frame(n))).collect();
        assert_eq!(
            seen,
            [
                Observation::New,
                Observation::New,
                Observation::Repeat,
                Observation::New,
                Observation::Repeat,
                Observation::New
            ]
        );
        let report = tally.finish(60.0);
        assert_eq!((report.received, report.repeats, report.skipped, report.last_frame), (4, 2, 1, 4));
        assert_eq!(report.polls, 6);
    }

    #[test]
    fn a_torn_frame_is_counted_and_located() {
        let mut tally = Tally::new(0);
        let mut torn = frame(7);
        let later = frame(8);
        // The bottom half comes from the next frame, and carries its stamp.
        let half = (SIZE.0 * SIZE.1 * 2) as usize;
        torn[half..].copy_from_slice(&later[half..]);
        for row in SIZE.1 / 2..SIZE.1 - frame_tag::FOOTER_HEIGHT {
            for x in frame_tag::STAMP_X..frame_tag::STAMP_X + frame_tag::STAMP_WIDTH {
                let at = ((row * SIZE.0 + x) * 4) as usize;
                torn[at..at + 4].copy_from_slice(&[8, 8, 8, 255]);
            }
        }
        for row in frame_tag::HEADER_HEIGHT..SIZE.1 / 2 {
            for x in frame_tag::STAMP_X..frame_tag::STAMP_X + frame_tag::STAMP_WIDTH {
                let at = ((row * SIZE.0 + x) * 4) as usize;
                torn[at..at + 4].copy_from_slice(&[7, 7, 7, 255]);
            }
        }
        observe(&mut tally, &torn);
        let report = tally.finish(60.0);
        assert_eq!((report.torn, report.torn_polls), (1, 1));
        assert_eq!(report.tear_rows, [SIZE.1 / 2]);
    }

    #[test]
    fn a_torn_repeat_counts_as_an_exposed_read_but_not_a_torn_frame() {
        let mut tally = Tally::new(0);
        observe(&mut tally, &frame(5));
        let mut again = frame(5);
        let other = frame(6);
        let footer = ((SIZE.1 - frame_tag::FOOTER_HEIGHT) * SIZE.0 * 4) as usize;
        again[footer..].copy_from_slice(&other[footer..]);
        assert_eq!(observe(&mut tally, &again), Observation::Repeat);
        let report = tally.finish(60.0);
        assert_eq!((report.torn, report.torn_polls, report.repeats), (0, 1, 1));
    }

    #[test]
    fn sampled_frames_are_hashed() {
        let mut tally = Tally::new(2);
        for n in 0..5 {
            observe(&mut tally, &frame(n));
        }
        let report = tally.finish(60.0);
        let frames: Vec<u32> = report.hashes.iter().map(|(f, _)| *f).collect();
        assert_eq!(frames, [0, 2, 4]);
        assert_eq!(report.hashes[1].1, fnv1a(&frame(2)));
    }

    #[test]
    fn wrong_patch_bytes_are_counted() {
        let mut tally = Tally::new(0);
        let mut bad = frame(1);
        let (x, y) = patch_centre(0, SIZE.1);
        bad[((y * SIZE.0 + x) * 4) as usize] = 255;
        observe(&mut tally, &bad);
        assert_eq!(tally.finish(60.0).patch_failures, 1);
    }
}
