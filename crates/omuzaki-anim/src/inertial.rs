//! Inertialized transitions (#2354): switching what drives the body (a clip, a gesture, the idle, locomotion) without a
//! crossfade.
//!
//! A crossfade plays two sources at once and averages them, so for its whole length the body shows neither: a gesture
//! that brings the hands in front while the idle lets them hang can sweep the arms through the body, and the new motion
//! is diluted until the fade ends. Inertialization (Bollo, GDC 2016 and 2018) plays only the **new** source, at full
//! fidelity from its first frame, and carries what was on screen as an **offset** from it: per bone, the rotation from
//! the new source's pose to the last pose shown, and for the hips the distance. The offset and its velocity then decay
//! to zero with a critically damped spring, so the output's position and velocity are continuous across the switch and
//! it lands exactly on the new source (bit for bit once the offset is below [`SETTLED`]).
//!
//! The offset's starting velocity is the output's own velocity just before the switch, so the motion carries on. That
//! assumes the new source starts at rest, which the pose clips do: their `smooth` and `smoother` easings stop at every
//! key (`poses::clip`). A source that starts moving adds its own velocity on top, as it should.
//!
//! [`Inertializer::switch`] marks the switch; [`Inertializer::step`] is called every frame with whatever source is
//! current. One state per character, fixed size: a frame allocates nothing.

use omuzaki_anim_api::{HUMANOID_COUNT, HumanoidPose};
use omuzaki_core::glam::{Quat, Vec3};

/// The default half-life of the spring, seconds: its envelope halves this often. From rest an offset has 60% left after
/// one half-life and under 3% after four, so 0.1 s lands in about 0.4 s, close to a 0.25 s crossfade in feel, without
/// its averaging.
pub const DEFAULT_HALF_LIFE: f32 = 0.1;
/// The longest step taken, seconds, so a stalled frame cannot fling the spring.
pub const MAX_DT: f32 = 0.1;
/// Once every offset is below this (radians, metres: a tenth of a millimetre at arm's length) and every offset velocity
/// below it per second, the offset is dropped and the output is the source exactly.
pub const SETTLED: f32 = 1e-4;
/// The shortest half-life accepted, seconds; a shorter one (or a non-finite one) is raised to it.
const MIN_HALF_LIFE: f32 = 1e-3;

/// The rotation from `from` to `to` as a rotation vector (axis times angle, radians), the short way round.
fn rotation_vector(from: Quat, to: Quat) -> Vec3 {
    let q = to * from.inverse();
    let q = if q.w < 0.0 { -q } else { q };
    let v = q.normalize().to_scaled_axis();
    if v.is_finite() { v } else { Vec3::ZERO }
}

/// One critically damped decay of an offset and its velocity over `dt`, with `damping` (`2 ln 2 / half-life`): the
/// exact solution, so any step size is stable.
fn decay(offset: Vec3, velocity: Vec3, damping: f32, dt: f32) -> (Vec3, Vec3) {
    let j = velocity + offset * damping;
    let fall = (-damping * dt).exp();
    (fall * (offset + j * dt), fall * (velocity - j * damping * dt))
}

/// The inertialization state of one character's body.
#[derive(Clone, Debug)]
pub struct Inertializer {
    /// `ln 2 / half-life * 2`: the spring's damping.
    damping: f32,
    /// Per bone, the rotation applied on top of the source (parent space, applied first), as a rotation vector.
    offsets: [Vec3; HUMANOID_COUNT],
    /// The offsets' angular velocities, radians per second.
    spins: [Vec3; HUMANOID_COUNT],
    /// The hips' offset from the source, metres, and its velocity.
    hips: Vec3,
    hips_velocity: Vec3,
    /// Whether an offset is live; when not, the output is the source exactly.
    active: bool,
    /// The last pose shown and its velocity (per bone angular, and the hips'), for the next switch.
    last: Option<HumanoidPose>,
    last_spins: [Vec3; HUMANOID_COUNT],
    last_hips_velocity: Vec3,
    /// The next step starts from a new source.
    pending: bool,
}

impl Default for Inertializer {
    fn default() -> Self {
        Self::new(DEFAULT_HALF_LIFE)
    }
}

impl Inertializer {
    /// A state whose offsets halve every `half_life` seconds (at least a millisecond; a non-finite value takes
    /// [`DEFAULT_HALF_LIFE`]).
    pub fn new(half_life: f32) -> Self {
        let half_life = if half_life.is_finite() { half_life.max(MIN_HALF_LIFE) } else { DEFAULT_HALF_LIFE };
        Self {
            damping: 2.0 * std::f32::consts::LN_2 / half_life,
            offsets: [Vec3::ZERO; HUMANOID_COUNT],
            spins: [Vec3::ZERO; HUMANOID_COUNT],
            hips: Vec3::ZERO,
            hips_velocity: Vec3::ZERO,
            active: false,
            last: None,
            last_spins: [Vec3::ZERO; HUMANOID_COUNT],
            last_hips_velocity: Vec3::ZERO,
            pending: false,
        }
    }

    /// The source changes: the next [`step`](Self::step) gets the new one, and the output carries on from the last pose
    /// shown. A switch during a transition starts from where that transition had got to. Before any step there is
    /// nothing to carry, and the first step shows its source as it is.
    pub fn switch(&mut self) {
        self.pending = true;
    }

    /// Forgets everything: the next step shows its source as it is.
    pub fn reset(&mut self) {
        *self = Self { damping: self.damping, ..Self::new(DEFAULT_HALF_LIFE) };
    }

    /// Whether an offset is still decaying (`false` once the output is the source exactly).
    pub fn is_transitioning(&self) -> bool {
        self.active
    }

    /// One frame: the current `source` with what is left of the offset on top. `dt` is the time since the last step,
    /// seconds (non-finite or negative counts as `0`, longer than [`MAX_DT`] as `MAX_DT`).
    pub fn step(&mut self, dt: f32, source: &HumanoidPose) -> HumanoidPose {
        let dt = if dt.is_finite() { dt.clamp(0.0, MAX_DT) } else { 0.0 };
        if self.pending {
            self.pending = false;
            if let Some(last) = &self.last {
                for (offset, (from, to)) in self.offsets.iter_mut().zip(source.rotations.iter().zip(&last.rotations)) {
                    *offset = rotation_vector(*from, *to);
                }
                self.spins = self.last_spins;
                self.hips = last.hips_offset - source.hips_offset;
                self.hips_velocity = self.last_hips_velocity;
                self.active = true;
            }
        }
        if self.active {
            self.advance(dt);
        }
        let mut out = *source;
        if self.active {
            for ((rotation, offset), from) in out.rotations.iter_mut().zip(&self.offsets).zip(&source.rotations) {
                *rotation = (Quat::from_scaled_axis(*offset) * *from).normalize();
            }
            out.hips_offset = source.hips_offset + self.hips;
        }
        self.remember(dt, &out);
        out
    }

    /// Decays every offset by `dt`, and drops them all once they have settled.
    fn advance(&mut self, dt: f32) {
        let mut settled = true;
        for (x, v) in self.offsets.iter_mut().zip(self.spins.iter_mut()) {
            (*x, *v) = decay(*x, *v, self.damping, dt);
            settled &= x.length() < SETTLED && v.length() < SETTLED;
        }
        (self.hips, self.hips_velocity) = decay(self.hips, self.hips_velocity, self.damping, dt);
        settled &= self.hips.length() < SETTLED && self.hips_velocity.length() < SETTLED;
        if settled {
            self.offsets = [Vec3::ZERO; HUMANOID_COUNT];
            self.spins = [Vec3::ZERO; HUMANOID_COUNT];
            (self.hips, self.hips_velocity) = (Vec3::ZERO, Vec3::ZERO);
            self.active = false;
        }
    }

    /// Keeps `out` and its velocity for the next switch. A zero step keeps the last velocity.
    fn remember(&mut self, dt: f32, out: &HumanoidPose) {
        if let Some(last) = &self.last
            && dt > 0.0
        {
            for (spin, (from, to)) in self.last_spins.iter_mut().zip(last.rotations.iter().zip(&out.rotations)) {
                *spin = rotation_vector(*from, *to) / dt;
            }
            self.last_hips_velocity = (out.hips_offset - last.hips_offset) / dt;
        }
        self.last = Some(*out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_decay_halves_the_offset_every_half_life_from_rest() {
        let y = 2.0 * std::f32::consts::LN_2 / 0.1;
        // A critically damped spring from rest is not a pure exponential: at one half-life it has (1 + 2 ln 2) / 4 of
        // its start left.
        let (x, _) = decay(Vec3::X, Vec3::ZERO, y, 0.1);
        let exact = (1.0 + 2.0 * std::f32::consts::LN_2) * 0.25;
        assert!((x.x - exact).abs() < 1e-5, "{x}");
        // Ten steps of a tenth are one step of the whole.
        let mut s = (Vec3::X, Vec3::Y);
        for _ in 0..10 {
            s = decay(s.0, s.1, y, 0.01);
        }
        let whole = decay(Vec3::X, Vec3::Y, y, 0.1);
        assert!((s.0 - whole.0).length() < 1e-5 && (s.1 - whole.1).length() < 1e-4);
    }

    #[test]
    fn a_rotation_vector_takes_the_short_way_and_survives_nonsense() {
        let a = Quat::from_rotation_z(0.3);
        assert!((rotation_vector(a, -a)).length() < 1e-6, "q and -q are the same rotation");
        assert!((rotation_vector(Quat::IDENTITY, a) - Vec3::Z * 0.3).length() < 1e-6);
        assert_eq!(rotation_vector(Quat::from_xyzw(f32::NAN, 0.0, 0.0, 1.0), a), Vec3::ZERO);
    }
}
