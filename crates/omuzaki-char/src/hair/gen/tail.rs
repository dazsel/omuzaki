//! Ponytail and twintail generator: a bunch of clumps gathered at a tie on the head, then fanning out and drooping
//! under gravity. A new design in the prototype's clump language (the prototype has no tail); twintails are a tail
//! group with [`HairGroup::mirror`](omuzaki_char_api::HairGroup::mirror) set, so the second tail is the exact mirror image.
//!
//! **The axis.** The tail leaves the head at [`TailGen::root`] along the surface normal and runs straight through the
//! tie neck ([`TIE_LENGTH`]), where the tie band sits (15 to 20 mm off the scalp with the presets' root offset). Then it
//! bends within [`BEND_LENGTH`] to hang: the direction at fraction `s` is `lerp(outward, hang, droop * smoothstep(s, tie,
//! bend_done))`, with `hang` straight down tilted by [`TailGen::lean`] toward the head's vertical axis (positive: the
//! tail hugs the back of the neck) or away from it (negative: twintails swinging out). The axis is integrated in
//! [`AXIS_SAMPLES`] equal steps and carries a rotation-minimising frame (parallel transport), so the bunch never twists
//! at the bend. Lengths marked "scaled" below are for a head of crown radius [`REFERENCE_HEAD_RADIUS`] and grow with the
//! head's own.
//!
//! **The bunch.** Clump `i` of `n` sits at `(sqrt((i + 0.5) / n), i * golden angle)` in the bunch's cross-section (a
//! sunflower pattern: even for any count), scaled by the bunch radius `W(s)`: [`ROOT_RADIUS`] (scaled) where the tail
//! leaves the scalp, [`TailGen::tie_radius`] (scaled) at the tie, then widening by `tan(spread / 2)` per metre of
//! length. The locks are broad lens tubes ([`TailGen::width`], scaled) that overlap near the tie, so the tail reads as
//! one mass. A clump's length is [`TailGen::length`] shortened by up to [`LENGTH_JITTER`] on a fixed pattern, so the
//! tips are ragged.
//!
//! **Sway.** The [`Wave`] makes a gentle S: sideways along the frame's `u` and, at [`DEPTH_SWAY`]
//! of the amplitude and a quarter turn later, front to back along `v` (what the side view shows). It is
//! `amplitude * grow(s) * sin(2 pi frequency s + phase + i * PHASE_STEP)`, growing in over `growth`, and `curl` bends the
//! tip (squared smoothstep over the last [`CURL_FROM`]..1).
//!
//! **Gather locks.** [`TailGen::gather`] broad flat locks (after the tail's own) sweep from the back and side hairline
//! along the scalp, a few millimetres off it, and rise into the middle of the tie, so a style with a cap reads as hair
//! swept up into the gather ([`gather_locks`]); the presets have them, and `0` turns them off.
//!
//! **The tie.** One [`Tie`] torus round the gather at the tie, hiding where the locks start; the part draws it in its own
//! material.
//!
//! **Clearance.** Every rest point stands at least `CLEARANCE` off the head ([`Root::keep_outside`]).
//!
//! **Anchors, rig and skin.** [`anchors`] gives the tail's root frame for [`AnchorsFact`](omuzaki_char_api::AnchorsFact)
//! and [`rig`] its spring chain; the hair part publishes both and skins the locks to the chain's bones ([`skin`]): by arc
//! length between neighbouring bones, with the first [`ROOT_BAND`] (scaled) blending to the head.
//!
//! **Braids.** With [`TailGen::braid`] the tail is a plait instead of a loose bunch: [`TailBraid::strands`] strands
//! follow the axis, each on a figure of eight about it (`sin phi` across the plait's face, `sin 2 phi` through it, a
//! `1 / strands` turn behind its neighbour), so each passes over and under the others in turn, as the braid crown's do.
//! The plait swells out of the tie, narrows toward an end band and ends in a loose tuft of
//! [`TailBraid::tuft_locks`] locks. The axis, the chain, the anchors and the skin are the tail's, so a braid moves on
//! the same springs. [`side_braids`] and [`back_braid`] are the presets.
//!
//! **The body.** Where the build has a body, the axis keeps the bunch (or the plait) clear of it: a step that would
//! enter it turns to run along its surface, so a lock brought forward ([`TailGen::forward`]) drapes over the shoulder.
//!
//! The generator draws no random numbers.

use std::f64::consts::{PI, TAU};

use omuzaki_anim_api::{HumanoidBone, SpringChain, SpringParams, Transform};
use omuzaki_char_api::{
    Anchor, BodyFit, BoneName, Bulge, ExtraBone, HeadSurface, JointRef, RigContribution, TailBraid, TailGen, Taper,
    TaperShape, Wave, humanoid_id,
};
use omuzaki_core::glam::{DQuat, DVec3, Quat, Vec3};
use omuzaki_core::id::SlugError;

use super::cap::smoothstep;
use super::curtain::BodyKeepOut;
pub use super::root::REFERENCE_HEAD_RADIUS;
use super::root::{GATHER_SHADE, GATHER_THICKNESS, Root, gather_locks, gather_width, head_scale, sane};
use crate::hair::compile::{CompiledGroup, GroupGeometry, HairGenCtx};
use crate::hair::tie::Tie;
use crate::kit::catmull::spaced_points_into;

/// Points per clump after the resample.
pub const REST_POINTS: usize = 28;
/// Samples along the axis (a multiple of 2, 3, 4 and 5, so every chain length lands on a sample).
pub const AXIS_SAMPLES: usize = 61;
/// Length of the straight tie neck, from the root to where the tail starts to bend (scaled), metres.
pub const TIE_LENGTH: f64 = 0.008;
/// Length over which the tail bends from leaving the head to hanging (scaled), metres.
pub const BEND_LENGTH: f64 = 0.035;
/// Radius of the bunch where it leaves the scalp (scaled), metres.
pub const ROOT_RADIUS: f64 = 0.012;
/// Length from the first bone over which the locks blend from the head to the chain (scaled), metres.
pub const ROOT_BAND: f64 = 0.03;
/// The most a clump is shortened, as a fraction of the length.
pub const LENGTH_JITTER: f64 = 0.14;
/// Phase added to the wave per clump index, radians: small, so neighbouring locks sway together.
pub const PHASE_STEP: f64 = 0.25;
/// The front-to-back sway, as a fraction of the sideways one.
pub const DEPTH_SWAY: f64 = 0.7;
/// Where along the tail the curl starts.
pub const CURL_FROM: f64 = 0.78;
/// The golden angle, radians: successive clumps are this far round the bunch.
const GOLDEN_ANGLE: f64 = PI * (3.0 - 2.236_067_977_499_79);
/// The root used for a coordinate of [`TailGen::root`] that is not finite (the default's).
const DEFAULT_ROOT: [f64; 3] = [80.0, 150.0, 0.03];
/// Shade of the clump with jitter 0; each clump is up to [`SHADE_RANGE`] darker.
const SHADE: f64 = 1.0;
/// How much darker the darkest clump is.
const SHADE_RANGE: f64 = 0.06;
/// The share of the first lock's width that the tie ring stands out from the bunch.
const TIE_STAND_OFF: f64 = 0.45;
/// The tie's tube radius, as a fraction of the lock width at the tie.
const TIE_TUBE: f64 = 0.22;
/// The spring chain's parameters at the root (stiff: the tie holds) and at the tip (loose).
const CHAIN_ROOT: SpringParams =
    SpringParams { hit_radius: 0.02, stiffness: 1.0, gravity_power: 0.3, gravity_dir: Vec3::NEG_Y, drag_force: 0.4 };
const CHAIN_TIP: SpringParams =
    SpringParams { hit_radius: 0.02, stiffness: 0.5, gravity_power: 0.5, gravity_dir: Vec3::NEG_Y, drag_force: 0.3 };

/// The ponytail preset: one mass of broad locks tied high on the back of the head, hanging close to the neck.
pub fn ponytail() -> TailGen {
    TailGen {
        root: [72.0, 180.0, 0.010],
        length: 0.40,
        count: 8,
        spread_deg: 12.0,
        droop: 1.0,
        lean: 0.3,
        gather: 6,
        wave: Wave { amplitude: 0.012, frequency: 1.0, ..Wave::default() },
        width: Taper { start: 0.030, mid: 0.036, tip: 0.005, tip_at: 0.55, ..Taper::default() },
        ..TailGen::default()
    }
}

/// The twintails preset: two tails tied behind and above the ears, hanging past the shoulders with a slight outward
/// bulge. Set the group's `mirror`.
pub fn twintails() -> TailGen {
    TailGen {
        root: [68.0, 118.0, 0.010],
        length: 0.46,
        count: 6,
        spread_deg: 12.0,
        droop: 1.0,
        lean: -0.12,
        gather: 5,
        wave: Wave { amplitude: 0.010, frequency: 1.0, ..Wave::default() },
        width: Taper { start: 0.030, mid: 0.036, tip: 0.005, tip_at: 0.55, ..Taper::default() },
        ..TailGen::default()
    }
}

/// The side-braids preset: a braided lock behind each ear, falling in front of the shoulder with its flat face to the
/// front. Set the group's `mirror`.
pub fn side_braids() -> TailGen {
    TailGen {
        root: [100.0, 82.0, 0.012],
        length: 0.42,
        droop: 1.0,
        lean: -0.05,
        forward: 0.45,
        braid: Some(TailBraid { crossings: 6.0, width: 0.044, face_deg: 90.0, ..TailBraid::default() }),
        ..TailGen::default()
    }
}

/// The back-braid preset: one thick plait from the nape down the back, with gather locks swept into its top band.
pub fn back_braid() -> TailGen {
    TailGen {
        root: [110.0, 180.0, 0.010],
        length: 0.5,
        droop: 1.0,
        lean: 0.35,
        gather: 6,
        braid: Some(TailBraid { crossings: 7.0, width: 0.06, narrow: 0.4, ..TailBraid::default() }),
        ..TailGen::default()
    }
}

/// One frame of the axis: where it is and the two directions across it.
#[derive(Clone, Copy)]
struct Frame {
    p: DVec3,
    u: DVec3,
    v: DVec3,
}

/// The tail's parameters limited to what the maths needs, and its axis.
struct Shape {
    root: Root,
    /// The head's size against [`REFERENCE_HEAD_RADIUS`].
    scale: f64,
    length: f64,
    spread_tan: f64,
    tie_radius: f64,
    /// The fraction of the length where the tie ends and where the bend is done.
    tie_at: f64,
    frames: [Frame; AXIS_SAMPLES],
}

impl Shape {
    /// The tail's shape on `head`, kept off `body` where the build has one.
    fn new(g: &TailGen, head: &dyn HeadSurface, body: Option<&dyn BodyFit>) -> Self {
        let root = Root::new(g.root, DEFAULT_ROOT, head);
        let scale = head_scale(head);
        let length = sane(g.length, 0.05, 1.5, 0.4);
        let droop = sane(g.droop, 0.0, 1.0, 0.0);
        let lean = sane(g.lean, -1.0, 1.0, 0.0);
        let forward = sane(g.forward, -1.0, 1.0, 0.0);
        let spread_tan = (sane(g.spread_deg, 0.0, 120.0, 0.0).to_radians() / 2.0).tan();
        let tie_radius = sane(g.tie_radius, 0.002, 0.05, 0.008) * scale;
        let tie_at = (TIE_LENGTH * scale / length).min(0.5);
        let bend_done = ((TIE_LENGTH + BEND_LENGTH) * scale / length).min(1.0).max(tie_at + 1e-3);
        // A tail keeps its bunch (or its braid) clear of the body, with the outer locks' own width to spare.
        let keep_out = body.map(|fit| {
            let half = g.braid.as_ref().map_or(tie_radius + length * spread_tan, |b| braid_width(b, scale) * 0.5);
            (BodyKeepOut::new(fit), half + BODY_CLEARANCE)
        });
        let frames = axis(&root, length, [droop, lean, forward], [tie_at, bend_done], keep_out.as_ref());
        Self { root, scale, length, spread_tan, tie_radius, tie_at, frames }
    }

    /// The axis frame at fraction `s` of the length, between the samples.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small index
    fn frame_at(&self, s: f64) -> Frame {
        let x = s.clamp(0.0, 1.0) * (AXIS_SAMPLES - 1) as f64;
        let i = (x.floor() as usize).min(AXIS_SAMPLES - 2);
        let t = x - i as f64;
        let (a, b) = (self.frames[i], self.frames[i + 1]);
        Frame { p: a.p.lerp(b.p, t), u: a.u.lerp(b.u, t).normalize_or(a.u), v: a.v.lerp(b.v, t).normalize_or(a.v) }
    }

    /// The unit direction of the axis at fraction `s`.
    fn tangent_at(&self, s: f64) -> DVec3 {
        let (a, b) = (self.frame_at(s - 0.01).p, self.frame_at(s + 0.01).p);
        (b - a).normalize_or(DVec3::NEG_Y)
    }

    /// The radius of the bunch at fraction `s` of the length.
    fn bunch(&self, s: f64) -> f64 {
        if s < self.tie_at {
            ROOT_RADIUS * self.scale + (self.tie_radius - ROOT_RADIUS * self.scale) * smoothstep(s, 0.0, self.tie_at)
        } else {
            self.tie_radius + (s - self.tie_at) * self.length * self.spread_tan
        }
    }
}

/// How far the tail's outer locks keep off the body, beyond the bunch's own half width, metres.
const BODY_CLEARANCE: f64 = 0.012;

/// The axis from the root: [`AXIS_SAMPLES`] frames, equally spaced by arc length. `bend` is `[droop, lean, forward]`
/// and `at` the fractions where the bend starts and ends. With a `keep_out` (the body and the margin to keep), a step
/// that would enter the body turns to run along its surface instead, so the tail drapes over a shoulder.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // sample indices are small; margins in f32
fn axis(
    root: &Root,
    length: f64,
    bend: [f64; 3],
    at: [f64; 2],
    keep_out: Option<&(BodyKeepOut, f64)>,
) -> [Frame; AXIS_SAMPLES] {
    let start = root.outward;
    let inward = DVec3::new(-start.x, 0.0, -start.z).normalize_or_zero();
    let hang = (DVec3::NEG_Y + inward * bend[1] + DVec3::Z * bend[2]).normalize_or(DVec3::NEG_Y);
    let u0 = start.cross(DVec3::Y).try_normalize().unwrap_or(DVec3::X);
    let mut frame = Frame { p: root.point, u: u0, v: start.cross(u0) };
    let mut dir = start;
    let step = length / (AXIS_SAMPLES - 1) as f64;
    let mut frames = [frame; AXIS_SAMPLES];
    for (k, slot) in frames.iter_mut().enumerate() {
        *slot = frame;
        let s = k as f64 / (AXIS_SAMPLES - 1) as f64;
        let mut next = start.lerp(hang, bend[0] * smoothstep(s, at[0], at[1])).normalize_or(hang);
        if let Some((body, margin)) = keep_out {
            let ahead = (frame.p + next * step).as_vec3();
            let pushed = body.push_out(ahead, *margin as f32).as_dvec3();
            next = (pushed - frame.p).normalize_or(next);
        }
        let turn = DQuat::from_rotation_arc(dir, next);
        frame = Frame { p: frame.p + next * step, u: turn * frame.u, v: turn * frame.v };
        dir = next;
    }
    frames
}

/// Compiles the group: [`TailGen::count`] clumps and the tie that gathers them, or, for a braid, its strands, its tuft
/// and the two bands.
pub fn compile(g: &TailGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let shape = Shape::new(g, ctx.head, ctx.body);
    if let Some(braid) = &g.braid {
        return compile_braid(g, braid, &shape, ctx);
    }
    let width = Taper {
        start: g.width.start * shape.scale,
        mid: g.width.mid * shape.scale,
        tip: g.width.tip * shape.scale,
        ..g.width
    };
    let count = usize::from(g.count);
    let mut clumps: Vec<_> = (0..count)
        .map(|i| {
            let rest = clump(g, &shape, ctx.head, i, count);
            ctx.clump(rest, width, g.thickness, SHADE - SHADE_RANGE * jitter(i))
        })
        .collect();
    // The gather locks end in the middle of the tie.
    let end = shape.frames[tie_sample(&shape)].p;
    for rest in gather_locks(&shape.root, ctx.head, g.gather, end) {
        clumps.push(ctx.clump(rest, gather_width(shape.scale), GATHER_THICKNESS, GATHER_SHADE));
    }
    CompiledGroup { clumps, geometry: GroupGeometry::Tied { ties: vec![tie(&shape, width)] } }
}

/// The band round the gather, where the tie neck ends.
#[allow(clippy::cast_possible_truncation)] // positions are f32 downstream
fn tie(shape: &Shape, width: Taper) -> Tie {
    Tie {
        centre: shape.frames[tie_sample(shape)].p.as_vec3(),
        axis: shape.root.outward.as_vec3(),
        radius: (shape.tie_radius + TIE_STAND_OFF * width.start) as f32,
        tube: (TIE_TUBE * width.start) as f32,
    }
}

/// The axis sample where the tie sits.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // a small, non-negative index
fn tie_sample(shape: &Shape) -> usize {
    ((shape.tie_at * (AXIS_SAMPLES - 1) as f64).round() as usize).min(AXIS_SAMPLES - 1)
}

/// How many clumps [`compile`] makes: [`TailGen::count`] locks (for a braid, its strands and the tuft's locks), then
/// [`TailGen::gather`] gather locks.
pub fn clump_count(g: &TailGen) -> usize {
    let own = g.braid.as_ref().map_or(usize::from(g.count), |b| usize::from(b.strands) + usize::from(b.tuft_locks));
    own + usize::from(g.gather)
}

/// The fixed pattern in `0..1` that shortens and darkens clump `i`.
#[allow(clippy::cast_precision_loss)] // clump indices are small
fn jitter(i: usize) -> f64 {
    (i as f64 * 0.618_033_988_749_894_8).fract()
}

/// Clump `i` of `n`: [`REST_POINTS`] rest points, root first.
#[allow(clippy::cast_precision_loss)] // indices are small
fn clump(g: &TailGen, shape: &Shape, head: &dyn HeadSurface, index: usize, count: usize) -> Vec<Vec3> {
    let radius = ((index as f64 + 0.5) / count as f64).sqrt();
    let angle = index as f64 * GOLDEN_ANGLE;
    let (cu, cv) = (radius * angle.cos(), radius * angle.sin());
    let scale = 1.0 - LENGTH_JITTER * jitter(index);
    let wave = &g.wave;
    let amplitude = sane(wave.amplitude, 0.0, 0.2, 0.0);
    let frequency = sane(wave.frequency, 0.0, 12.0, 0.0);
    let phase = sane(wave.phase, -7.0, 7.0, 0.0) + index as f64 * PHASE_STEP;
    let curl = sane(wave.curl, -0.2, 0.2, 0.0);
    let (g0, g1) = (sane(wave.growth[0], 0.0, 1.0, 0.0), sane(wave.growth[1], 0.0, 1.0, 1.0));
    let root = shape.root.point;
    let mut points = [DVec3::ZERO; AXIS_SAMPLES];
    for (k, (point, frame)) in points.iter_mut().zip(&shape.frames).enumerate() {
        let along = k as f64 / (AXIS_SAMPLES - 1) as f64;
        let grow = amplitude * smoothstep(along, g0, g1);
        let arg = TAU * frequency * along + phase;
        let sideways = grow * arg.sin() + curl * smoothstep(along, CURL_FROM, 1.0).powi(2);
        let depth = grow * DEPTH_SWAY * (arg + PI / 2.0).sin();
        let on_axis = root + (frame.p - root) * scale;
        let p = on_axis + (frame.u * cu + frame.v * cv) * shape.bunch(along) + frame.u * sideways + frame.v * depth;
        *point = shape.root.keep_outside(head, p);
    }
    let mut spaced = [DVec3::ZERO; REST_POINTS];
    // The resample refuses only a non-finite or zero-length line, which the clamps above rule out; if it ever does,
    // the clump hangs straight down so the count the dispatch checks never changes.
    if spaced_points_into(&points, &mut spaced).is_err() {
        straight_down(shape, &mut spaced);
    }
    // The resample cuts corners, so the clearance is applied again to what is kept.
    spaced.iter().map(|p| shape.root.keep_outside(head, *p).as_vec3()).collect()
}

/// A line from the root straight down, [`REST_POINTS`] points long.
#[allow(clippy::cast_precision_loss)] // point indices are small
fn straight_down(shape: &Shape, out: &mut [DVec3; REST_POINTS]) {
    for (k, p) in out.iter_mut().enumerate() {
        *p = shape.root.point + DVec3::NEG_Y * (shape.length * k as f64 / (REST_POINTS - 1) as f64);
    }
}

/// The names and frames of the anchors a tail publishes, for a head that is `head`: `tail_root` (where the tail leaves the head,
/// +Y along the tail's first direction). A mirrored group publishes `tail_root_l` (the +x tail) and `tail_root_r`.
pub fn anchors(g: &TailGen, head: &dyn HeadSurface, mirror: bool) -> Vec<(String, Anchor)> {
    let shape = Shape::new(g, head, None);
    let root = Anchor {
        bone: JointRef::HEAD,
        position: shape.root.point.as_vec3(),
        rotation: Quat::from_rotation_arc(Vec3::Y, shape.root.outward.as_vec3()),
    };
    sides(mirror, head)
        .into_iter()
        .map(|(suffix, flip)| (format!("tail_root{suffix}"), mirrored(&root, flip)))
        .collect()
}

// ------------------------------------------------------------------------------------------------ braids

/// Points along a braid strand, root to the end band.
pub const BRAID_POINTS: usize = 140;
/// Points along a lock of a braid's tuft.
const TUFT_POINTS: usize = 16;
/// The shade of each strand of a braid, in turn, so the strands read apart where they cross.
const STRAND_SHADES: [f64; 5] = [1.0, 0.9, 0.95, 0.86, 0.97];
/// Where the braid reaches its full width, past the top band, as a fraction of the braided length.
const BRAID_SWELL: f64 = 0.06;
/// The braid's width at the top band and at the end band, against its full width there.
const BRAID_BAND_WIDTH: [f64; 2] = [0.45, 0.6];
/// A strand's width against the braid's: wide enough that neighbouring strands overlap as they cross.
const STRAND_WIDTH: f64 = 0.62;
/// How far a strand swings across the braid, against the braid's half width less the strand's own.
const SWING: f64 = 1.0;
/// The end band's ring and tube, against the braid's width there.
const END_BAND: (f64, f64) = (0.42, 0.16);

/// The braid's full width on a head of size `scale`, metres.
fn braid_width(b: &TailBraid, scale: f64) -> f64 {
    sane(b.width, 0.005, 0.2, 0.034) * scale
}

/// The braid's parameters limited to what the maths needs, along a tail's [`Shape`].
struct Plait {
    strands: u8,
    crossings: f64,
    width: f64,
    narrow: f64,
    depth: f64,
    thickness: f64,
    /// Where the braid starts (the top band) and ends (the end band), as fractions of the tail's length.
    from: f64,
    to: f64,
    face: f64,
}

impl Plait {
    fn new(b: &TailBraid, shape: &Shape) -> Self {
        let from = shape.tie_at;
        let to = (1.0 - sane(b.tuft, 0.0, 0.6, 0.16)).max(from + 0.05);
        Self {
            strands: b.strands.clamp(2, 5),
            crossings: sane(b.crossings, 1.0, 40.0, 7.0),
            width: braid_width(b, shape.scale),
            narrow: sane(b.narrow, 0.0, 0.9, 0.35),
            depth: sane(b.depth, 0.1, 1.5, 0.55),
            thickness: sane(b.strand_thickness, 0.1, 1.0, 0.55),
            from,
            to,
            face: sane(b.face_deg, -360.0, 360.0, 0.0).to_radians(),
        }
    }

    /// The braid's width at fraction `s` of the tail: swelling out of the top band, narrowing toward the end band.
    fn width_at(&self, s: f64) -> f64 {
        let along = ((s - self.from) / (self.to - self.from)).clamp(0.0, 1.0);
        let swell = BRAID_BAND_WIDTH[0] + (1.0 - BRAID_BAND_WIDTH[0]) * smoothstep(along, 0.0, BRAID_SWELL);
        let gather = 1.0 - (1.0 - BRAID_BAND_WIDTH[1]) * smoothstep(along, 1.0 - BRAID_SWELL, 1.0);
        self.width * (1.0 - self.narrow * along) * swell * gather
    }

    /// The two directions across the axis at `frame` the plait swings in: `a` across its flat face and `b` through it.
    fn axes(&self, frame: &Frame) -> (DVec3, DVec3) {
        let (sin, cos) = self.face.sin_cos();
        (frame.u * cos + frame.v * sin, frame.v * cos - frame.u * sin)
    }

    /// Strand `k`'s point at fraction `s` of the tail. Above the top band the strands converge from the root's bunch;
    /// from it to the end band each follows the plait's figure of eight, `sin phi` across the face and `sin 2 phi`
    /// through it, a third of a turn (for three strands) behind its neighbour, so every strand passes over and under
    /// the others in turn.
    fn strand_point(&self, shape: &Shape, k: u8, s: f64) -> DVec3 {
        let frame = shape.frame_at(s);
        let (a, b) = self.axes(&frame);
        let phase = TAU * f64::from(k) / f64::from(self.strands);
        if s < self.from {
            let r = shape.bunch(s) * 0.6;
            return frame.p + (a * phase.cos() + b * phase.sin()) * r;
        }
        let along = ((s - self.from) / (self.to - self.from)).clamp(0.0, 1.0);
        let phi = TAU * self.crossings * along + phase;
        let half = 0.5 * self.width_at(s);
        let swing = (half - 0.5 * STRAND_WIDTH * half) * SWING;
        // The swing grows in out of the band and closes into the end band, where the strands are gathered.
        let open = smoothstep(along, 0.0, BRAID_SWELL) * (1.0 - smoothstep(along, 1.0 - BRAID_SWELL * 0.5, 1.0) * 0.6);
        frame.p + a * (swing * phi.sin() * open) + b * (half * self.depth * 0.5 * (2.0 * phi).sin() * open)
    }
}

/// Compiles a braided tail: the strands from the root to the end band, the tuft's locks below it, the gather locks,
/// and the two bands.
#[allow(clippy::cast_precision_loss)] // point indices are small
fn compile_braid(g: &TailGen, b: &TailBraid, shape: &Shape, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let plait = Plait::new(b, shape);
    let strand_width = STRAND_WIDTH * plait.width;
    let width = Taper {
        start: strand_width * BRAID_BAND_WIDTH[0],
        mid: strand_width,
        tip: strand_width * BRAID_BAND_WIDTH[1] * (1.0 - plait.narrow),
        tip_at: 0.5,
        shape: TaperShape::Linear,
    };
    let thickness = Bulge { base: 0.5 * plait.thickness * strand_width, swell: 0.0 };
    let mut clumps = Vec::with_capacity(clump_count(g));
    for k in 0..plait.strands {
        let rest = (0..BRAID_POINTS)
            .map(|i| {
                let s = plait.to * i as f64 / (BRAID_POINTS - 1) as f64;
                shape.root.keep_outside(ctx.head, plait.strand_point(shape, k, s)).as_vec3()
            })
            .collect();
        let shade = STRAND_SHADES[usize::from(k) % STRAND_SHADES.len()];
        clumps.push(ctx.clump(rest, width, thickness, shade));
    }
    let end_width = plait.width_at(plait.to);
    let tuft_width = Taper {
        start: 0.55 * end_width,
        mid: 0.6 * end_width,
        tip: 0.08 * end_width,
        tip_at: 0.3,
        shape: TaperShape::Linear,
    };
    for i in 0..usize::from(b.tuft_locks) {
        let rest = tuft_lock(g, &plait, shape, i, usize::from(b.tuft_locks));
        clumps.push(ctx.clump(rest, tuft_width, g.thickness, SHADE - SHADE_RANGE * jitter(i)));
    }
    let end = shape.frames[tie_sample(shape)].p;
    for rest in gather_locks(&shape.root, ctx.head, g.gather, end) {
        clumps.push(ctx.clump(rest, gather_width(shape.scale), GATHER_THICKNESS, GATHER_SHADE));
    }
    let top = tie(shape, Taper { start: strand_width, ..width });
    let band = Tie {
        centre: shape.frame_at(plait.to).p.as_vec3(),
        axis: shape.tangent_at(plait.to).as_vec3(),
        #[allow(clippy::cast_possible_truncation)] // a small length
        radius: (END_BAND.0 * end_width) as f32,
        #[allow(clippy::cast_possible_truncation)] // a small length
        tube: (END_BAND.1 * end_width) as f32,
    };
    CompiledGroup { clumps, geometry: GroupGeometry::Tied { ties: vec![top, band] } }
}

/// Lock `i` of `n` of a braid's tuft: from the end band to the tail's tip, fanning out as a loose tail does.
#[allow(clippy::cast_precision_loss)] // indices are small
fn tuft_lock(g: &TailGen, plait: &Plait, shape: &Shape, i: usize, n: usize) -> Vec<Vec3> {
    let radius = ((i as f64 + 0.5) / n as f64).sqrt();
    let angle = i as f64 * GOLDEN_ANGLE;
    let scale = 1.0 - LENGTH_JITTER * jitter(i);
    let end_half = 0.5 * plait.width_at(plait.to);
    let wave = &g.wave;
    let amplitude = sane(wave.amplitude, 0.0, 0.2, 0.0) * 0.3;
    (0..TUFT_POINTS)
        .map(|k| {
            let t = k as f64 / (TUFT_POINTS - 1) as f64;
            let s = plait.to + (1.0 - plait.to) * t * scale;
            let frame = shape.frame_at(s);
            let (a, b) = plait.axes(&frame);
            let spread = end_half * 0.35 + (s - plait.to) * shape.length * shape.spread_tan.max(0.12);
            let sway = amplitude * (TAU * t + i as f64 * PHASE_STEP).sin() * t;
            let p = frame.p + (a * angle.cos() + b * angle.sin() * plait.depth) * radius * spread + a * sway;
            p.as_vec3()
        })
        .collect()
}

/// One side's spring chain: bone names and rest positions, root first.
#[derive(Clone, Debug, PartialEq)]
pub struct Chain {
    /// The chain's name (`prefix` with the side's `_l` or `_r`).
    pub name: String,
    /// The bones' names, root first.
    pub bones: Vec<BoneName>,
    /// Where each bone rests, bind space.
    pub rest: Vec<Vec3>,
    /// Length from the first bone over which the locks blend from the head to the chain, metres.
    pub root_band: f32,
    /// The spring parameters at the root and the tip, tapered between.
    pub springs: [SpringParams; 2],
}

/// The chains of a tail group: one per side (`_l` as authored, then `_r` for a mirrored group), each
/// [`TailGen::chain_bones`] bones from the root along the axis (kept off `body` as the locks are). `prefix` starts every
/// bone and chain name.
///
/// # Errors
/// [`SlugError`] if `prefix` makes a bone name that is not a valid name.
pub fn chains(
    g: &TailGen,
    head: &dyn HeadSurface,
    body: Option<&dyn BodyFit>,
    prefix: &str,
    mirror: bool,
) -> Result<Vec<Chain>, SlugError> {
    let shape = Shape::new(g, head, body);
    let bones = usize::from(g.chain_bones.clamp(MIN_CHAIN, MAX_CHAIN));
    let mut out = Vec::new();
    for (suffix, flip) in sides(mirror, head) {
        let mut names = Vec::with_capacity(bones);
        let mut rest = Vec::with_capacity(bones);
        for i in 0..bones {
            let sample = i * (AXIS_SAMPLES - 1) / (bones - 1);
            let mut at = shape.root.keep_outside(head, shape.frames[sample].p).as_vec3();
            if let Some(mid_x) = flip {
                at.x = 2.0 * mid_x - at.x;
            }
            names.push(BoneName::new(format!("{prefix}{suffix}_{i}"))?);
            rest.push(at);
        }
        out.push(Chain {
            name: format!("{prefix}{suffix}"),
            bones: names,
            rest,
            #[allow(clippy::cast_possible_truncation)] // a small length
            root_band: (ROOT_BAND * shape.scale) as f32,
            springs: [CHAIN_ROOT, CHAIN_TIP],
        });
    }
    Ok(out)
}

/// Adds the tail's spring chains to `out` (see [`chains`]): each bone the child of the one before, the first the child
/// of the head bone, and one chain per side over them that collides with `colliders` (collider group names).
///
/// # Errors
/// [`SlugError`] if `prefix` makes a bone name that is not a valid name.
pub fn rig(
    g: &TailGen,
    head: &dyn HeadSurface,
    body: Option<&dyn BodyFit>,
    prefix: &str,
    mirror: bool,
    colliders: &[String],
    out: &mut RigContribution,
) -> Result<(), SlugError> {
    add_chains(chains(g, head, body, prefix, mirror)?, colliders, out);
    Ok(())
}

/// Adds `chains` to `out` as spring chains: each bone the child of the one before, the first the child of the head
/// bone, colliding with `colliders` (collider group names). A tail's chains and, built for export, the simulated hair's
/// strand chains both go through here.
pub fn add_chains(chains: Vec<Chain>, colliders: &[String], out: &mut RigContribution) {
    for chain in chains {
        let mut ids = Vec::with_capacity(chain.bones.len());
        let mut parent = JointRef::HEAD;
        for (bone, at) in chain.bones.iter().zip(&chain.rest) {
            ids.push(out.add_bone(ExtraBone { name: bone.clone(), parent, rest: Transform::from_translation(*at) }));
            parent = JointRef::Named(bone.clone());
        }
        let [root, tip] = chain.springs;
        let mut spring = SpringChain::tapered(chain.name, &ids, root, tip);
        spring.collider_groups = colliders.to_vec();
        spring.center = Some(humanoid_id(HumanoidBone::Head));
        out.chains.push(spring);
    }
}

/// How a vertex follows a chain: the share of the head, and the rest split between bone `bone` and the next.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Skin {
    /// Weight of the head bone.
    pub head: f32,
    /// The chain bone the vertex is nearest above; it blends with `bone + 1` by `along`.
    pub bone: usize,
    /// How far from `bone` toward `bone + 1`, `0..=1`.
    pub along: f32,
}

impl Skin {
    /// The weights of head, `bone` and `bone + 1`, which sum to 1.
    pub fn weights(self) -> [f32; 3] {
        let rest = 1.0 - self.head;
        [self.head, rest * (1.0 - self.along), rest * self.along]
    }
}

/// How the vertex at `p` follows `chain`: it is projected onto the chain's line to find its arc length between
/// neighbouring bones, and the first [`ROOT_BAND`] of the chain blends to the head (a vertex before the first bone is
/// the head's alone).
pub fn skin(chain: &Chain, p: Vec3) -> Skin {
    let mut best = (f32::MAX, 0, 0.0_f32, 0.0_f32);
    let mut arc = 0.0_f32;
    for (i, pair) in chain.rest.windows(2).enumerate() {
        let seg = pair[1] - pair[0];
        let length = seg.length();
        let t = if length > 0.0 { ((p - pair[0]).dot(seg) / (length * length)).clamp(0.0, 1.0) } else { 0.0 };
        let d = (p - (pair[0] + seg * t)).length_squared();
        if d < best.0 {
            best = (d, i, t, arc + t * length);
        }
        arc += length;
    }
    let (_, bone, along, at) = best;
    let band = chain.root_band.max(f32::EPSILON);
    let k = (at / band).clamp(0.0, 1.0);
    Skin { head: 1.0 - k * k * (3.0 - 2.0 * k), bone, along }
}

/// Fewest bones in a tail's chain (the contract's range).
pub const MIN_CHAIN: u8 = 4;
/// Most bones in a tail's chain.
pub const MAX_CHAIN: u8 = 6;

/// The sides a group makes: one unsuffixed unless it `mirror`s, then `_l` (as authored) and `_r` (reflected in the plane
/// `x = head centre x`, `Some(that x)`).
fn sides(mirror: bool, head: &dyn HeadSurface) -> Vec<(&'static str, Option<f32>)> {
    if mirror { vec![("_l", None), ("_r", Some(head.centre().x))] } else { vec![("", None)] }
}

/// `anchor` reflected in the plane `x = mid_x` (unchanged for `None`).
fn mirrored(anchor: &Anchor, mid_x: Option<f32>) -> Anchor {
    let Some(mid_x) = mid_x else { return anchor.clone() };
    let up = anchor.rotation * Vec3::Y;
    Anchor {
        bone: anchor.bone.clone(),
        position: Vec3::new(2.0 * mid_x - anchor.position.x, anchor.position.y, anchor.position.z),
        rotation: Quat::from_rotation_arc(Vec3::Y, Vec3::new(-up.x, up.y, up.z)),
    }
}
