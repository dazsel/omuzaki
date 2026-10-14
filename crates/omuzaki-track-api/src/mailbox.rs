//! The newest-wins hand-off from a tracker thread to the stage thread (spec §8: "lock-free hand-off, newest frame
//! wins, never queue").
//!
//! A triple buffer: three slots, one owned by the writer, one by the reader, and one in the middle, plus an atomic
//! byte naming the middle slot and whether it holds a frame the reader hasn't taken. Publishing fills the writer's
//! slot and swaps it into the middle; polling swaps the middle out if it is fresh. Both sides are wait-free (one
//! atomic swap each), never allocate after [`mailbox`] returns, and a slot is only ever touched by its owner, so a
//! frame is never torn. An unread frame is simply overwritten by the next one: nothing queues.
//!
//! Slots are arrays of `AtomicU32` words, so the crate needs no `unsafe` (the pattern of `omuzaki-audio-api`'s ring);
//! the swap's acquire/release ordering is what publishes the words, and their own accesses are relaxed.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};

use omuzaki_anim_api::{
    ARKIT_COUNT, ChannelMask, DeviceProfile, EyeAngles, FrameSource, HeadPose, TrackFrame, TrackStatus,
};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;

/// Set in the shared byte while the middle slot holds a frame the reader has not taken.
const FRESH: u8 = 0b100;
/// The slot-index bits of the shared byte.
const INDEX: u8 = 0b011;

// Word layout of one frame.
const SEQ: usize = 0; // 2 words
const TIME: usize = 2; // 2 words, f64 bits
const FLAGS: usize = 4; // profile in bits 0-7, then LOST, HEAD, POSITION, EYES
const PRESENT: usize = 5; // 4 words, u128
const BLEND: usize = 9; // ARKIT_COUNT words
const HEAD: usize = BLEND + ARKIT_COUNT; // yaw, pitch, roll, then position x, y, z
const EYES: usize = HEAD + 6; // 4 words
const WORDS: usize = EYES + 4;

const LOST: u32 = 1 << 8;
const HAS_HEAD: u32 = 1 << 9;
const HAS_POSITION: u32 = 1 << 10;
const HAS_EYES: u32 = 1 << 11;

struct Shared {
    slots: [[AtomicU32; WORDS]; 3],
    middle: AtomicU8,
}

/// Creates a connected writer (for the tracker thread) and reader (for the stage thread). Cold: allocates once.
pub fn mailbox() -> (MailboxWriter, MailboxReader) {
    let shared = Arc::new(Shared {
        slots: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU32::new(0))),
        middle: AtomicU8::new(1),
    });
    (MailboxWriter { shared: Arc::clone(&shared), back: 2 }, MailboxReader { shared, front: 0 })
}

/// The producing end. `Send`, not `Clone`: exactly one producer.
pub struct MailboxWriter {
    shared: Arc<Shared>,
    back: u8,
}

impl MailboxWriter {
    /// Makes `frame` the newest; an older frame the reader hasn't taken is dropped. Hot path: wait-free, no
    /// allocation.
    pub fn publish(&mut self, frame: &TrackFrame) {
        let words = encode(frame);
        for (cell, word) in self.shared.slots[usize::from(self.back)].iter().zip(words) {
            cell.store(word, Ordering::Relaxed);
        }
        let previous = self.shared.middle.swap(self.back | FRESH, Ordering::AcqRel);
        self.back = previous & INDEX;
    }
}

/// The consuming end, the pipeline's [`FrameSource`]. Exactly one consumer.
pub struct MailboxReader {
    shared: Arc<Shared>,
    front: u8,
}

impl FrameSource for MailboxReader {
    /// The newest frame published since the last poll, or `None`. `now` is not needed: frames carry their own
    /// capture time. Hot path: wait-free, no allocation, never blocks.
    fn poll(&mut self, _now: Seconds) -> Option<TrackFrame> {
        if self.shared.middle.load(Ordering::Relaxed) & FRESH == 0 {
            return None;
        }
        let previous = self.shared.middle.swap(self.front, Ordering::AcqRel);
        self.front = previous & INDEX;
        let mut words = [0; WORDS];
        for (word, cell) in words.iter_mut().zip(&self.shared.slots[usize::from(self.front)]) {
            *word = cell.load(Ordering::Relaxed);
        }
        Some(decode(&words))
    }
}

#[allow(clippy::cast_possible_truncation)] // splitting integers into their 32-bit words
fn encode(f: &TrackFrame) -> [u32; WORDS] {
    let mut w = [0; WORDS];
    let split = |w: &mut [u32], v: u64| {
        w[0] = v as u32;
        w[1] = (v >> 32) as u32;
    };
    split(&mut w[SEQ..], f.seq);
    split(&mut w[TIME..], f.time.0.to_bits());
    let bits = f.present.bits();
    for (i, word) in w[PRESENT..PRESENT + 4].iter_mut().enumerate() {
        *word = (bits >> (32 * i)) as u32;
    }
    let mut flags = match f.profile {
        DeviceProfile::Arkit => 0,
        // `DeviceProfile` is non-exhaustive; `every_profile_round_trips` fails when a variant is added unmapped.
        _ => 1,
    };
    if f.status == TrackStatus::Lost {
        flags |= LOST;
    }
    for (word, value) in w[BLEND..HEAD].iter_mut().zip(f.blend) {
        *word = value.to_bits();
    }
    if let Some(head) = f.head {
        flags |= HAS_HEAD;
        w[HEAD] = head.yaw.to_bits();
        w[HEAD + 1] = head.pitch.to_bits();
        w[HEAD + 2] = head.roll.to_bits();
        if let Some(p) = head.position {
            flags |= HAS_POSITION;
            w[HEAD + 3] = p.x.to_bits();
            w[HEAD + 4] = p.y.to_bits();
            w[HEAD + 5] = p.z.to_bits();
        }
    }
    if let Some(e) = f.eyes {
        flags |= HAS_EYES;
        w[EYES] = e.left_yaw.to_bits();
        w[EYES + 1] = e.left_pitch.to_bits();
        w[EYES + 2] = e.right_yaw.to_bits();
        w[EYES + 3] = e.right_pitch.to_bits();
    }
    w[FLAGS] = flags;
    w
}

fn decode(w: &[u32; WORDS]) -> TrackFrame {
    let join = |i: usize| u64::from(w[i]) | (u64::from(w[i + 1]) << 32);
    let float = |i: usize| f32::from_bits(w[i]);
    let flags = w[FLAGS];
    let profile = match flags & 0xff {
        0 => DeviceProfile::Arkit,
        _ => DeviceProfile::Webcam,
    };
    let mut frame = TrackFrame::empty(join(SEQ), Seconds(f64::from_bits(join(TIME))), profile);
    if flags & LOST != 0 {
        frame.status = TrackStatus::Lost;
    }
    let bits = (0..4).fold(0_u128, |acc, i| acc | (u128::from(w[PRESENT + i]) << (32 * i)));
    frame.present = ChannelMask::from_bits_truncate(bits);
    for (value, word) in frame.blend.iter_mut().zip(&w[BLEND..HEAD]) {
        *value = f32::from_bits(*word);
    }
    if flags & HAS_HEAD != 0 {
        let position =
            (flags & HAS_POSITION != 0).then(|| Vec3::new(float(HEAD + 3), float(HEAD + 4), float(HEAD + 5)));
        frame.head = Some(HeadPose { yaw: float(HEAD), pitch: float(HEAD + 1), roll: float(HEAD + 2), position });
    }
    if flags & HAS_EYES != 0 {
        frame.eyes = Some(EyeAngles {
            left_yaw: float(EYES),
            left_pitch: float(EYES + 1),
            right_yaw: float(EYES + 2),
            right_pitch: float(EYES + 3),
        });
    }
    frame
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::Channel;

    use super::*;

    fn frame(seq: u64) -> TrackFrame {
        #[allow(clippy::cast_precision_loss)] // small test counters
        let mut f = TrackFrame::empty(seq, Seconds(seq as f64 / 30.0), DeviceProfile::Webcam);
        f.set(Channel::JAW_OPEN, 0.25);
        f
    }

    #[test]
    fn nothing_published_polls_none_and_each_frame_is_taken_once() {
        let (mut tx, mut rx) = mailbox();
        assert_eq!(rx.poll(Seconds(0.0)), None);
        tx.publish(&frame(1));
        assert_eq!(rx.poll(Seconds(0.0)), Some(frame(1)));
        assert_eq!(rx.poll(Seconds(0.0)), None);
    }

    #[test]
    fn the_newest_frame_wins() {
        let (mut tx, mut rx) = mailbox();
        for seq in 1..=5 {
            tx.publish(&frame(seq));
        }
        assert_eq!(rx.poll(Seconds(0.0)), Some(frame(5)));
        assert_eq!(rx.poll(Seconds(0.0)), None);
        tx.publish(&frame(6));
        assert_eq!(rx.poll(Seconds(0.0)).map(|f| f.seq), Some(6));
    }

    #[test]
    fn every_field_round_trips() {
        let mut f = TrackFrame::lost(u64::MAX - 3, Seconds(-12.5e-7), DeviceProfile::Arkit);
        for (i, value) in f.blend.iter_mut().enumerate() {
            #[allow(clippy::cast_precision_loss)]
            let v = i as f32 * -0.37;
            *value = v;
        }
        f.present = ChannelMask::ARKIT;
        f.head = Some(HeadPose { yaw: 0.1, pitch: -0.2, roll: 0.3, position: Some(Vec3::new(0.01, -0.02, 0.6)) });
        f.eyes = Some(EyeAngles { left_yaw: 0.05, left_pitch: -0.04, right_yaw: 0.03, right_pitch: -0.02 });
        assert_eq!(decode(&encode(&f)), f);
        f.head = Some(HeadPose { position: None, ..HeadPose::default() });
        f.eyes = None;
        assert_eq!(decode(&encode(&f)), f);
    }

    #[test]
    fn every_profile_round_trips() {
        // The schema lists every variant, so a new one fails here until the encoding maps it.
        let schema = serde_json::to_value(schemars::schema_for!(DeviceProfile)).unwrap();
        let variants = schema["oneOf"].as_array().unwrap();
        assert!(variants.len() >= 2);
        for variant in variants {
            let profile: DeviceProfile = serde_json::from_value(variant["const"].clone()).unwrap();
            let f = TrackFrame::empty(1, Seconds(0.0), profile);
            assert_eq!(decode(&encode(&f)).profile, profile, "{variant}");
        }
    }
}
