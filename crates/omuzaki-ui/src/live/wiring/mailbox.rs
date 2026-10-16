//! [`frame_mailbox`]: the newest-wins hand-off from the perform thread to the render thread.
//!
//! The perform thread writes one [`LiveFrame`] per step and never waits; the render thread reads the newest one when
//! it draws and never takes a lock. It is a seqlock over atomic words, like the stage's calibration mailbox, so it
//! needs no `unsafe`: the writer marks a write in progress (an odd sequence), stores the words and marks it done; a
//! reader copies the words and keeps the copy only if the sequence was even and unchanged across the copy. A frame
//! that was overwritten before anyone read it is simply gone, which is what "newest wins" means.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

use omuzaki_anim_api::{CHANNEL_CAPACITY, Channel, ChannelBuf, HUMANOID_COUNT, HumanoidPose, Tick};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_stage::perform::{PerformOutput, Tracking};

/// What one perform step hands the render thread: everything the stage's frame needs from the performer.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LiveFrame {
    /// The step that produced it.
    pub tick: Tick,
    /// Whether the face was tracked on that step.
    pub tracking: Tracking,
    /// The animation pose: what the rig reads.
    pub pose: HumanoidPose,
    /// The final channels: what the face painter reads.
    pub channels: ChannelBuf,
    /// When the newest tracker frame reached the perform thread, on the perform clock; `None` before the first. It
    /// carries across steps that bring no new frame, so the render side can say how long ago the picture's data
    /// arrived (the latency the health strip shows).
    pub arrived: Option<Seconds>,
}

impl LiveFrame {
    /// The frame a step produced, with `arrived` as the arrival of the newest tracker frame so far.
    pub fn from_output(out: &PerformOutput<'_>, arrived: Option<Seconds>) -> Self {
        Self { tick: out.tick, tracking: out.tracking, pose: *out.pose, channels: out.bus.values, arrived }
    }
}

// Layout: tick index (2 words), tick time (2), tick dt (1), tracking (1), hips offset (3), the rotations (4 per bone),
// then every channel slot.
const TICK_WORDS: usize = 5;
const TRACKING_WORDS: usize = 1;
const ARRIVED_WORDS: usize = 2;
const HIPS_WORDS: usize = 3;
const ROTATION_WORDS: usize = 4 * HUMANOID_COUNT;
/// Words in one encoded [`LiveFrame`].
const WORDS: usize = TICK_WORDS + TRACKING_WORDS + ARRIVED_WORDS + HIPS_WORDS + ROTATION_WORDS + CHANNEL_CAPACITY;

/// The record both ends share.
struct Shared {
    /// Odd while a write is in progress; `2 * n` after the `n`-th write.
    seq: AtomicU64,
    words: [AtomicU32; WORDS],
}

/// The perform thread's end. Only one exists, so writes never overlap.
pub struct FrameWriter {
    shared: Arc<Shared>,
}

/// A render-side end. Cheap to clone; each clone remembers which frame it saw last.
#[derive(Clone)]
pub struct FrameReader {
    shared: Arc<Shared>,
    /// The sequence of the newest frame this reader returned.
    seen: u64,
}

/// Makes the two ends of an empty mailbox. Cold: allocates the shared words once.
pub fn frame_mailbox() -> (FrameWriter, FrameReader) {
    let shared = Arc::new(Shared { seq: AtomicU64::new(0), words: std::array::from_fn(|_| AtomicU32::new(0)) });
    (FrameWriter { shared: Arc::clone(&shared) }, FrameReader { shared, seen: 0 })
}

impl FrameWriter {
    /// Replaces the frame in the mailbox. Never blocks, locks or allocates.
    pub fn publish(&mut self, frame: &LiveFrame) {
        let words = encode(frame);
        let shared = &*self.shared;
        let before = shared.seq.load(Ordering::Relaxed);
        shared.seq.store(before.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        for (slot, word) in shared.words.iter().zip(words) {
            slot.store(word, Ordering::Relaxed);
        }
        shared.seq.store(before.wrapping_add(2), Ordering::Release);
    }

    /// Frames published so far.
    pub fn published(&self) -> u64 {
        self.shared.seq.load(Ordering::Acquire) / 2
    }
}

impl FrameReader {
    /// The newest frame, if one arrived since the last call that returned one; `None` otherwise. Never locks or
    /// allocates; while a write is in progress it retries, which the writer bounds to one frame's copy.
    pub fn take_new(&mut self) -> Option<LiveFrame> {
        let (seq, frame) = self.read()?;
        if seq == self.seen {
            return None;
        }
        self.seen = seq;
        Some(frame)
    }

    /// The newest frame whether or not it was seen before; `None` before the first.
    pub fn latest(&self) -> Option<LiveFrame> {
        self.read().map(|(_, frame)| frame)
    }

    /// Frames published so far (the writer's count, read without waiting).
    pub fn published(&self) -> u64 {
        self.shared.seq.load(Ordering::Acquire) / 2
    }

    fn read(&self) -> Option<(u64, LiveFrame)> {
        let shared = &*self.shared;
        loop {
            let first = shared.seq.load(Ordering::Acquire);
            if first == 0 {
                return None;
            }
            if first.is_multiple_of(2) {
                let mut words = [0_u32; WORDS];
                for (word, slot) in words.iter_mut().zip(&shared.words) {
                    *word = slot.load(Ordering::Relaxed);
                }
                fence(Ordering::Acquire);
                if shared.seq.load(Ordering::Relaxed) == first {
                    return Some((first, decode(&words)));
                }
            }
            std::hint::spin_loop();
        }
    }
}

impl std::fmt::Debug for FrameReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameReader").field("published", &self.published()).finish_non_exhaustive()
    }
}

impl std::fmt::Debug for FrameWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FrameWriter").field("published", &self.published()).finish_non_exhaustive()
    }
}

/// Splits a `u64` into its low and high words.
#[allow(clippy::cast_possible_truncation)] // both halves are masked to 32 bits
fn halves(value: u64) -> [u32; 2] {
    [value as u32, (value >> 32) as u32]
}

fn join(lo: u32, hi: u32) -> u64 {
    u64::from(lo) | (u64::from(hi) << 32)
}

fn tracking_word(tracking: Tracking) -> u32 {
    match tracking {
        Tracking::NoFrames => 0,
        Tracking::Tracking => 1,
        Tracking::Lost => 2,
    }
}

fn tracking_of(word: u32) -> Tracking {
    match word {
        0 => Tracking::NoFrames,
        1 => Tracking::Tracking,
        _ => Tracking::Lost,
    }
}

fn encode(frame: &LiveFrame) -> [u32; WORDS] {
    let mut words = [0_u32; WORDS];
    let mut next = words.iter_mut();
    let mut put = |value: u32| {
        if let Some(word) = next.next() {
            *word = value;
        }
    };
    let [index_lo, index_hi] = halves(frame.tick.index);
    let [time_lo, time_hi] = halves(frame.tick.time.0.to_bits());
    // No arrival is a NaN, which no clock reads.
    let [arrived_lo, arrived_hi] = halves(frame.arrived.map_or(f64::NAN, |at| at.0).to_bits());
    for word in
        [index_lo, index_hi, time_lo, time_hi, frame.tick.dt.to_bits(), tracking_word(frame.tracking), arrived_lo, arrived_hi]
    {
        put(word);
    }
    for value in frame.pose.hips_offset.to_array() {
        put(value.to_bits());
    }
    for rotation in &frame.pose.rotations {
        for value in rotation.to_array() {
            put(value.to_bits());
        }
    }
    for value in frame.channels.as_slice() {
        put(value.to_bits());
    }
    words
}

fn decode(words: &[u32; WORDS]) -> LiveFrame {
    let mut next = words.iter().copied();
    let mut take = || next.next().unwrap_or(0);
    let index = join(take(), take());
    let time = f64::from_bits(join(take(), take()));
    let dt = f32::from_bits(take());
    let tracking = tracking_of(take());
    let arrived = Some(f64::from_bits(join(take(), take()))).filter(|at| !at.is_nan()).map(Seconds);
    let mut float = || f32::from_bits(take());
    let hips_offset = Vec3::new(float(), float(), float());
    let rotations = std::array::from_fn(|_| Quat::from_xyzw(float(), float(), float(), float()));
    let mut channels = ChannelBuf::NEUTRAL;
    for i in 0..CHANNEL_CAPACITY {
        let value = float();
        // Every slot was written from `as_slice`; one with no channel id is neutral (`0.0`) on both sides.
        if let Some(channel) = Channel::from_index(i) {
            channels.set(channel, value);
        }
    }
    LiveFrame {
        tick: Tick { index, time: Seconds(time), dt },
        tracking,
        pose: HumanoidPose { hips_offset, rotations },
        channels,
        arrived,
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::HumanoidBone;

    use super::*;

    fn frame(index: u64) -> LiveFrame {
        #[allow(clippy::cast_precision_loss)] // small test indices
        let t = index as f32;
        let mut pose = HumanoidPose::REST;
        pose.hips_offset = Vec3::new(0.01 * t, -0.5, 2.0);
        pose.set(HumanoidBone::Head, Quat::from_rotation_y(0.1 * t));
        let mut channels = ChannelBuf::NEUTRAL;
        channels.set(Channel::JAW_OPEN, 0.25);
        channels.set(Channel::HEAD_YAW, -0.125 * t);
        // Odd indices have an arrival and even ones none, so the round trip covers both.
        let arrived = (index % 2 == 1).then(|| Seconds(0.5 * t));
        LiveFrame { tick: Tick::at_rate(index, 60.0), tracking: Tracking::Lost, pose, channels, arrived }
    }

    #[test]
    fn a_frame_round_trips_bit_for_bit() {
        for index in [0, 1, 7, u64::from(u32::MAX) + 3] {
            let want = frame(index);
            assert_eq!(decode(&encode(&want)), want);
        }
    }

    #[test]
    fn the_reader_sees_nothing_before_the_first_frame_and_each_frame_once() {
        let (mut writer, mut reader) = frame_mailbox();
        assert_eq!(reader.take_new(), None);
        assert_eq!(reader.latest(), None);
        writer.publish(&frame(1));
        assert_eq!(reader.take_new(), Some(frame(1)));
        assert_eq!(reader.take_new(), None, "a frame is new only once");
        assert_eq!(reader.latest(), Some(frame(1)));
    }

    #[test]
    fn the_newest_frame_wins_and_older_ones_are_dropped() {
        let (mut writer, mut reader) = frame_mailbox();
        for i in 1..=5 {
            writer.publish(&frame(i));
        }
        assert_eq!(reader.take_new(), Some(frame(5)));
        assert_eq!((writer.published(), reader.published()), (5, 5));
    }

    #[test]
    fn a_clone_keeps_its_own_place() {
        let (mut writer, mut first) = frame_mailbox();
        let mut second = first.clone();
        writer.publish(&frame(2));
        assert!(first.take_new().is_some());
        assert_eq!(second.take_new(), Some(frame(2)), "the clone has not seen it yet");
    }

    #[test]
    fn a_reader_on_another_thread_never_sees_a_torn_frame() {
        let (mut writer, mut reader) = frame_mailbox();
        let watcher = std::thread::spawn(move || {
            let mut seen = 0;
            while seen < 2000 {
                if let Some(got) = reader.take_new() {
                    // Every field of a frame comes from the same index, so a mix of two writes would differ.
                    assert_eq!(got, frame(got.tick.index), "a torn frame");
                    seen += 1;
                }
                if reader.published() >= 20_000 {
                    break;
                }
            }
        });
        for i in 1..=20_000 {
            writer.publish(&frame(i));
        }
        watcher.join().expect("the watcher finishes");
    }
}
