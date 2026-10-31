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
//!   same on every render;
//! - **placing the face**: a shot's `eyes` (or `mouth`) say where on the frame the face lands, and the zoom, roll and
//!   shift are solved so it lands there ([`CameraRig::face`]), as a camera operator frames to a mark.

use omuzaki_core::glam::{Mat4, Vec2, Vec3, Vec4};
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
    /// Where the painted face's right eye, left eye and mouth are in the world with the character at rest, for shots
    /// that place the face (`eyes`, `mouth`); `None` for a character without one, whose shots keep their framing.
    pub face: Option<[Vec3; 3]>,
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
                let mix = |a: [f32; 2], b: [f32; 2]| [lerp(a[0], b[0], k), lerp(a[1], b[1], k)];
                if let (Some(a), Some(b)) = (before.eyes, n.eyes) {
                    n.eyes = Some([mix(a[0], b[0]), mix(a[1], b[1])]);
                }
                if let (Some(a), Some(b)) = (before.mouth, n.mouth) {
                    n.mouth = Some(mix(a, b));
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
        // The shot without the handheld drift: what a face is placed against, so the drift still moves it after.
        let still = Self::numbers_at(timeline, timeline.shot_index_at(t), t);
        let framing = match n.framing {
            ShotFraming::Face => Framing::Face,
            ShotFraming::Bust => Framing::Bust,
        };
        let base = match (still.head_width, self.landmarks.head) {
            (Some(width), Some(head)) => {
                let def = Self::def(framing, &still, 1.0, still.shift);
                self.zoom_for_head_width(framing, &def, head, width)?
            }
            _ => 1.0,
        };
        match (still.eyes, still.mouth, self.face) {
            (Some(marks), _, Some([right, left, _])) => {
                let placed = self.place_eyes(framing, &still, marks, [right, left])?;
                // The drift turns the placed camera where it stands, as a hand would.
                let turn = Mat4::from_rotation_y((n.yaw - still.yaw).to_radians())
                    * Mat4::from_rotation_x(-(n.pitch - still.pitch).to_radians())
                    * Mat4::from_rotation_z(-(n.roll - still.roll).to_radians());
                Ok(Camera { transform: placed.transform * turn, ..placed })
            }
            (None, Some(mark), Some([.., mouth])) => {
                let held = self.lens(framing, &still, base * still.zoom, still.shift, still.roll)?;
                let slide = self.slide_to(&held, mouth, mark);
                let mut camera = self.lens(framing, &n, base * n.zoom, n.shift, n.roll)?;
                camera.transform.w_axis += slide.extend(0.0);
                Ok(camera)
            }
            _ => self.lens(framing, &n, base * n.zoom, n.shift, n.roll),
        }
    }

    /// The stage's camera definition for `n` at `zoom` and `shift`.
    fn def(framing: Framing, n: &ShotNumbers, zoom: f32, shift: [f32; 2]) -> CameraDef {
        let mut def = CameraDef::preset(framing);
        def.yaw = n.yaw.clamp(-180.0, 180.0);
        def.pitch = n.pitch;
        def.fov = FOV_DEG;
        def.shift = shift;
        def.zoom = zoom.max(0.05);
        def
    }

    /// The camera for `n` at `zoom`, `shift` and `roll`, placed by the character's root.
    fn lens(
        &self,
        framing: Framing,
        n: &ShotNumbers,
        zoom: f32,
        shift: [f32; 2],
        roll: f32,
    ) -> Result<Camera, ShortError> {
        let def = Self::def(framing, n, zoom, shift);
        let mut camera =
            frame(framing, &self.landmarks, &def, self.aspect).map_err(|e| ShortError::Render(e.to_string()))?;
        camera.transform = self.root * camera.transform * Mat4::from_rotation_z(-roll.to_radians());
        Ok(camera)
    }

    /// Where `world` lands through `camera`, in frame heights from the frame's centre (x right, y down); `None`
    /// behind the lens.
    fn land(&self, camera: &Camera, world: Vec3) -> Option<Vec2> {
        let clip = camera.projection_matrix(self.aspect) * camera.transform.inverse() * world.extend(1.0);
        (clip.w > 1e-5).then(|| Vec2::new(clip.x / clip.w * 0.5 * self.aspect, -clip.y / clip.w * 0.5))
    }

    /// A share of the frame from its top left in the units [`Self::land`] gives.
    fn mark(&self, share: [f32; 2]) -> Vec2 {
        Vec2::new((share[0] - 0.5) * self.aspect, share[1] - 0.5)
    }

    /// The half height of the lens's view at a distance of one: `tan(fov / 2)`.
    fn spread() -> f32 {
        (FOV_DEG.to_radians() * 0.5).tan()
    }

    /// Where a point `depth` in front of the lens lands on `mark`, in the camera's own axes.
    fn seen_at(&self, mark: [f32; 2], depth: f32) -> Vec3 {
        let at = self.mark(mark);
        Vec3::new(2.0 * at.x * depth * Self::spread(), -2.0 * at.y * depth * Self::spread(), -depth)
    }

    /// The camera, looking as `n` looks, that lands `eyes` (world, right then left) on `marks`. The camera is placed,
    /// not framed: its position follows from the eye spacing and their midpoint in closed form, and its roll and
    /// position are then refined by Gauss-Newton on the miss through the lens (smooth in these numbers, so it lands in
    /// two or three rounds). `zoom`, `punch` and `push` then dolly it toward the eyes, which keeps them on their marks'
    /// line of sight.
    fn place_eyes(
        &self,
        framing: Framing,
        n: &ShotNumbers,
        marks: [[f32; 2]; 2],
        eyes: [Vec3; 2],
    ) -> Result<Camera, ShortError> {
        let looking = self.lens(framing, n, 1.0, [0.0, 0.0], 0.0)?;
        let turned = |roll: f32| {
            let mut camera = looking;
            camera.transform = looking.transform * Mat4::from_rotation_z(-roll.to_radians());
            camera
        };
        let want = marks.map(|m| self.mark(m));
        // The eye line's roll on the frame, and the eyes' own seen from the unrolled camera.
        let eye_line = |a: Vec2, b: Vec2| (b.y - a.y).atan2(b.x - a.x).to_degrees();
        let seen = eyes.map(|e| looking.transform.inverse().transform_point3(e));
        let roll0 =
            eye_line(want[0], want[1]) - eye_line(Vec2::new(seen[0].x, -seen[0].y), Vec2::new(seen[1].x, -seen[1].y));
        let spacing = want[0].distance(want[1]).max(1e-4);
        let depth = 0.5 * eyes[0].distance(eyes[1]) / (Self::spread() * spacing);
        let middle = eyes[0].midpoint(eyes[1]);
        let mid_mark = [marks[0][0].midpoint(marks[1][0]), marks[0][1].midpoint(marks[1][1])];
        let place = |v: [f32; 4]| {
            let mut camera = turned(roll0 + v[3]);
            let offset = camera.transform.transform_vector3(self.seen_at(mid_mark, depth));
            camera.transform.w_axis = (middle - offset + Vec3::new(v[0], v[1], v[2])).extend(1.0);
            camera
        };
        let miss = |v: [f32; 4]| -> Result<Option<[f32; 4]>, ShortError> {
            let camera = place(v);
            Ok(match (self.land(&camera, eyes[0]), self.land(&camera, eyes[1])) {
                (Some(a), Some(b)) => Some([a.x - want[0].x, a.y - want[0].y, b.x - want[1].x, b.y - want[1].y]),
                _ => None,
            })
        };
        let reach = 0.25 * depth.max(0.01);
        let v = solve([0.0; 4], [1e-4, 1e-4, 1e-4, 0.01], [reach, reach, reach, 10.0], miss)?;
        let mut camera = place(v);
        let position = camera.transform.w_axis.truncate();
        let zoom = n.zoom.max(0.05);
        camera.transform.w_axis = (middle + (position - middle) / zoom).extend(1.0);
        let distance = position.distance(middle) / zoom;
        if let omuzaki_render_api::Projection::Perspective { near, far, .. } = &mut camera.projection {
            *near = near.min(0.1 * distance).max(1e-3);
            *far = far.max(distance + 2.0 * self.landmarks.height_m);
        }
        Ok(camera)
    }

    /// How far to move `camera` across its own picture plane so `mouth` (world) lands on `mark`: exact, since a move
    /// in that plane keeps every depth.
    fn slide_to(&self, camera: &Camera, mouth: Vec3, mark: [f32; 2]) -> Vec3 {
        let seen = camera.transform.inverse().transform_point3(mouth);
        let want = self.seen_at(mark, -seen.z);
        camera.transform.transform_vector3(Vec3::new(seen.x - want.x, seen.y - want.y, 0.0))
    }
}

/// The four numbers from `start` at which `miss` is zero: Gauss-Newton with a finite-difference Jacobian (`step` per
/// number, taken backward where forward leaves the lens's front), each step capped by `reach` per number and halved
/// until the miss shrinks. Stops where the miss is under a ten-millionth of the frame or no step shrinks it.
fn solve(
    start: [f32; 4],
    step: [f32; 4],
    reach: [f32; 4],
    mut miss: impl FnMut([f32; 4]) -> Result<Option<[f32; 4]>, ShortError>,
) -> Result<[f32; 4], ShortError> {
    let mut v = start;
    let Some(first) = miss(v)? else { return Ok(v) };
    let mut r = Vec4::from_array(first);
    for _ in 0..40 {
        if r.length() < 1e-7 {
            break;
        }
        let mut cols = [Vec4::ZERO; 4];
        for (k, col) in cols.iter_mut().enumerate() {
            let mut probe = |h: f32| {
                let mut moved = v;
                moved[k] += h;
                miss(moved)
            };
            *col = match probe(step[k])? {
                Some(m) => (Vec4::from_array(m) - r) / step[k],
                None => match probe(-step[k])? {
                    Some(m) => (r - Vec4::from_array(m)) / step[k],
                    None => return Ok(v),
                },
            };
        }
        let jacobian = Mat4::from_cols(cols[0], cols[1], cols[2], cols[3]);
        if jacobian.determinant().abs() < 1e-14 {
            break;
        }
        let mut d = jacobian.inverse() * -r;
        if !d.is_finite() {
            break;
        }
        d /= (0..4).map(|k| d[k].abs() / reach[k]).fold(1.0_f32, f32::max);
        let mut taken = false;
        let mut length = 1.0;
        while length > 1.0 / 256.0 {
            let mut next = v;
            for (k, value) in next.iter_mut().enumerate() {
                *value += d[k] * length;
            }
            if let Some(m) = miss(next)?
                && Vec4::from_array(m).length() < r.length()
            {
                v = next;
                r = Vec4::from_array(m);
                taken = true;
                break;
            }
            length *= 0.5;
        }
        if !taken {
            break;
        }
    }
    Ok(v)
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

    fn rig(face: [Vec3; 3]) -> CameraRig {
        use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, RegistryBuilder};
        omuzaki_stage::link();
        let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
        let built = RegistryBuilder
            .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
            .expect("the mock character builds");
        let landmarks = Landmarks::measure(&built).unwrap();
        CameraRig { landmarks, root: Mat4::IDENTITY, aspect: 9.0 / 16.0, seed: 3, face: Some(face) }
    }

    #[test]
    fn a_shot_lands_the_eyes_and_the_mouth_on_their_marks() {
        let probe = rig([Vec3::ZERO; 3]);
        let centre = probe.landmarks.head.expect("the mock has a head").centre();
        let face = [
            centre + Vec3::new(-0.03, 0.01, 0.08),
            centre + Vec3::new(0.03, 0.01, 0.08),
            centre + Vec3::new(0.0, -0.05, 0.09),
        ];
        let rig = rig(face);
        let short = r#"
character = "mock"
handheld = 0.0
[voice]
kind = "silent"
[[line]]
text = "Here I am, tilted."
camera = { eyes = [[0.3, 0.44], [0.62, 0.4]] }
[[line]]
text = "And now only my mouth shows."
camera = { head_width = 2.4, mouth = [0.45, 0.2] }
"#;
        let def = crate::ShortDef::from_toml(short).unwrap();
        let voice =
            crate::voice::voice_for(&def, |_| unreachable!("silent"), crate::voice::Synthesis::Estimate).unwrap();
        let timeline = Timeline::new(&def, &voice).unwrap();
        let first = timeline.shots.iter().find(|s| s.shot.eyes.is_some()).expect("the eyes shot").start + 0.1;
        let camera = rig.camera_at(&timeline, first).unwrap();
        for (eye, mark) in face.iter().zip([[0.3, 0.44], [0.62, 0.4]]) {
            let landed = rig.land(&camera, *eye).unwrap();
            // A ten-thousandth of the frame's height: a fifth of a pixel on a 1920-high frame.
            assert!(landed.distance(rig.mark(mark)) < 1e-4, "{landed} against {}", rig.mark(mark));
        }
        let second = timeline.shots.iter().find(|s| s.shot.mouth.is_some()).expect("the mouth shot").start + 0.1;
        let camera = rig.camera_at(&timeline, second).unwrap();
        let landed = rig.land(&camera, face[2]).unwrap();
        assert!(landed.distance(rig.mark([0.45, 0.2])) < 1e-4, "{landed}");
        let head = rig.landmarks.head.unwrap();
        let share = head_share(&camera, rig.aspect, head);
        assert!((share - 2.4).abs() < 0.05 * 2.4, "the head width still sizes a mouth shot: {share}");
    }

    #[test]
    fn eyes_on_the_same_mark_or_far_off_the_frame_are_refused() {
        let read = |camera: &str| {
            crate::ShortDef::from_toml(&format!(
                "character = \"mock\"\n[voice]\nkind = \"silent\"\n[[line]]\ntext = \"Hi.\"\ncamera = {camera}\n"
            ))
        };
        assert!(read("{ eyes = [[0.4, 0.4], [0.6, 0.4]] }").is_ok());
        assert!(read("{ eyes = [[0.4, 0.4], [0.4, 0.4]] }").is_err());
        assert!(read("{ mouth = [0.5, 2.0] }").is_err());
    }
}
