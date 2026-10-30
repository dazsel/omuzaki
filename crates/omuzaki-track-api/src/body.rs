//! Body tracking (spec v3 §22 PF-06 hands, PF-07 body): the solved upper body a body tracker publishes, and the
//! newest-wins board that carries it to the stage.
//!
//! A [`BodyFrame`] is already solved into the character's terms, so any source fills the same value: a webcam body
//! tracker (the `MediaPipe` pose and hand landmarkers), VMC bones from a mocap app, or `OpenXR` trackers later. The stage
//! never sees landmarks or protocols:
//!
//! - **Bones** are [`HumanoidPose`] rotations in VRM normalised space (T space: identity is the T-pose, each rotation
//!   is parent-local), for the bones in [`BodyFrame::mask`] only. A webcam gives the upper chest, the arms and the
//!   hands' wrists; a mocap source (VMC) can give any bone, legs and fingers included. The head, neck, eyes and jaw are
//!   the face's and are never read from a body frame.
//! - **Fingers** are four anatomical, side-neutral angles per finger ([`FingerAngles`]), the conventions of the
//!   character kit's hand shapes, so the stage poses them through the rig's own hand rest.
//! - **Sides** are the **performer's own**, unmirrored: the performer's left hand drives the character's left hand.
//!   The stage mirrors the frame when the person asked for a mirrored performance (PF-27), the same switch the face
//!   uses.
//! - **Confidence** per arm and per hand, `0..=1`: how sure the source is. The stage fades a part out as it drops, so a
//!   hand leaving the frame eases back to the animated pose instead of freezing.
//!
//! The hand-off is the face mailbox's pattern: a triple buffer of atomic words, wait-free on both sides, nothing
//! queued. A [`BodyBoard`] is made once with the performer and lives as long as it: the stage keeps its one
//! [`BodyReader`], and each body tracker that starts leases the one [`BodyWriter`] for as long as it runs.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};

use omuzaki_anim_api::{BoneMask, HUMANOID_COUNT, HumanoidBone, HumanoidPose};
use omuzaki_core::glam::Quat;
use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A side of the performer's body.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    /// The performer's own left.
    Left,
    /// The performer's own right.
    Right,
}

impl Side {
    /// Both sides, left first: the index order of every per-side array here.
    pub const BOTH: [Self; 2] = [Self::Left, Self::Right];

    /// Its index in a per-side array.
    pub const fn index(self) -> usize {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }

    /// The other side.
    #[must_use]
    pub const fn other(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }
}

/// One finger's pose, radians, anatomical and side-neutral (the kit's hand-shape conventions, `omuzaki-anim`
/// `hands`): `spread` swings the whole finger in the plane of the palm (toward the thumb side is positive; for the thumb,
/// away from the index), `mcp`, `pip` and `dip` flex the knuckle, the middle joint and the tip toward the palm.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FingerAngles {
    /// Swing in the palm's plane.
    pub spread: f32,
    /// Knuckle flexion (for the thumb, the metacarpal's flexion across the palm).
    pub mcp: f32,
    /// Middle joint flexion.
    pub pip: f32,
    /// Tip joint flexion.
    pub dip: f32,
}

/// The five fingers of one hand, thumb first.
pub type Fingers = [FingerAngles; 5];

/// A hand shape held to the camera that presses a trigger key (PF-06 "gestures as triggers", PF-25), the way a face
/// gesture does: bind an expression to its [`key`](Self::key) with `trigger.bind`. Either hand shows it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandGesture {
    /// Every finger straight: a wave or a "stop".
    OpenPalm,
    /// Every finger curled.
    Fist,
    /// The index straight, the other three curled.
    Point,
    /// The index and middle straight, the ring and little curled.
    Peace,
    /// The thumb straight, the four fingers curled.
    ThumbsUp,
}

impl HandGesture {
    /// Every hand gesture, in a fixed order (bit `i` of [`HandGestures`] is `ALL[i]`).
    pub const ALL: [Self; 5] = [Self::OpenPalm, Self::Fist, Self::Point, Self::Peace, Self::ThumbsUp];

    /// The trigger key it presses.
    pub const fn key(self) -> &'static str {
        match self {
            Self::OpenPalm => "gesture:open_palm",
            Self::Fist => "gesture:fist",
            Self::Point => "gesture:point",
            Self::Peace => "gesture:peace",
            Self::ThumbsUp => "gesture:thumbs_up",
        }
    }

    const fn bit(self) -> u8 {
        1 << self as u8
    }
}

/// A set of [`HandGesture`]s: the ones showing now. `Copy`, one byte.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct HandGestures(u8);

impl HandGestures {
    /// None showing.
    pub const NONE: Self = Self(0);

    /// The set with `gesture` added.
    #[must_use]
    pub const fn with(self, gesture: HandGesture) -> Self {
        Self(self.0 | gesture.bit())
    }

    /// Whether `gesture` is in the set.
    pub const fn contains(self, gesture: HandGesture) -> bool {
        self.0 & gesture.bit() != 0
    }

    /// Whether the set is empty.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The gestures in the set, in [`HandGesture::ALL`] order.
    pub fn iter(self) -> impl Iterator<Item = HandGesture> {
        HandGesture::ALL.into_iter().filter(move |g| self.contains(*g))
    }
}

/// What a body tracker should track. Hands and arms each cost a model's run per frame, so each is a switch. A field
/// left out reads as its [`Default`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BodyConfig {
    /// The upper body: the chest's turn and tilt, the shoulders, elbows and wrists.
    pub arms: bool,
    /// The hands: wrist rotation and every finger.
    pub hands: bool,
    /// The heavier pose model: steadier elbows and wrists, about twice the cost.
    pub precise: bool,
}

impl Default for BodyConfig {
    /// Arms and hands, on the lighter pose model.
    fn default() -> Self {
        Self { arms: true, hands: true, precise: false }
    }
}

impl BodyConfig {
    /// Tracks nothing: body tracking off.
    pub const OFF: Self = Self { arms: false, hands: false, precise: false };

    /// Whether it tracks anything at all.
    pub fn any(&self) -> bool {
        self.arms || self.hands
    }
}

/// One solved body sample. `Copy`, fixed-size, allocation-free, like a face [`TrackFrame`](omuzaki_anim_api::TrackFrame).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BodyFrame {
    /// Sender sequence number.
    pub seq: u64,
    /// Capture time on the pipeline clock.
    pub time: Seconds,
    /// T-space rotations of the bones in `mask`; every other bone is ignored.
    pub pose: HumanoidPose,
    /// The bones this frame has an opinion on.
    pub mask: BoneMask,
    /// Each hand's fingers, by the performer's [`Side`]; `None` when that hand is not seen.
    pub fingers: [Option<Fingers>; 2],
    /// How sure the source is of each arm (shoulder to wrist), by [`Side`], `0..=1`.
    pub arms: [f32; 2],
    /// How sure the source is of each hand (wrist rotation and fingers), by [`Side`], `0..=1`.
    pub hands: [f32; 2],
    /// How sure the source is of the rest of the masked bones (hips, spine, shoulders, legs), `0..=1`. Finger bones in
    /// the mask go with their hand's confidence.
    pub body: f32,
    /// Whether the face's head pose is already relative to the chest (a mocap source sends the head as a local
    /// rotation), so the tracked chest carries the head. `false` for a webcam, whose head pose is in the camera's frame:
    /// the stage then turns the neck back by the chest's turn, so the head stays where the face put it.
    pub head_relative: bool,
}

impl BodyFrame {
    /// A frame that sees nothing: no bones, no fingers, zero confidence.
    pub const fn empty(seq: u64, time: Seconds) -> Self {
        Self {
            seq,
            time,
            pose: HumanoidPose::REST,
            mask: BoneMask::EMPTY,
            fingers: [None, None],
            arms: [0.0; 2],
            hands: [0.0; 2],
            body: 0.0,
            head_relative: false,
        }
    }

    /// Whether the frame sees any part of the body.
    pub fn sees_anything(&self) -> bool {
        self.arms.iter().chain(&self.hands).chain([&self.body]).any(|&c| c > 0.0)
    }
}

/// Where a body tracker stands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", tag = "state", content = "reason")]
pub enum BodyState {
    /// Not tracking: switched off, or this tracker has no body tracking.
    #[default]
    Off,
    /// Its models are loading (seconds, once per switch to another pose model).
    Loading,
    /// Tracking every camera frame it is given.
    Running,
    /// It could not start: the models are missing or refused. The reason is for the person.
    Failed(String),
}

/// How a body tracker is doing: what it was asked for, where it stands, what it sees and what it costs. The default is
/// a tracker that was never switched on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BodyStatus {
    /// What it was asked to track.
    pub config: BodyConfig,
    /// Where it stands.
    pub state: BodyState,
    /// How sure it is of each arm on the newest frame, by the performer's [`Side`] (left, right), `0..=1`.
    pub arms: [f32; 2],
    /// How sure it is of each hand on the newest frame, by the performer's [`Side`], `0..=1`.
    pub hands: [f32; 2],
    /// Frames tracked since it started.
    pub frames: u64,
    /// Time to track one frame (every model it ran, and the solve), median of the recent frames, milliseconds.
    pub ms_p50: f32,
    /// The same, 95th percentile.
    pub ms_p95: f32,
}

impl Default for BodyStatus {
    fn default() -> Self {
        Self {
            config: BodyConfig::OFF,
            state: BodyState::Off,
            arms: [0.0; 2],
            hands: [0.0; 2],
            frames: 0,
            ms_p50: 0.0,
            ms_p95: 0.0,
        }
    }
}

/// Where body frames come from, as the stage polls them: the newest frame since the last poll, or `None`.
pub trait BodySource: Send {
    /// The newest frame published since the last call. Hot path: never blocks, locks or allocates.
    fn poll(&mut self) -> Option<BodyFrame>;
}

/// Set in the shared byte while the middle slot holds a frame the reader has not taken.
const FRESH: u8 = 0b100;
/// The slot-index bits of the shared byte.
const INDEX: u8 = 0b011;

// Word layout of one frame.
const SEQ: usize = 0; // 2 words
const TIME: usize = 2; // 2 words, f64 bits
const MASK: usize = 4; // 2 words, one bit per humanoid bone
const FLAGS: usize = 6; // bit 0: left fingers present, bit 1: right fingers present, bit 2: head relative
const CONFIDENCE: usize = 7; // arms left, right, hands left, right, body
const ROTATIONS: usize = CONFIDENCE + 5; // 4 words per humanoid bone
const FINGERS: usize = ROTATIONS + 4 * HUMANOID_COUNT; // 2 hands x 5 fingers x 4 angles
const WORDS: usize = FINGERS + 2 * 5 * 4;

struct Shared {
    slots: [[AtomicU32; WORDS]; 3],
    middle: AtomicU8,
    /// The slot the next writer fills: the last writer's, so a new lease never takes the reader's or the middle one.
    back: AtomicU8,
    /// Whether a [`BodyWriter`] is out.
    writing: AtomicBool,
    /// The hand gestures showing now ([`HandGestures`]' bits), read by whoever presses their keys.
    gestures: AtomicU8,
}

/// The newest-wins hand-off from body trackers to the stage. Made once; the reader stays with the stage and each
/// running tracker leases the writer. Cold to make (allocates once), wait-free to use.
pub struct BodyBoard {
    shared: Arc<Shared>,
}

impl std::fmt::Debug for BodyBoard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyBoard").field("writing", &self.shared.writing.load(Ordering::Relaxed)).finish()
    }
}

impl BodyBoard {
    /// A board and its one reader.
    pub fn new() -> (Arc<Self>, BodyReader) {
        let shared = Arc::new(Shared {
            slots: std::array::from_fn(|_| std::array::from_fn(|_| AtomicU32::new(0))),
            middle: AtomicU8::new(1),
            back: AtomicU8::new(2),
            writing: AtomicBool::new(false),
            gestures: AtomicU8::new(0),
        });
        (Arc::new(Self { shared: Arc::clone(&shared) }), BodyReader { shared, front: 0 })
    }

    /// The writer, or `None` while another one is out: one body tracker publishes at a time. Dropping the writer
    /// returns it to the board.
    pub fn writer(&self) -> Option<BodyWriter> {
        let taken = self.shared.writing.swap(true, Ordering::AcqRel);
        (!taken)
            .then(|| BodyWriter { shared: Arc::clone(&self.shared), back: self.shared.back.load(Ordering::Acquire) })
    }

    /// Whether a writer is out (a body tracker is running).
    pub fn is_writing(&self) -> bool {
        self.shared.writing.load(Ordering::Acquire)
    }

    /// The hand gestures showing now; none while no tracker is running. Any number of readers; wait-free.
    pub fn gestures(&self) -> HandGestures {
        HandGestures(self.shared.gestures.load(Ordering::Acquire))
    }
}

/// The producing end, leased from a [`BodyBoard`]. `Send`, not `Clone`.
pub struct BodyWriter {
    shared: Arc<Shared>,
    back: u8,
}

impl std::fmt::Debug for BodyWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyWriter").finish_non_exhaustive()
    }
}

impl BodyWriter {
    /// Makes `frame` the newest; an older frame the reader hasn't taken is dropped. Hot path: wait-free.
    pub fn publish(&mut self, frame: &BodyFrame) {
        let words = encode(frame);
        for (cell, word) in self.shared.slots[usize::from(self.back)].iter().zip(words) {
            cell.store(word, Ordering::Relaxed);
        }
        let previous = self.shared.middle.swap(self.back | FRESH, Ordering::AcqRel);
        self.back = previous & INDEX;
    }

    /// Sets the hand gestures showing now (the tracker decides them, with its own hold and release times).
    pub fn show(&mut self, gestures: HandGestures) {
        self.shared.gestures.store(gestures.0, Ordering::Release);
    }
}

impl Drop for BodyWriter {
    fn drop(&mut self) {
        self.shared.back.store(self.back, Ordering::Release);
        // A tracker that stops lets go of its gestures.
        self.shared.gestures.store(0, Ordering::Release);
        self.shared.writing.store(false, Ordering::Release);
    }
}

/// The consuming end: the stage's [`BodySource`].
pub struct BodyReader {
    shared: Arc<Shared>,
    front: u8,
}

impl std::fmt::Debug for BodyReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BodyReader").finish_non_exhaustive()
    }
}

impl BodySource for BodyReader {
    fn poll(&mut self) -> Option<BodyFrame> {
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
fn encode(f: &BodyFrame) -> [u32; WORDS] {
    // Exhaustive pattern: a field added to the frame fails to compile here until it is encoded.
    let BodyFrame { seq, time, pose, mask, fingers, arms, hands, body, head_relative } = *f;
    let mut w = [0; WORDS];
    let split = |w: &mut [u32], v: u64| {
        w[0] = v as u32;
        w[1] = (v >> 32) as u32;
    };
    split(&mut w[SEQ..], seq);
    split(&mut w[TIME..], time.0.to_bits());
    let bits = HumanoidBone::ALL.iter().filter(|b| mask.contains(**b)).fold(0_u64, |acc, b| acc | 1 << b.index());
    split(&mut w[MASK..], bits);
    for (i, confidence) in arms.iter().chain(&hands).chain([&body]).enumerate() {
        w[CONFIDENCE + i] = confidence.to_bits();
    }
    for (bone, chunk) in pose.rotations.iter().zip(w[ROTATIONS..FINGERS].as_chunks_mut::<4>().0) {
        *chunk = bone.to_array().map(f32::to_bits);
    }
    let mut flags = u32::from(head_relative) << 2;
    for (side, hand) in fingers.iter().enumerate() {
        let Some(hand) = hand else { continue };
        flags |= 1 << side;
        for (finger, angles) in hand.iter().enumerate() {
            let at = FINGERS + (side * 5 + finger) * 4;
            w[at..at + 4].copy_from_slice(&[angles.spread, angles.mcp, angles.pip, angles.dip].map(f32::to_bits));
        }
    }
    w[FLAGS] = flags;
    w
}

fn decode(w: &[u32; WORDS]) -> BodyFrame {
    let join = |i: usize| u64::from(w[i]) | (u64::from(w[i + 1]) << 32);
    let float = |i: usize| f32::from_bits(w[i]);
    let mut frame = BodyFrame::empty(join(SEQ), Seconds(f64::from_bits(join(TIME))));
    let bits = join(MASK);
    frame.mask =
        HumanoidBone::ALL.iter().filter(|b| bits >> b.index() & 1 == 1).fold(BoneMask::EMPTY, |m, b| m.with(*b));
    frame.arms = [float(CONFIDENCE), float(CONFIDENCE + 1)];
    frame.hands = [float(CONFIDENCE + 2), float(CONFIDENCE + 3)];
    frame.body = float(CONFIDENCE + 4);
    frame.head_relative = w[FLAGS] >> 2 & 1 == 1;
    for (bone, chunk) in frame.pose.rotations.iter_mut().zip(w[ROTATIONS..FINGERS].as_chunks::<4>().0) {
        *bone = Quat::from_array(chunk.map(f32::from_bits));
    }
    for side in 0..2 {
        if w[FLAGS] >> side & 1 == 0 {
            continue;
        }
        frame.fingers[side] = Some(std::array::from_fn(|finger| {
            let at = FINGERS + (side * 5 + finger) * 4;
            FingerAngles { spread: float(at), mcp: float(at + 1), pip: float(at + 2), dip: float(at + 3) }
        }));
    }
    frame
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // values copied through the board are compared exactly
mod tests {
    use super::*;

    fn frame(seq: u64) -> BodyFrame {
        #[allow(clippy::cast_precision_loss)] // small test counters
        let mut f = BodyFrame::empty(seq, Seconds(seq as f64 / 30.0));
        f.pose.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(-1.1));
        f.pose.set(HumanoidBone::RightHand, Quat::from_rotation_y(0.4));
        f.mask = BoneMask::of(&[HumanoidBone::LeftUpperArm, HumanoidBone::RightHand]);
        f.arms = [0.9, 0.0];
        f.hands = [0.0, 0.75];
        f.body = 0.5;
        f.head_relative = seq % 2 == 1;
        let mut fingers = [FingerAngles::default(); 5];
        fingers[1] = FingerAngles { spread: 0.1, mcp: 1.2, pip: 1.4, dip: 0.9 };
        f.fingers[1] = Some(fingers);
        f
    }

    #[test]
    fn a_frame_round_trips_exactly() {
        let (board, mut reader) = BodyBoard::new();
        let mut writer = board.writer().expect("the writer is free");
        assert_eq!(reader.poll(), None, "nothing published yet");
        writer.publish(&frame(3));
        assert_eq!(reader.poll(), Some(frame(3)));
        assert_eq!(reader.poll(), None, "a frame is taken once");
        let empty = BodyFrame::empty(9, Seconds(1.5));
        writer.publish(&empty);
        assert_eq!(reader.poll(), Some(empty));
    }

    #[test]
    fn the_newest_frame_wins() {
        let (board, mut reader) = BodyBoard::new();
        let mut writer = board.writer().expect("the writer is free");
        for seq in 0..5 {
            writer.publish(&frame(seq));
        }
        assert_eq!(reader.poll().map(|f| f.seq), Some(4));
    }

    #[test]
    fn one_writer_at_a_time_and_it_comes_back_on_drop() {
        let (board, _reader) = BodyBoard::new();
        let first = board.writer().expect("free");
        assert!(board.is_writing() && board.writer().is_none());
        drop(first);
        assert!(!board.is_writing());
        assert!(board.writer().is_some());
    }

    #[test]
    fn a_new_lease_never_writes_the_slot_the_reader_holds() {
        let (board, mut reader) = BodyBoard::new();
        // Publish and read so the slots move round, then hand the writer to a second tracker.
        for round in 0..6 {
            let mut writer = board.writer().expect("free");
            writer.publish(&frame(round * 2));
            assert_eq!(reader.poll().map(|f| f.seq), Some(round * 2));
            writer.publish(&frame(round * 2 + 1));
            drop(writer);
            // The reader's own slot still holds the frame it took, and the fresh one arrives whole.
            assert_eq!(reader.poll(), Some(frame(round * 2 + 1)));
        }
    }

    #[test]
    fn gestures_show_while_the_tracker_runs_and_go_when_it_stops() {
        let (board, _reader) = BodyBoard::new();
        let mut writer = board.writer().expect("free");
        let shown = HandGestures::NONE.with(HandGesture::Peace).with(HandGesture::Fist);
        writer.show(shown);
        assert_eq!(board.gestures().iter().collect::<Vec<_>>(), [HandGesture::Fist, HandGesture::Peace]);
        drop(writer);
        assert!(board.gestures().is_empty());
    }

    #[test]
    fn frames_cross_threads_whole() {
        let (board, mut reader) = BodyBoard::new();
        let mut writer = board.writer().expect("free");
        let producer = std::thread::spawn(move || {
            for seq in 0..2000 {
                writer.publish(&frame(seq));
            }
        });
        let mut last = None;
        while !producer.is_finished() {
            if let Some(f) = reader.poll() {
                // Never torn: every field matches the frame of that seq.
                assert_eq!(f, frame(f.seq));
                assert!(last.is_none_or(|l| f.seq > l), "seq goes forward");
                last = Some(f.seq);
            }
        }
        producer.join().expect("the producer finished");
    }

    #[test]
    fn config_defaults_track_arms_and_hands() {
        let c = BodyConfig::default();
        assert!(c.any() && c.arms && c.hands && !c.precise);
        assert!(!BodyConfig { arms: false, hands: false, precise: true }.any());
    }
}
