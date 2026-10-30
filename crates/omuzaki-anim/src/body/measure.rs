//! How alive a body looks, as numbers (master checklist §8: "no frozen 0.5 s windows"; #2349: stroke-to-onset
//! offsets): a talking take is synthesised from a seed (head motion, a voice's loudness and stressed syllables, smiles
//! and frowns), played through an animator, and the body it produces is measured.
//!
//! - **Frozen windows**: 0.5 s windows in which no upper-body bone (spine, chest, shoulders, arms) turns faster than
//!   [`FROZEN_DEG_S`]. A face-only body has nearly nothing else; the target is none.
//! - **Motion** by region: the mean turning speed of the torso, the arms and the fingers, degrees a second.
//! - **Pops**: the fastest turn of any body bone, degrees a second. A jump in the pose shows up here first.
//! - **Strokes**: how many gesture strokes landed a minute, and each stroke's offset from the nearest stressed
//!   syllable: the median and 90th percentile of the absolute offset, the share within 100 ms, and how many were
//!   timed from the rhythm rather than reacting.
//!
//! The take is the same for every style with the same seed, so two styles compare like for like.

use omuzaki_anim_api::{Animator, BodyStyle, Bus, Channel, Cues, HumanoidBone as B, HumanoidPose, Tick};
use omuzaki_core::glam::Quat;
use omuzaki_core::rng::Rng;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::LivingBody;

/// Frame rate of a measured take.
pub const TAKE_FPS: f32 = 60.0;
/// A bone turning slower than this (degrees a second) is still.
pub const FROZEN_DEG_S: f32 = 1.5;
/// The frozen window's length, seconds.
pub const FROZEN_WINDOW_S: f32 = 0.5;

/// The upper-body bones the frozen windows watch.
const UPPER: [B; 9] = [
    B::Spine,
    B::Chest,
    B::UpperChest,
    B::LeftShoulder,
    B::RightShoulder,
    B::LeftUpperArm,
    B::RightUpperArm,
    B::LeftLowerArm,
    B::RightLowerArm,
];
const TORSO: [B; 4] = [B::Hips, B::Spine, B::Chest, B::UpperChest];
const ARMS: [B; 8] = [
    B::LeftShoulder,
    B::RightShoulder,
    B::LeftUpperArm,
    B::RightUpperArm,
    B::LeftLowerArm,
    B::RightLowerArm,
    B::LeftHand,
    B::RightHand,
];

/// One frame of a synthetic take: the bus and the voice cues.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TakeFrame {
    /// The channels the face pipeline would hand the animator.
    pub bus: Bus,
    /// The voice.
    pub cues: Cues,
}

/// A synthetic talking take: talk and pauses, a rhythm of stressed syllables with a natural spread, head motion with
/// nods on the stresses, breath, and the odd smile or frown. Deterministic in `seed`.
#[derive(Clone, Debug)]
pub struct Take {
    /// The frames at [`TAKE_FPS`].
    pub frames: Vec<TakeFrame>,
    /// When each stressed syllable started, seconds.
    pub onsets: Vec<f32>,
}

impl Take {
    /// A take `seconds` long that talks about `talk_share` of the time.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // bounded counts
    pub fn synthesize(seconds: f32, talk_share: f32, seed: u64) -> Self {
        let mut rng = Rng::new(seed ^ 0x7461_6b65);
        let n = (seconds.max(0.0) * TAKE_FPS).round() as usize;
        let share = talk_share.clamp(0.0, 1.0);
        // Talk and pause spans.
        let mut talking = vec![false; n];
        let mut onsets = Vec::new();
        let mut t = rng.range_f32(0.3, 1.2);
        while t < seconds {
            let talk = rng.range_f32(2.0, 6.0) * share.max(0.05) * 1.4;
            let pause = rng.range_f32(0.7, 2.6) * (1.0 - share).max(0.05) * 2.2;
            let end = (t + talk).min(seconds);
            let mut s = t + rng.range_f32(0.05, 0.2);
            // A speaker's stresses: about 0.45 s apart with a fifth of spread.
            let beat = rng.range_f32(0.38, 0.55);
            while s < end - 0.1 {
                onsets.push(s);
                s += beat * rng.range_f32(0.8, 1.2);
            }
            let (from, to) = ((t * TAKE_FPS) as usize, ((end * TAKE_FPS) as usize).min(n));
            talking.iter_mut().take(to).skip(from).for_each(|f| *f = true);
            t = end + pause;
        }
        let mut faces = Rng::new(seed ^ 0x6661_6365);
        let mut frames = Vec::with_capacity(n);
        let mut next_onset = 0;
        let (mut smile, mut frown, mut mood_until) = (0.0_f32, 0.0_f32, 0.0_f32);
        let (mut smile_to, mut frown_to) = (0.0_f32, 0.0_f32);
        let mut nod = 0.0_f32;
        for (i, &talks) in talking.iter().enumerate() {
            let time = i as f32 / TAKE_FPS;
            let mut cues = Cues::default();
            if next_onset < onsets.len() && onsets[next_onset] <= time {
                cues.onset = true;
                nod = 1.0;
                next_onset += 1;
            }
            if talks {
                let syllable = (time * 4.7 * std::f32::consts::TAU).sin().abs();
                cues.loudness = 0.25 + 0.35 * syllable + if cues.onset { 0.25 } else { 0.0 };
            }
            if time >= mood_until {
                let r = faces.next_f32();
                (smile_to, frown_to) = if r < 0.3 { (0.7, 0.0) } else if r < 0.42 { (0.0, 0.6) } else { (0.1, 0.0) };
                mood_until = time + faces.range_f32(2.0, 5.0);
            }
            smile += (smile_to - smile) * 0.05;
            frown += (frown_to - frown) * 0.05;
            nod *= 0.9;
            let mut bus = Bus::default();
            let v = &mut bus.values;
            let s = |f: f32, p: f32| (time * f * std::f32::consts::TAU + p).sin();
            v.set(Channel::HEAD_YAW, 0.18 * s(0.11, 0.0) + 0.07 * s(0.37, 1.3));
            v.set(Channel::HEAD_PITCH, 0.06 * s(0.17, 2.0) + 0.04 * nod);
            v.set(Channel::HEAD_ROLL, 0.05 * s(0.13, 0.7));
            v.set(Channel::BREATH, 0.5 + 0.5 * s(0.25, 0.0));
            v.set(Channel::MOUTH_SMILE_LEFT, smile);
            v.set(Channel::MOUTH_SMILE_RIGHT, smile);
            v.set(Channel::MOUTH_FROWN_LEFT, frown);
            v.set(Channel::MOUTH_FROWN_RIGHT, frown);
            v.set(Channel::BROW_INNER_UP, frown * 0.6);
            v.set(Channel::JAW_OPEN, if talks { 0.15 + 0.3 * cues.loudness } else { 0.0 });
            frames.push(TakeFrame { bus, cues });
        }
        Self { frames, onsets }
    }

    /// Plays the take through `animator` from `base`, calling `each` with every frame's index and pose.
    #[allow(clippy::cast_possible_truncation)] // a frame index
    pub fn play(&self, animator: &mut dyn Animator, base: &HumanoidPose, mut each: impl FnMut(usize, &HumanoidPose)) {
        for (i, frame) in self.frames.iter().enumerate() {
            animator.hear(&frame.cues);
            let mut pose = *base;
            animator.animate(Tick::at_rate(i as u64, TAKE_FPS), &frame.bus, &[], &mut pose);
            each(i, &pose);
        }
    }
}

/// The measures of one body over a take.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BodyMotion {
    /// Seconds measured.
    pub seconds: f32,
    /// Non-overlapping 0.5 s windows with the whole upper body still.
    pub frozen_windows: u32,
    /// Share of all windows that were frozen.
    pub frozen_share: f32,
    /// Mean turning speed of the hips, spine and chest, degrees a second.
    pub torso_deg_s: f32,
    /// Mean turning speed of the shoulders, arms and wrists, degrees a second.
    pub arms_deg_s: f32,
    /// Mean turning speed of the finger bones, degrees a second.
    pub fingers_deg_s: f32,
    /// The fastest turn of any bone, degrees a second: a pop shows here.
    pub peak_deg_s: f32,
    /// The bone that turned fastest.
    pub peak_bone: String,
    /// Gesture phrases started a minute.
    pub phrases_per_min: f32,
    /// Strokes landed a minute.
    pub strokes_per_min: f32,
    /// Median of each stroke's absolute offset from the nearest stressed syllable, milliseconds.
    pub stroke_offset_p50_ms: f32,
    /// 90th percentile of the same, milliseconds.
    pub stroke_offset_p90_ms: f32,
    /// Share of strokes within 100 ms of a stressed syllable.
    pub strokes_within_100ms: f32,
    /// Share of strokes started from the rhythm before their stress.
    pub strokes_predicted: f32,
    /// Digest of every pose, for determinism checks.
    pub checksum: u64,
}

/// Angle between two rotations, degrees.
fn angle_deg(a: Quat, b: Quat) -> f32 {
    a.angle_between(b).to_degrees()
}

/// Measures `style` on `take`, from the base pose `base`, with the body's choices seeded by `seed`.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // bounded counts
pub fn measure(style: &BodyStyle, take: &Take, base: &HumanoidPose, seed: u64) -> BodyMotion {
    let mut body = LivingBody::new(*style, seed);
    let n = take.frames.len();
    let mut prev: Option<HumanoidPose> = None;
    let window = (FROZEN_WINDOW_S * TAKE_FPS).round() as usize;
    let (mut frozen, mut windows, mut moving_in_window, mut in_window) = (0u32, 0u32, false, 0usize);
    let (mut torso, mut arms, mut fingers, mut peak) = (0.0_f64, 0.0_f64, 0.0_f64, 0.0_f32);
    let mut peak_bone = B::Hips;
    let mut checksum = 0xcbf2_9ce4_8422_2325_u64;
    let mut landings: Vec<(f32, bool)> = Vec::new();
    let mut phrases = 0u32;
    let finger_bones: Vec<B> = B::ALL
        .into_iter()
        .filter(|b| ["Thumb", "Index", "Middle", "Ring", "Little"].iter().any(|f| b.name().contains(f)))
        .collect();
    for (i, frame) in take.frames.iter().enumerate() {
        body.hear(&frame.cues);
        let mut pose = *base;
        body.animate(Tick::at_rate(i as u64, TAKE_FPS), &frame.bus, &[], &mut pose);
        let log = body.stroke_log();
        let time = i as f32 / TAKE_FPS;
        if log.landed {
            landings.push((time, log.predicted));
        }
        phrases += u32::from(log.phrase);
        checksum = checksum.rotate_left(5) ^ pose.checksum();
        if let Some(prev) = prev {
            let turn = |b: B| angle_deg(prev.get(b), pose.get(b)) * TAKE_FPS;
            torso += f64::from(TORSO.iter().map(|b| turn(*b)).sum::<f32>() / TORSO.len() as f32);
            arms += f64::from(ARMS.iter().map(|b| turn(*b)).sum::<f32>() / ARMS.len() as f32);
            fingers += f64::from(finger_bones.iter().map(|b| turn(*b)).sum::<f32>() / finger_bones.len() as f32);
            for b in B::ALL {
                let v = turn(b);
                if v > peak {
                    peak = v;
                    peak_bone = b;
                }
            }
            moving_in_window |= UPPER.iter().any(|b| turn(*b) >= FROZEN_DEG_S);
        }
        prev = Some(pose);
        in_window += 1;
        if in_window == window {
            windows += 1;
            frozen += u32::from(!moving_in_window);
            moving_in_window = false;
            in_window = 0;
        }
    }
    let seconds = n as f32 / TAKE_FPS;
    let steps = n.saturating_sub(1).max(1) as f64;
    let mut offsets: Vec<f32> = landings
        .iter()
        .map(|(t, _)| {
            take.onsets.iter().map(|o| (t - o).abs()).fold(f32::INFINITY, f32::min) * 1000.0
        })
        .collect();
    offsets.sort_by(f32::total_cmp);
    let pct = |q: f32| {
        if offsets.is_empty() {
            0.0
        } else {
            offsets[((offsets.len() - 1) as f32 * q).round() as usize]
        }
    };
    let strokes = landings.len() as f32;
    let per_min = |count: f32| if seconds > 0.0 { count * 60.0 / seconds } else { 0.0 };
    BodyMotion {
        seconds,
        frozen_windows: frozen,
        frozen_share: if windows == 0 { 0.0 } else { frozen as f32 / windows as f32 },
        torso_deg_s: (torso / steps) as f32,
        arms_deg_s: (arms / steps) as f32,
        fingers_deg_s: (fingers / steps) as f32,
        peak_deg_s: peak,
        peak_bone: peak_bone.name().to_owned(),
        phrases_per_min: per_min(phrases as f32),
        strokes_per_min: per_min(strokes),
        stroke_offset_p50_ms: pct(0.5),
        stroke_offset_p90_ms: pct(0.9),
        strokes_within_100ms: if strokes > 0.0 {
            offsets.iter().filter(|o| **o <= 100.0).count() as f32 / strokes
        } else {
            0.0
        },
        strokes_predicted: if strokes > 0.0 {
            landings.iter().filter(|(_, p)| *p).count() as f32 / strokes
        } else {
            0.0
        },
        checksum,
    }
}

/// The face-only body (the bus animator alone, which `BodyStyle::OFF` is exactly) on the same take, measured the same
/// way: the baseline a living body is compared with.
pub fn measure_face_only(take: &Take, base: &HumanoidPose) -> BodyMotion {
    measure(&BodyStyle::OFF, take, base, 0)
}
