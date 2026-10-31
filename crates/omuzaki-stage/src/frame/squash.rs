//! Squash and stretch (`scene.squash`): the drawn body stretches along the world's up axis as it moves up or down and
//! squashes when that motion stops or turns, keeping its volume.
//!
//! [`Jelly`] follows the hips' height. Its weight is a damped spring on the hips: a steady move carries it along, a
//! sudden stop (a landing) or start (the push of a hop) leaves it behind for a moment, and that lag squashes the body
//! before the spring bounces it back. The speed adds a stretch on top. The result is one matrix about the character's
//! ground point, `y` scaled by `s` and the ground plane by `1 / sqrt(s)`, which the stage applies to what it draws
//! only: the rig, the physics, the face and the touches see the body as posed.

use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_stage_api::Squash;

/// The longest step the spring takes at once, seconds: stable for the stiffest bounce the settings allow.
const MAX_STEP_S: f32 = 1.0 / 240.0;
/// A gap between body moves longer than this (a pause, a seek) starts the spring afresh, seconds.
const MAX_GAP_S: f64 = 0.5;
/// A hips move this far in one step is a teleport, not motion, metres: the spring starts afresh.
const TELEPORT_M: f32 = 1.0;
/// How quickly the hips' measured speed follows the motion, per second: enough to drop tracking jitter.
const SPEED_RATE: f32 = 30.0;

/// The squash and stretch state of one body.
#[derive(Clone, Debug)]
pub(crate) struct Jelly {
    settings: Squash,
    /// The last body move: when, and where the hips were.
    last: Option<(f64, f32)>,
    /// The hips' smoothed vertical speed, metres per second.
    speed: f32,
    /// The weight's height and vertical speed.
    weight: f32,
    weight_speed: f32,
    /// The height scale drawn now (`1` is as posed).
    scale: f32,
}

impl Default for Jelly {
    fn default() -> Self {
        Self { settings: Squash::OFF, last: None, speed: 0.0, weight: 0.0, weight_speed: 0.0, scale: 1.0 }
    }
}

impl Jelly {
    /// Takes a scene's settings; a change starts the spring afresh.
    pub(crate) fn set(&mut self, settings: Squash) {
        if settings != self.settings {
            *self = Self { settings, ..Self::default() };
        }
    }

    /// The height scale drawn now: `1` is as posed, above it stretched, below it squashed.
    pub(crate) fn scale(&self) -> f32 {
        self.scale
    }

    /// Follows a body move at `time` with the hips `hips_y` metres up, and returns the new height scale.
    pub(crate) fn step(&mut self, time: f64, hips_y: f32) -> f32 {
        let q = self.settings;
        if q.is_off() || !hips_y.is_finite() {
            self.scale = 1.0;
            return self.scale;
        }
        #[allow(clippy::cast_possible_truncation)] // a gap under half a second fits f32
        let dt = match self.last {
            Some((t, y)) if time > t && time - t <= MAX_GAP_S && (hips_y - y).abs() < TELEPORT_M => (time - t) as f32,
            _ => {
                self.last = Some((time, hips_y));
                (self.speed, self.weight, self.weight_speed, self.scale) = (0.0, hips_y, 0.0, 1.0);
                return self.scale;
            }
        };
        let measured = (hips_y - self.last.map_or(hips_y, |(_, y)| y)) / dt;
        self.last = Some((time, hips_y));
        self.speed += (measured - self.speed) * (1.0 - (-dt * SPEED_RATE).exp());
        let omega = std::f32::consts::TAU * q.bounce_hz;
        // The spring pulls the weight to the hips and damps its speed toward theirs; the hips are where this move left
        // them for the whole step.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small, positive count
        let steps = (dt / MAX_STEP_S).ceil().max(1.0) as u32;
        #[allow(clippy::cast_precision_loss)]
        let h = dt / steps as f32;
        for _ in 0..steps {
            let accel = -omega * omega * (self.weight - hips_y) - 2.0 * q.damping * omega * (self.weight_speed - measured);
            self.weight_speed += accel * h;
            self.weight += self.weight_speed * h;
        }
        // The weight below the hips (still falling as they stop, or left behind as they push up) squashes; above them
        // (the bounce back) it stretches. The lag in metres times omega is a speed, like the stretch's.
        let lag = omega * (self.weight - hips_y);
        let change = q.stretch * self.speed.abs() + q.squash * lag;
        self.scale = 1.0 + change.clamp(-q.limit, q.limit);
        self.scale
    }

    /// The drawn deformation for a body standing at `ground` (its root's origin): the height scaled about the ground
    /// point and the ground plane by the inverse square root, so the volume stays.
    pub(crate) fn matrix(&self, ground: Vec3) -> Mat4 {
        if (self.scale - 1.0).abs() < 1e-6 {
            return Mat4::IDENTITY;
        }
        let across = self.scale.sqrt().recip();
        Mat4::from_translation(ground)
            * Mat4::from_scale(Vec3::new(across, self.scale, across))
            * Mat4::from_translation(-ground)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARTOON: Squash = Squash { stretch: 0.1, squash: 0.2, bounce_hz: 3.0, damping: 0.35, limit: 0.25 };

    /// Steps `jelly` at 60 Hz through `heights`, returning the scale after each.
    fn run(jelly: &mut Jelly, heights: impl IntoIterator<Item = f32>) -> Vec<f32> {
        (0u16..).zip(heights).map(|(i, y)| jelly.step(f64::from(i) / 60.0, y)).collect()
    }

    fn jelly(settings: Squash) -> Jelly {
        let mut j = Jelly::default();
        j.set(settings);
        j
    }

    #[test]
    fn off_and_standing_still_draw_the_body_as_posed() {
        assert!(run(&mut jelly(Squash::OFF), (0..60u16).map(|i| f32::from(i) * 0.05)).iter().all(|&s| (s - 1.0).abs() < 1e-6));
        let still = run(&mut jelly(CARTOON), std::iter::repeat_n(0.9, 60));
        assert!(still.iter().all(|&s| (s - 1.0).abs() < 1e-6), "{still:?}");
        assert_eq!(jelly(CARTOON).matrix(Vec3::ZERO), Mat4::IDENTITY);
    }

    #[test]
    fn a_steady_rise_stretches_and_a_landing_squashes_then_settles() {
        let mut j = jelly(CARTOON);
        // Falling at 2 m/s for half a second, then on the ground.
        let fall = (0..30u16).map(|i| 2.0 - f32::from(i) * (2.0 / 60.0));
        let scales = run(&mut j, fall.chain(std::iter::repeat_n(1.0, 60)));
        let falling = scales[25];
        assert!(falling > 1.1, "stretched while falling: {falling}");
        let landed = scales[30..45].iter().copied().fold(f32::MAX, f32::min);
        assert!(landed < 0.95, "squashed on landing: {landed}");
        let settled = scales[scales.len() - 1];
        assert!((settled - 1.0).abs() < 0.02, "settles back: {settled}");
        assert!(scales.iter().all(|&s| (0.75..=1.25).contains(&s)), "within the limit");
    }

    #[test]
    fn the_volume_stays_and_the_ground_point_stays_put() {
        let mut j = jelly(CARTOON);
        run(&mut j, (0..20u16).map(|i| f32::from(i) * 0.03));
        let m = j.matrix(Vec3::new(0.5, 0.0, -1.0));
        let det = m.determinant();
        assert!((det - 1.0).abs() < 1e-4, "volume kept: {det}");
        assert!(m.transform_point3(Vec3::new(0.5, 0.0, -1.0)).abs_diff_eq(Vec3::new(0.5, 0.0, -1.0), 1e-5));
        assert!(j.scale() > 1.0, "rising stretches");
    }

    #[test]
    fn a_teleport_or_a_seek_starts_afresh() {
        let mut j = jelly(CARTOON);
        run(&mut j, (0..10u16).map(|i| f32::from(i) * 0.03));
        assert!(j.scale() > 1.0);
        assert_eq!(j.step(1.0, 5.0), 1.0, "a jump of metres is a teleport");
        assert_eq!(j.step(0.2, 5.0), 1.0, "a clock that went back");
    }
}
