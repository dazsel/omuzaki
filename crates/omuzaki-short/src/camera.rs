//! The camera track (SH-10): the shot in force at a moment, as a camera.
//!
//! Every shot frames through the stage's one framing function (`omuzaki_stage::scene::frame::frame`), so a `face`
//! shot means the same on every character. On top of it come what a person behind a camera does:
//!
//! - **cuts** at the line that names a new shot, or an **ease** from the shot before over `ease` seconds;
//! - a **punch-in**: the zoom grows by `punch` over [`PUNCH_S`] at the shot's start, eased out, as an editor's quick
//!   zoom on a word;
//! - a **push**: the zoom grows by `push` over the line;
//! - **dutch roll** about the lens axis;
//! - **handheld** drift, always a little ([`DRIFT_DEG`]), and `shake` on top: a stronger drift plus a jolt at the cut
//!   that dies away in [`JOLT_S`]. The drift is a sum of slow sines with phases from the short's seed, so it is the
//!   same on every render.

use omuzaki_core::glam::Mat4;
use omuzaki_render_api::Camera;
use omuzaki_stage::scene::frame::frame;
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage_api::{CameraDef, Framing};

use crate::def::{ShotFraming, ShotNumbers};
use crate::error::ShortError;
use crate::timeline::Timeline;

/// How long a punch-in takes, seconds.
pub const PUNCH_S: f32 = 0.22;
/// The handheld drift every shot has, degrees (yaw and pitch; roll is half).
pub const DRIFT_DEG: f32 = 0.25;
/// How much drift `shake = 1` adds, degrees.
pub const SHAKE_DEG: f32 = 1.2;
/// How long a shake's jolt at the cut lasts, seconds.
pub const JOLT_S: f32 = 0.35;
/// The lens's vertical field of view, degrees: a little long, as phone shorts are shot.
pub const FOV_DEG: f32 = 28.0;

/// What the camera track needs of the character and the frame.
#[derive(Clone, Copy, Debug)]
pub struct CameraRig {
    /// The character, measured in its bind pose.
    pub landmarks: Landmarks,
    /// Where the character stands.
    pub root: Mat4,
    /// Width over height.
    pub aspect: f32,
    /// The short's seed, for the handheld drift.
    pub seed: u64,
}

fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

fn ease_out(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    1.0 - (1.0 - x).powi(3)
}

fn lerp(a: f32, b: f32, k: f32) -> f32 {
    a + (b - a) * k
}

/// Three smooth noises in -1..1 (yaw, pitch, roll) at `t`, from `seed`.
#[allow(clippy::cast_precision_loss, reason = "seed bits to a phase")]
fn wobble(seed: u64, t: f32) -> [f32; 3] {
    let phase = |k: u64| {
        ((seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(k.wrapping_mul(0x632B_E59B_D9B4_E019)) >> 40) as f32)
            / (1u64 << 24) as f32
            * std::f32::consts::TAU
    };
    std::array::from_fn(|axis| {
        let a = axis as u64 * 3;
        0.55 * (t * 0.71 + phase(a)).sin()
            + 0.3 * (t * 1.37 + phase(a + 1)).sin()
            + 0.15 * (t * 2.93 + phase(a + 2)).sin()
    })
}

impl CameraRig {
    /// The shot `i` of `timeline` as numbers at `t`: its own, eased from the one before, with its punch and push.
    fn numbers_at(timeline: &Timeline, i: usize, t: f32) -> ShotNumbers {
        let span = &timeline.shots[i];
        let mut n = span.shot;
        let since = t - span.start;
        if n.ease > 0.0 && i > 0 {
            let before = Self::numbers_at(timeline, i - 1, span.start);
            if before.framing == n.framing {
                let k = smoothstep(since / n.ease);
                n.yaw = lerp(before.yaw, n.yaw, k);
                n.pitch = lerp(before.pitch, n.pitch, k);
                n.roll = lerp(before.roll, n.roll, k);
                n.zoom = lerp(before.zoom, n.zoom, k);
                n.shift = [lerp(before.shift[0], n.shift[0], k), lerp(before.shift[1], n.shift[1], k)];
                if let (Some(a), Some(b)) = (before.head_width, n.head_width) {
                    n.head_width = Some(lerp(a, b, k));
                }
            }
        }
        if n.punch > 1.0 {
            n.zoom *= lerp(1.0, n.punch, ease_out(since / PUNCH_S));
        }
        if n.push > 1.0 {
            let length = (span.line_end - span.start).max(0.1);
            n.zoom *= lerp(1.0, n.push, smoothstep(since / length));
        }
        n
    }

    /// The shot at `t` seconds as numbers, the handheld drift and the cut's jolt on top (roll included): what a camera
    /// does, before it is a camera. The 2D puppet path frames from these.
    #[must_use]
    pub fn shot_at(timeline: &Timeline, seed: u64, t: f32) -> ShotNumbers {
        let i = timeline.shot_index_at(t);
        let mut n = Self::numbers_at(timeline, i, t);
        let since = t - timeline.shots[i].start;
        let w = wobble(seed, t);
        let drift = DRIFT_DEG * timeline.handheld + SHAKE_DEG * n.shake;
        let jolt = n.shake * 2.5 * (-since / (JOLT_S / 4.0)).exp() * (since * 38.0).sin();
        n.yaw += drift * w[0] + jolt;
        n.pitch = (n.pitch + drift * w[1] + 0.6 * jolt).clamp(-89.0, 89.0);
        n.roll += 0.5 * drift * w[2] + 0.4 * jolt;
        n
    }

    /// The camera at `t` seconds.
    ///
    /// # Errors
    /// [`ShortError::Render`] when the framing can't frame the character (no head for a face shot).
    pub fn camera_at(&self, timeline: &Timeline, t: f32) -> Result<Camera, ShortError> {
        let n = Self::shot_at(timeline, self.seed, t);
        let roll = n.roll;
        let framing = match n.framing {
            ShotFraming::Face => Framing::Face,
            ShotFraming::Bust => Framing::Bust,
        };
        let mut def = CameraDef::preset(framing);
        def.yaw = n.yaw.clamp(-180.0, 180.0);
        def.pitch = n.pitch;
        def.fov = FOV_DEG;
        def.shift = n.shift;
        let base = match (n.head_width, self.landmarks.head) {
            (Some(width), Some(head)) => self.zoom_for_head_width(framing, &def, head, width)?,
            _ => 1.0,
        };
        def.zoom = (base * n.zoom).max(0.05);
        let mut camera =
            frame(framing, &self.landmarks, &def, self.aspect).map_err(|e| ShortError::Render(e.to_string()))?;
        camera.transform = self.root * camera.transform * Mat4::from_rotation_z(-roll.to_radians());
        Ok(camera)
    }
}

impl CameraRig {
    /// The zoom at which `head` spans `width` of the frame's width with `def`'s framing: the head is measured through
    /// the lens and the zoom scaled to it, a few times over, since the share grows a little faster than the zoom up
    /// close.
    fn zoom_for_head_width(
        &self,
        framing: Framing,
        def: &CameraDef,
        head: omuzaki_stage::scene::landmarks::Bounds,
        width: f32,
    ) -> Result<f32, ShortError> {
        zoom_for_share(width, |zoom| {
            let mut d = def.clone();
            d.zoom = zoom;
            let camera =
                frame(framing, &self.landmarks, &d, self.aspect).map_err(|e| ShortError::Render(e.to_string()))?;
            Ok(head_share(&camera, self.aspect, head))
        })
    }
}

/// The zoom at which `share_at` gives `want`: scaled by the miss until it lands within a thousandth.
fn zoom_for_share(want: f32, mut share_at: impl FnMut(f32) -> Result<f32, ShortError>) -> Result<f32, ShortError> {
    let mut zoom = 1.0_f32;
    for _ in 0..24 {
        let share = share_at(zoom)?;
        if !(share.is_finite() && share > 1e-4) || (share - want).abs() <= 1e-3 * want {
            break;
        }
        zoom = (zoom * want / share).clamp(0.05, 20.0);
    }
    Ok(zoom)
}

/// How much of the frame's width `head`'s box spans through `camera`: the spread of its corners in normalised
/// device coordinates, halved. Corners behind the lens are left out.
fn head_share(camera: &Camera, aspect: f32, head: omuzaki_stage::scene::landmarks::Bounds) -> f32 {
    let clip_from_world = camera.projection_matrix(aspect) * camera.transform.inverse();
    let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
    for i in 0..8 {
        let p = omuzaki_core::glam::Vec3::new(
            if i & 1 == 0 { head.min.x } else { head.max.x },
            if i & 2 == 0 { head.min.y } else { head.max.y },
            if i & 4 == 0 { head.min.z } else { head.max.z },
        );
        let c = clip_from_world * p.extend(1.0);
        if c.w > 1e-5 {
            let x = c.x / c.w;
            lo = lo.min(x);
            hi = hi.max(x);
        }
    }
    (hi - lo) * 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn punch_and_push_grow_the_zoom_then_hold() {
        assert!((ease_out(0.0)).abs() < 1e-6 && (ease_out(1.0) - 1.0).abs() < 1e-6);
        assert!(smoothstep(0.5) > 0.49 && smoothstep(0.5) < 0.51);
        assert!(ease_out(0.3) > 0.3, "a punch moves fast first");
    }

    #[test]
    fn the_head_is_measured_through_the_lens() {
        use omuzaki_core::glam::Vec3;
        use omuzaki_stage::scene::landmarks::Bounds;
        // A 0.2 m wide box 1 m in front of a 90 degree lens on a square frame lands at -0.1 and 0.1: a tenth of the width.
        let camera = Camera {
            transform: Mat4::IDENTITY,
            projection: omuzaki_render_api::Projection::Perspective { fov_y_deg: 90.0, near: 0.01, far: 10.0 },
        };
        let head = Bounds { min: Vec3::new(-0.1, -0.1, -1.0), max: Vec3::new(0.1, 0.1, -1.0) };
        assert!((head_share(&camera, 1.0, head) - 0.1).abs() < 1e-4);
        let behind = Bounds { min: Vec3::new(-0.1, -0.1, 1.0), max: Vec3::new(0.1, 0.1, 1.0) };
        assert!(head_share(&camera, 1.0, behind).is_sign_negative(), "a head behind the lens spans nothing");
    }

    #[test]
    fn the_zoom_lands_the_share_asked_even_when_it_grows_faster_than_the_zoom() {
        // Up close a head grows faster than the zoom (its near side comes at the lens): share = 0.4 z / (1 - 0.1 z).
        let share = |z: f32| Ok::<f32, ShortError>(0.4 * z / (1.0 - 0.1 * z));
        for want in [0.5_f32, 0.8, 1.6] {
            let z = zoom_for_share(want, share).unwrap();
            let got = share(z).unwrap();
            assert!((got - want).abs() < 0.01 * want, "asked {want}, got {got} at zoom {z}");
        }
    }
}
