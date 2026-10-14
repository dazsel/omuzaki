//! The solver: a world position in, eased per-eye `eyeLook*` channels out.

use omuzaki_anim_api::{Bus, Channel, ChannelMask, Source, Tick};
use omuzaki_core::glam::{Mat4, Vec3};

use super::target::{GazeKind, Resolved};

/// Radians of eye turn per look unit sideways (`face.js`: `atan2(x, z) / 0.7`; 1 unit is about 40 degrees).
pub const X_RADIANS_PER_UNIT: f32 = 0.7;
/// Radians of eye turn per look unit vertically (`face.js`: `asin(y / |v|) / 0.6`).
pub const Y_RADIANS_PER_UNIT: f32 = 0.6;
/// Easing rate (1/s) for ambient following, as in `face.js` (`1 - exp(-dt * 10)`).
pub const FOLLOW_RATE: f32 = 10.0;
/// Easing rate (1/s) for a deliberate look; a saccade lands in tens of milliseconds (`1 - exp(-dt * 40)`).
pub const DRIVEN_RATE: f32 = 40.0;
/// Half the distance between the pupils in the face layout (`face.js`: `EX = 0.090`), in eye-frame units.
pub const HALF_EYE_SPACING: f32 = 0.090;
/// A target is followed only while it is this far in front of the eyes: `z / |v|` must exceed it (about 75 degrees
/// off axis). Behind that the eyes ease back to centre instead of whipping round.
const FRONT_CONE: f32 = 0.25;

/// One eye's look, in look units (`x` positive toward the character's left, `y` positive up).
type Look = (f32, f32);

/// The eyes after a solve.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct GazeFrame {
    /// The character's left eye.
    pub left: (f32, f32),
    /// The character's right eye.
    pub right: (f32, f32),
}

impl GazeFrame {
    /// The mean look of the two eyes, which is what a single-eye consumer follows.
    pub fn look(&self) -> (f32, f32) {
        (f32::midpoint(self.left.0, self.right.0), f32::midpoint(self.left.1, self.right.1))
    }

    /// How far the eyes turn toward each other, in look units: mean `eyeLookIn*` minus mean `eyeLookOut*`, as the
    /// painter derives it. Multiply by [`X_RADIANS_PER_UNIT`] for the angle each eye turns inward.
    pub fn vergence(&self) -> f32 {
        // Toward the character's left (positive x) turns the left eye out and the right eye in.
        f32::midpoint(self.right.0, -self.left.0)
    }
}

/// Aims the eyes at world targets and writes the result to the bus.
///
/// It keeps the eased look of the cyclopean eye (the midpoint between the pupils, which is what `face.js` follows) and
/// each eye's small offset from it, so near targets converge. No randomness and no allocation.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct GazeSolver {
    half_spacing: f32,
    look: Look,
    left: Look,
    right: Look,
}

impl Default for GazeSolver {
    fn default() -> Self {
        Self::new(HALF_EYE_SPACING)
    }
}

impl GazeSolver {
    /// A solver for eyes `2 * half_spacing` apart, in the units of the eye frame it will be given.
    pub fn new(half_spacing: f32) -> Self {
        Self { half_spacing, look: (0.0, 0.0), left: (0.0, 0.0), right: (0.0, 0.0) }
    }

    /// The eased cyclopean look, before per-eye convergence.
    pub fn look(&self) -> (f32, f32) {
        self.look
    }

    /// One frame. `eye_frame` is the head's world transform with the eye offset applied (the origin sits between the
    /// pupils, `+z` forward, `+x` toward the character's left, `+y` up); `target` is the resolved gaze target, or
    /// `None` for no target.
    ///
    /// With a target, the `eyeLook*` channels are replaced by the eased solution and the gaze region is claimed for
    /// [`Source::Score`]. With none, or while a person drives the eyes (the solver never fights a human), the bus is
    /// left alone and the easing state resets to centre, so the next target starts from rest. A target behind the
    /// character eases the eyes back to centre.
    pub fn step(&mut self, tick: Tick, eye_frame: Mat4, target: Option<Resolved>, bus: &mut Bus) -> GazeFrame {
        let Some(target) = target.filter(|_| !bus.owners.human_owns_any(ChannelMask::GAZE)) else {
            *self = Self::new(self.half_spacing);
            return GazeFrame::default();
        };
        let goal = self.goal(eye_frame.inverse().transform_point3(target.position));
        let rate = match target.kind {
            GazeKind::Follow => FOLLOW_RATE,
            GazeKind::Driven => DRIVEN_RATE,
        };
        let k = ease(tick.dt, rate);
        self.look = approach(self.look, goal.look, k);
        self.left = approach(self.left, goal.left, k);
        self.right = approach(self.right, goal.right, k);

        let frame = GazeFrame {
            left: clamp_look((self.look.0 + self.left.0, self.look.1 + self.left.1)),
            right: clamp_look((self.look.0 + self.right.0, self.look.1 + self.right.1)),
        };
        write(&mut bus.values, &frame);
        bus.owners.claim(Source::Score, ChannelMask::GAZE);
        frame
    }

    /// Where each eye wants to look for a target at `v` (eye-frame space): the cyclopean look, and each eye's offset
    /// from it. All zero when the target is not in front of the eyes or not a finite point.
    fn goal(&self, v: Vec3) -> Goal {
        let length = v.length();
        if !(length > 0.0 && length.is_finite() && v.z / length > FRONT_CONE) {
            return Goal::CENTRE;
        }
        let look = clamp_look(angles(v));
        // The left eye sits at +x, the right at -x; each aims from where it is. Each eye's goal is clamped like the
        // cyclopean one, so the offset is measured from the same clamped look and a saturated target stays per-eye.
        let offset = |side: f32| {
            let eye = clamp_look(angles(v - Vec3::new(side * self.half_spacing, 0.0, 0.0)));
            (eye.0 - look.0, eye.1 - look.1)
        };
        Goal { look, left: offset(1.0), right: offset(-1.0) }
    }
}

/// What the eyes are heading for.
struct Goal {
    look: Look,
    left: Look,
    right: Look,
}

impl Goal {
    const CENTRE: Self = Self { look: (0.0, 0.0), left: (0.0, 0.0), right: (0.0, 0.0) };
}

/// Look units for the direction to `v` (`face.js`: `atan2(x, z) / 0.7`, `asin(y / |v|) / 0.6`), unclamped.
fn angles(v: Vec3) -> Look {
    (v.x.atan2(v.z) / X_RADIANS_PER_UNIT, (v.y / v.length()).asin() / Y_RADIANS_PER_UNIT)
}

fn clamp_look((x, y): Look) -> Look {
    (x.clamp(-1.0, 1.0), y.clamp(-1.0, 1.0))
}

/// The fraction of the remaining distance covered this frame. A zero (or negative) step snaps, as `face.js` does; a
/// non-finite one holds still.
fn ease(dt: f32, rate: f32) -> f32 {
    if dt.is_nan() {
        0.0
    } else if dt > 0.0 {
        1.0 - (-dt * rate).exp()
    } else {
        1.0
    }
}

fn approach(from: Look, to: Look, k: f32) -> Look {
    (from.0 + (to.0 - from.0) * k, from.1 + (to.1 - from.1) * k)
}

/// Writes both eyes into the eight `eyeLook*` channels.
fn write(values: &mut omuzaki_anim_api::ChannelBuf, frame: &GazeFrame) {
    // Toward the character's left (positive x) turns the left eye out and the right eye in.
    let eyes = [
        (
            frame.left,
            Channel::EYE_LOOK_OUT_LEFT,
            Channel::EYE_LOOK_IN_LEFT,
            Channel::EYE_LOOK_UP_LEFT,
            Channel::EYE_LOOK_DOWN_LEFT,
        ),
        (
            frame.right,
            Channel::EYE_LOOK_IN_RIGHT,
            Channel::EYE_LOOK_OUT_RIGHT,
            Channel::EYE_LOOK_UP_RIGHT,
            Channel::EYE_LOOK_DOWN_RIGHT,
        ),
    ];
    for ((x, y), toward_left, toward_right, up, down) in eyes {
        values[toward_left] = x.max(0.0);
        values[toward_right] = (-x).max(0.0);
        values[up] = y.max(0.0);
        values[down] = (-y).max(0.0);
    }
}
