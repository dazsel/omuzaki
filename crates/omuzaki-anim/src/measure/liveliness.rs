//! The four liveliness numbers (spec VF-05), taken from a stream of frames at a fixed step.

use omuzaki_anim_api::{Channel, ChannelBuf, ChannelMask, LOOK_FULL_RAD};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A blink needs both lids past this (a lid at 0.6 is mostly shut).
const BLINK_LEVEL: f32 = 0.6;
/// Shortest and longest closed time that counts as a blink or a wink, seconds. Shorter is tracker noise; longer is
/// eyes held shut.
const BLINK_SECONDS: (f32, f32) = (0.040, 0.400);
/// Eye angular speed that makes a saccade, degrees per second.
const SACCADE_DEG_S: f32 = 100.0;
/// Two saccade peaks closer than this are one movement, seconds.
const SACCADE_GAP_S: f32 = 0.050;
/// A channel that moves less than this in one frame is still.
const FROZEN_STEP: f32 = 1e-3;
/// Slack so that a run of exactly 40 ms at 60 fps (2.4 frames, 3 frames = 50 ms) is not lost to rounding.
const EPS: f32 = 1e-6;

/// The face and head channels the change and frozen metrics read: the 52 `ARKit` shapes and the head pose.
const MEASURED: ChannelMask = ChannelMask::from_bits_truncate(ChannelMask::ARKIT.bits() | ChannelMask::HEAD.bits());

/// How alive a stream looks. All rates use the stream's length, `frames * dt`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Liveliness {
    /// Length of the stream, seconds.
    pub seconds: f32,
    /// Mean absolute change per second over the face and head channels (each in its own unit).
    pub change_per_second: f32,
    /// Blinks per minute: both `eyeBlink` channels above 0.6 for 40 to 400 ms.
    pub blinks_per_minute: f32,
    /// Winks per minute: one lid shut for 40 to 400 ms while the other never was.
    pub winks_per_minute: f32,
    /// Eye movements per second: peaks of eye angular speed above 100 degrees a second, at least 50 ms apart.
    pub saccades_per_second: f32,
    /// The longest stretch, seconds, in which no face or head channel moved by 0.001 or more between frames.
    pub longest_frozen_s: f32,
    /// Blinks counted.
    pub blinks: u32,
    /// Winks counted.
    pub winks: u32,
    /// Saccades counted.
    pub saccades: u32,
}

/// Measures `frames`, which are `dt` seconds apart. Deterministic and allocation-free: it only reads the slice, in
/// constant space. A stream of fewer than two frames, or a `dt` that is not positive and finite, measures as all
/// zeros (there is nothing to measure).
#[allow(clippy::cast_precision_loss)] // frame counts stay far below 2^24 for any stream a person records
pub fn liveliness(frames: &[ChannelBuf], dt: f32) -> Liveliness {
    if frames.len() < 2 || !dt.is_finite() || dt <= 0.0 {
        return Liveliness::default();
    }
    let seconds = frames.len() as f32 * dt;
    let (change, longest_frozen) = change_and_frozen(frames, dt);
    let (blinks, winks) = lid_events(frames, dt);
    let saccades = saccade_count(frames, dt);
    Liveliness {
        seconds,
        change_per_second: change,
        blinks_per_minute: blinks as f32 * 60.0 / seconds,
        winks_per_minute: winks as f32 * 60.0 / seconds,
        saccades_per_second: saccades as f32 / seconds,
        longest_frozen_s: longest_frozen,
        blinks,
        winks,
        saccades,
    }
}

/// Mean absolute delta per second, and the longest run of frame steps in which nothing moved.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // the mean of small steps fits an f32
fn change_and_frozen(frames: &[ChannelBuf], dt: f32) -> (f32, f32) {
    let mut total = 0.0_f64;
    let (mut run, mut longest) = (0_u32, 0_u32);
    for pair in frames.windows(2) {
        let mut step_sum = 0.0_f32;
        let mut still = true;
        for c in MEASURED {
            let d = (pair[1].get(c) - pair[0].get(c)).abs();
            step_sum += d;
            still &= d < FROZEN_STEP;
        }
        total += f64::from(step_sum);
        run = if still { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let steps = (frames.len() - 1) as f64;
    let per_step = total / steps / f64::from(MEASURED.count());
    ((per_step / f64::from(dt)) as f32, longest as f32 * dt)
}

/// Which lids are shut (past the level) in one frame.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Lids {
    Open,
    Left,
    Right,
    Both,
}

impl Lids {
    fn of(f: &ChannelBuf) -> Self {
        match (f.get(Channel::EYE_BLINK_LEFT) > BLINK_LEVEL, f.get(Channel::EYE_BLINK_RIGHT) > BLINK_LEVEL) {
            (true, true) => Self::Both,
            (true, false) => Self::Left,
            (false, true) => Self::Right,
            (false, false) => Self::Open,
        }
    }
}

/// Whether `frames` frames of `dt` seconds last as long as a blink or a wink may.
#[allow(clippy::cast_precision_loss)] // a run is a few frames
fn blink_length(frames: usize, dt: f32) -> bool {
    let t = frames as f32 * dt;
    (BLINK_SECONDS.0 - EPS..=BLINK_SECONDS.1 + EPS).contains(&t)
}

/// Blinks and winks, counted per contiguous run of one lid state. A blink is a run of both lids shut for the right
/// time. A wink is a run of one lid shut for the right time that does not touch a both-lids run: a lid that leads or
/// trails a blink by a few frames is part of that blink, not a wink. One pass, constant space: a run is judged when
/// the next one begins, which is when its neighbour on that side is known.
fn lid_events(frames: &[ChannelBuf], dt: f32) -> (u32, u32) {
    let (mut blinks, mut winks) = (0, 0);
    let mut judge = |state: Lids, len: usize, before: Option<Lids>, after: Option<Lids>| {
        let fits = blink_length(len, dt);
        let touches_both = before == Some(Lids::Both) || after == Some(Lids::Both);
        match state {
            Lids::Both => blinks += u32::from(fits),
            Lids::Left | Lids::Right => winks += u32::from(fits && !touches_both),
            Lids::Open => {}
        }
    };
    let (mut run, mut len, mut before) = (Lids::Open, 0_usize, None);
    for state in frames.iter().map(Lids::of) {
        if len > 0 && state != run {
            judge(run, len, before, Some(state));
            (before, len) = (Some(run), 0);
        }
        run = state;
        len += 1;
    }
    if len > 0 {
        judge(run, len, before, None);
    }
    (blinks, winks)
}

/// Eye speed in degrees a second over the step from frame `i` to `i + 1`; `0` past the end.
fn eye_speed(frames: &[ChannelBuf], i: usize, dt: f32) -> f32 {
    match frames.get(i..i + 2) {
        Some([a, b]) => {
            let (a, b) = (a.gaze(), b.gaze());
            (b.0 - a.0).hypot(b.1 - a.1) * LOOK_FULL_RAD.to_degrees() / dt
        }
        _ => 0.0,
    }
}

/// Saccades: local peaks of eye speed above the threshold. A peak is a speed above the threshold, above the one before
/// it, and followed (after any run of equal speeds) by a lower one. Two peaks less than 50 ms apart are one movement,
/// and the faster one stands for it. A plateau counts once, at its first frame. Constant space.
#[allow(clippy::cast_precision_loss)] // frame indices stay far below 2^24
#[allow(clippy::float_cmp)] // a plateau is a run of exactly equal speeds, as a held tracker value produces
fn saccade_count(frames: &[ChannelBuf], dt: f32) -> u32 {
    let (mut count, mut kept) = (0_u32, None::<(f32, f32)>);
    let (mut i, mut before) = (0, 0.0_f32);
    while i + 1 < frames.len() {
        let v = eye_speed(frames, i, dt);
        let mut end = i;
        while eye_speed(frames, end + 1, dt) == v && end + 2 < frames.len() {
            end += 1;
        }
        if v > SACCADE_DEG_S && v > before && eye_speed(frames, end + 1, dt) < v {
            let t = i as f32 * dt;
            match kept {
                Some((last_t, last_v)) if t - last_t < SACCADE_GAP_S - EPS => {
                    if v > last_v {
                        kept = Some((t, v));
                    }
                }
                _ => {
                    count += 1;
                    kept = Some((t, v));
                }
            }
        }
        (before, i) = (v, end + 1);
    }
    count
}
