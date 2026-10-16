//! Ponytail and twintail generator: a bunch of clumps gathered at a tie on the head, then fanning out and drooping
//! under gravity. A new design in the prototype's clump language (the prototype has no tail); twintails are a tail
//! group with [`HairGroup::mirror`](omuzaki_char_api::HairGroup::mirror) set, so the second tail is the exact mirror image.
//!
//! **The axis.** The tail leaves the head at [`TailGen::root`] along the surface normal, runs straight through the
//! tie neck (the first [`TIE_AT`] of its length), then bends toward gravity: the direction at fraction `s` is
//! `lerp(outward, down, droop * smoothstep(s, TIE_AT, DROOP_DONE))`. The axis is integrated in [`AXIS_SAMPLES`] equal steps and
//! carries a rotation-minimising frame (parallel transport), so the bunch never twists at a bend.
//!
//! **The bunch.** Clump `i` of `n` sits at `(sqrt((i + 0.5) / n), i * golden angle)` in the bunch's cross-section (a
//! sunflower pattern: even for any count), scaled by the bunch radius `W(s)`: [`ROOT_RADIUS`] where the tail leaves
//! the scalp, [`TailGen::tie_radius`] at the tie, then widening by `tan(spread / 2)` per metre of length. A clump's
//! length is [`TailGen::length`] shortened by up to [`LENGTH_JITTER`] on a fixed pattern, so the tips are ragged.
//!
//! **Sway.** The [`Wave`](omuzaki_char_api::Wave) sways a clump sideways along the frame's `u`: `amplitude * grow(s) *
//! sin(2 pi frequency s + phase + i * PHASE_STEP)`, growing in over `growth`, and `curl` bends the tip (squared
//! smoothstep over the last [`CURL_FROM`]..1).
//!
//! **Clearance.** Every rest point stands at least [`CLEARANCE`] off the head ([`Root::keep_outside`]).
//!
//! **Anchors and rig.** [`anchors`] gives the tail's root frame for [`AnchorsFact`](omuzaki_char_api::AnchorsFact) and
//! [`rig`] its spring chain (the hair part publishes both). The chain follows the bunch's axis.
//!
//! The generator draws no random numbers.

use std::f64::consts::{PI, TAU};

use omuzaki_anim_api::{HumanoidBone, SpringChain, SpringParams, Transform};
use omuzaki_char_api::{
    Anchor, BoneName, ExtraBone, HeadSurface, JointRef, RigContribution, TailGen, humanoid_id,
};
use omuzaki_core::glam::{DQuat, DVec3, Quat, Vec3};
use omuzaki_core::id::SlugError;

use super::cap::smoothstep;
use super::root::{Root, sane};
use crate::hair::compile::{CompiledGroup, HairGenCtx};
use crate::kit::catmull::spaced_points_into;

/// Points per clump after the resample.
pub const REST_POINTS: usize = 24;
/// Samples along the axis (a multiple of 2, 3, 4 and 5, so every chain length lands on a sample).
pub const AXIS_SAMPLES: usize = 61;
/// Where the tie ends, as a fraction of the length: the tail runs straight until then.
pub const TIE_AT: f64 = 0.12;
/// Where along the tail the bend toward gravity is complete: the tail hangs from here on.
pub const DROOP_DONE: f64 = 0.6;
/// Radius of the bunch where it leaves the scalp, metres.
pub const ROOT_RADIUS: f64 = 0.03;
/// The most a clump is shortened, as a fraction of the length.
pub const LENGTH_JITTER: f64 = 0.14;
/// Phase added to the wave per clump index, radians.
pub const PHASE_STEP: f64 = 0.9;
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
/// The spring chain's parameters at the root (stiff: the tie holds) and at the tip (loose).
const CHAIN_ROOT: SpringParams =
    SpringParams { hit_radius: 0.02, stiffness: 1.0, gravity_power: 0.3, gravity_dir: Vec3::NEG_Y, drag_force: 0.4 };
const CHAIN_TIP: SpringParams =
    SpringParams { hit_radius: 0.02, stiffness: 0.5, gravity_power: 0.5, gravity_dir: Vec3::NEG_Y, drag_force: 0.3 };

/// The ponytail preset: a high tail behind the head, hanging back.
pub fn ponytail() -> TailGen {
    TailGen {
        root: [70.0, 180.0, 0.02],
        length: 0.4,
        count: 9,
        spread_deg: 16.0,
        droop: 0.85,
        wave: omuzaki_char_api::Wave { amplitude: 0.012, frequency: 1.5, ..omuzaki_char_api::Wave::default() },
        ..TailGen::default()
    }
}

/// The twintails preset: two tails from the sides of the head, swinging out over the shoulders. Set the group's `mirror`.
pub fn twintails() -> TailGen {
    TailGen {
        root: [78.0, 100.0, 0.02],
        length: 0.42,
        count: 7,
        spread_deg: 14.0,
        droop: 0.8,
        wave: omuzaki_char_api::Wave { amplitude: 0.01, frequency: 1.2, ..omuzaki_char_api::Wave::default() },
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
    length: f64,
    spread_tan: f64,
    tie_radius: f64,
    frames: [Frame; AXIS_SAMPLES],
}

impl Shape {
    fn new(g: &TailGen, head: &dyn HeadSurface) -> Self {
        let root = Root::new(g.root, DEFAULT_ROOT, head);
        let length = sane(g.length, 0.05, 1.5, 0.4);
        let droop = sane(g.droop, 0.0, 1.0, 0.0);
        let spread_tan = (sane(g.spread_deg, 0.0, 120.0, 0.0).to_radians() / 2.0).tan();
        let tie_radius = sane(g.tie_radius, 0.002, 0.05, 0.012);
        let frames = axis(&root, length, droop);
        Self { root, length, spread_tan, tie_radius, frames }
    }

    /// The radius of the bunch at fraction `s` of the length.
    fn bunch(&self, s: f64) -> f64 {
        if s < TIE_AT {
            ROOT_RADIUS + (self.tie_radius - ROOT_RADIUS) * smoothstep(s, 0.0, TIE_AT)
        } else {
            self.tie_radius + (s - TIE_AT) * self.length * self.spread_tan
        }
    }
}

/// The axis from the root: [`AXIS_SAMPLES`] frames, equally spaced by arc length.
#[allow(clippy::cast_precision_loss)] // sample indices are small
fn axis(root: &Root, length: f64, droop: f64) -> [Frame; AXIS_SAMPLES] {
    let start = root.outward;
    let u0 = start.cross(DVec3::Y).try_normalize().unwrap_or(DVec3::X);
    let mut frame = Frame { p: root.point, u: u0, v: start.cross(u0) };
    let mut dir = start;
    let step = length / (AXIS_SAMPLES - 1) as f64;
    let mut frames = [frame; AXIS_SAMPLES];
    for (k, slot) in frames.iter_mut().enumerate() {
        *slot = frame;
        let s = k as f64 / (AXIS_SAMPLES - 1) as f64;
        let next = start.lerp(DVec3::NEG_Y, droop * smoothstep(s, TIE_AT, DROOP_DONE)).normalize_or(DVec3::NEG_Y);
        let turn = DQuat::from_rotation_arc(dir, next);
        frame = Frame { p: frame.p + next * step, u: turn * frame.u, v: turn * frame.v };
        dir = next;
    }
    frames
}

/// Compiles the group: [`TailGen::count`] clumps.
pub fn compile(g: &TailGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let shape = Shape::new(g, ctx.head);
    let count = usize::from(g.count);
    let clumps = (0..count)
        .map(|i| {
            let rest = clump(g, &shape, ctx.head, i, count);
            let jitter = jitter(i);
            ctx.clump(rest, g.width, g.thickness, SHADE - SHADE_RANGE * jitter)
        })
        .collect();
    CompiledGroup::tubes(clumps)
}

/// How many clumps [`compile`] makes: [`TailGen::count`].
pub fn clump_count(g: &TailGen) -> usize {
    usize::from(g.count)
}

/// The fixed pattern in `0..1` that shortens and darkens clump `i`.
#[allow(clippy::cast_precision_loss)] // clump indices are small
fn jitter(i: usize) -> f64 {
    (i as f64 * 0.618_033_988_749_894_8).fract()
}

/// Clump `i` of `n`: [`REST_POINTS`] rest points, root first.
#[allow(clippy::cast_precision_loss)] // indices are small
fn clump(g: &TailGen, shape: &Shape, head: &dyn HeadSurface, i: usize, n: usize) -> Vec<Vec3> {
    let radius = ((i as f64 + 0.5) / n as f64).sqrt();
    let angle = i as f64 * GOLDEN_ANGLE;
    let (cu, cv) = (radius * angle.cos(), radius * angle.sin());
    let scale = 1.0 - LENGTH_JITTER * jitter(i);
    let wave = &g.wave;
    let amplitude = sane(wave.amplitude, 0.0, 0.2, 0.0);
    let frequency = sane(wave.frequency, 0.0, 12.0, 0.0);
    let phase = sane(wave.phase, -7.0, 7.0, 0.0) + i as f64 * PHASE_STEP;
    let curl = sane(wave.curl, -0.2, 0.2, 0.0);
    let (g0, g1) = (sane(wave.growth[0], 0.0, 1.0, 0.0), sane(wave.growth[1], 0.0, 1.0, 1.0));
    let root = shape.root.point;
    let mut points = [DVec3::ZERO; AXIS_SAMPLES];
    for (k, (point, f)) in points.iter_mut().zip(&shape.frames).enumerate() {
        let s = k as f64 / (AXIS_SAMPLES - 1) as f64;
        let across = amplitude * smoothstep(s, g0, g1) * (TAU * frequency * s + phase).sin()
            + curl * smoothstep(s, CURL_FROM, 1.0).powi(2);
        let on_axis = root + (f.p - root) * scale;
        let p = on_axis + (f.u * cu + f.v * cv) * shape.bunch(s) + f.u * across;
        *point = shape.root.keep_outside(head, p);
    }
    let mut spaced = [DVec3::ZERO; REST_POINTS];
    // The resample refuses only a non-finite or zero-length line, which the clamps above rule out; if it ever does,
    // the clump hangs straight down so the count the dispatch checks never changes.
    if spaced_points_into(&points, &mut spaced).is_err() {
        straight_down(shape, &mut spaced);
    }
    spaced.iter().map(|p| p.as_vec3()).collect()
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
    let shape = Shape::new(g, head);
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

/// Adds the tail's spring chain to `out`: [`TailGen::chain_bones`] bones from the root along the axis, each the child
/// of the one before, the first the child of the head bone, and one chain over them that collides with `colliders`
/// (collider group names). `prefix` starts every bone and chain name (the part and group, so names are unique in the
/// character); a mirrored group adds a chain per side with `_l` and `_r` segments.
///
/// # Errors
/// [`SlugError`] if `prefix` makes a bone name that is not a valid name.
pub fn rig(
    g: &TailGen,
    head: &dyn HeadSurface,
    prefix: &str,
    mirror: bool,
    colliders: &[String],
    out: &mut RigContribution,
) -> Result<(), SlugError> {
    let shape = Shape::new(g, head);
    let bones = usize::from(g.chain_bones.clamp(MIN_CHAIN, MAX_CHAIN));
    for (suffix, flip) in sides(mirror, head) {
        let name = |i: usize| BoneName::new(format!("{prefix}{suffix}_{i}"));
        let mut ids = Vec::with_capacity(bones);
        let mut parent = JointRef::HEAD;
        for i in 0..bones {
            let sample = i * (AXIS_SAMPLES - 1) / (bones - 1);
            let p = shape.root.keep_outside(head, shape.frames[sample].p);
            let mut at = p.as_vec3();
            if let Some(mid_x) = flip {
                at.x = 2.0 * mid_x - at.x;
            }
            let bone = name(i)?;
            ids.push(out.add_bone(ExtraBone { name: bone.clone(), parent, rest: Transform::from_translation(at) }));
            parent = JointRef::Named(bone);
        }
        let mut chain = SpringChain::tapered(format!("{prefix}{suffix}"), &ids, CHAIN_ROOT, CHAIN_TIP);
        chain.collider_groups = colliders.to_vec();
        chain.center = Some(humanoid_id(HumanoidBone::Head));
        out.chains.push(chain);
    }
    Ok(())
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
