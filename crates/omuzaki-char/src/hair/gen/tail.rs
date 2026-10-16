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
//! **Sway.** The [`Wave`](omuzaki_char_api::Wave) makes a gentle S: sideways along the frame's `u` and, at [`DEPTH_SWAY`]
//! of the amplitude and a quarter turn later, front to back along `v` (what the side view shows). It is
//! `amplitude * grow(s) * sin(2 pi frequency s + phase + i * PHASE_STEP)`, growing in over `growth`, and `curl` bends the
//! tip (squared smoothstep over the last [`CURL_FROM`]..1).
//!
//! **The tie.** One [`Tie`] torus round the gather at the tie, hiding where the locks start; the part draws it in its own
//! material.
//!
//! **Clearance.** Every rest point stands at least [`CLEARANCE`] off the head ([`Root::keep_outside`]).
//!
//! **Anchors, rig and skin.** [`anchors`] gives the tail's root frame for [`AnchorsFact`](omuzaki_char_api::AnchorsFact)
//! and [`rig`] its spring chain; the hair part publishes both and skins the locks to the chain's bones ([`skin`]): by arc
//! length between neighbouring bones, with the first [`ROOT_BAND`] (scaled) blending to the head.
//!
//! The generator draws no random numbers.

use std::f64::consts::{PI, TAU};

use omuzaki_anim_api::{HumanoidBone, SpringChain, SpringParams, Transform};
use omuzaki_char_api::{
    Anchor, BoneName, ExtraBone, HeadSurface, JointRef, RigContribution, TailGen, Taper, Wave, humanoid_id,
};
use omuzaki_core::glam::{DQuat, DVec3, Quat, Vec3};
use omuzaki_core::id::SlugError;

use super::cap::smoothstep;
use super::root::{Root, sane};
use crate::hair::compile::{CompiledGroup, GroupGeometry, HairGenCtx};
use crate::hair::tie::Tie;
use crate::kit::catmull::spaced_points_into;

/// Points per clump after the resample.
pub const REST_POINTS: usize = 28;
/// Samples along the axis (a multiple of 2, 3, 4 and 5, so every chain length lands on a sample).
pub const AXIS_SAMPLES: usize = 61;
/// The crown radius of the head the lengths below are written for (the `standard` template's), metres.
pub const REFERENCE_HEAD_RADIUS: f64 = 0.1;
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
        wave: Wave { amplitude: 0.012, frequency: 1.0, ..Wave::default() },
        width: Taper { start: 0.030, mid: 0.036, tip: 0.005, tip_at: 0.55 },
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
        wave: Wave { amplitude: 0.010, frequency: 1.0, ..Wave::default() },
        width: Taper { start: 0.030, mid: 0.036, tip: 0.005, tip_at: 0.55 },
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
    fn new(g: &TailGen, head: &dyn HeadSurface) -> Self {
        let root = Root::new(g.root, DEFAULT_ROOT, head);
        let scale = (f64::from(head.radius(Vec3::Y)) / REFERENCE_HEAD_RADIUS).clamp(0.5, 2.0);
        let length = sane(g.length, 0.05, 1.5, 0.4);
        let droop = sane(g.droop, 0.0, 1.0, 0.0);
        let lean = sane(g.lean, -1.0, 1.0, 0.0);
        let spread_tan = (sane(g.spread_deg, 0.0, 120.0, 0.0).to_radians() / 2.0).tan();
        let tie_radius = sane(g.tie_radius, 0.002, 0.05, 0.008) * scale;
        let tie_at = (TIE_LENGTH * scale / length).min(0.5);
        let bend_done = ((TIE_LENGTH + BEND_LENGTH) * scale / length).min(1.0).max(tie_at + 1e-3);
        let frames = axis(&root, length, [droop, lean], [tie_at, bend_done]);
        Self { root, scale, length, spread_tan, tie_radius, tie_at, frames }
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

/// The axis from the root: [`AXIS_SAMPLES`] frames, equally spaced by arc length. `bend` is `[droop, lean]` and `at` the
/// fractions where the bend starts and ends.
#[allow(clippy::cast_precision_loss)] // sample indices are small
fn axis(root: &Root, length: f64, bend: [f64; 2], at: [f64; 2]) -> [Frame; AXIS_SAMPLES] {
    let start = root.outward;
    let inward = DVec3::new(-start.x, 0.0, -start.z).normalize_or_zero();
    let hang = (DVec3::NEG_Y + inward * bend[1]).normalize_or(DVec3::NEG_Y);
    let u0 = start.cross(DVec3::Y).try_normalize().unwrap_or(DVec3::X);
    let mut frame = Frame { p: root.point, u: u0, v: start.cross(u0) };
    let mut dir = start;
    let step = length / (AXIS_SAMPLES - 1) as f64;
    let mut frames = [frame; AXIS_SAMPLES];
    for (k, slot) in frames.iter_mut().enumerate() {
        *slot = frame;
        let s = k as f64 / (AXIS_SAMPLES - 1) as f64;
        let next = start.lerp(hang, bend[0] * smoothstep(s, at[0], at[1])).normalize_or(hang);
        let turn = DQuat::from_rotation_arc(dir, next);
        frame = Frame { p: frame.p + next * step, u: turn * frame.u, v: turn * frame.v };
        dir = next;
    }
    frames
}

/// Compiles the group: [`TailGen::count`] clumps and the tie that gathers them.
pub fn compile(g: &TailGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let shape = Shape::new(g, ctx.head);
    let width = Taper {
        start: g.width.start * shape.scale,
        mid: g.width.mid * shape.scale,
        tip: g.width.tip * shape.scale,
        tip_at: g.width.tip_at,
    };
    let count = usize::from(g.count);
    let clumps = (0..count)
        .map(|i| {
            let rest = clump(g, &shape, ctx.head, i, count);
            ctx.clump(rest, width, g.thickness, SHADE - SHADE_RANGE * jitter(i))
        })
        .collect();
    CompiledGroup { clumps, geometry: GroupGeometry::Tied { ties: vec![tie(&shape, width)] } }
}

/// The band round the gather, where the tie neck ends.
#[allow(clippy::cast_possible_truncation)] // positions are f32 downstream
fn tie(shape: &Shape, width: Taper) -> Tie {
    let at = (shape.tie_at * (AXIS_SAMPLES - 1) as f64).round() as usize;
    Tie {
        centre: shape.frames[at.min(AXIS_SAMPLES - 1)].p.as_vec3(),
        axis: shape.root.outward.as_vec3(),
        radius: (shape.tie_radius + TIE_STAND_OFF * width.start) as f32,
        tube: (TIE_TUBE * width.start) as f32,
    }
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
        let grow = amplitude * smoothstep(s, g0, g1);
        let arg = TAU * frequency * s + phase;
        let sideways = grow * arg.sin() + curl * smoothstep(s, CURL_FROM, 1.0).powi(2);
        let depth = grow * DEPTH_SWAY * (arg + PI / 2.0).sin();
        let on_axis = root + (f.p - root) * scale;
        let p = on_axis + (f.u * cu + f.v * cv) * shape.bunch(s) + f.u * sideways + f.v * depth;
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
}

/// The chains of a tail group: one per side (`_l` as authored, then `_r` for a mirrored group), each
/// [`TailGen::chain_bones`] bones from the root along the axis. `prefix` starts every bone and chain name.
///
/// # Errors
/// [`SlugError`] if `prefix` makes a bone name that is not a valid name.
pub fn chains(g: &TailGen, head: &dyn HeadSurface, prefix: &str, mirror: bool) -> Result<Vec<Chain>, SlugError> {
    let shape = Shape::new(g, head);
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
    prefix: &str,
    mirror: bool,
    colliders: &[String],
    out: &mut RigContribution,
) -> Result<(), SlugError> {
    for chain in chains(g, head, prefix, mirror)? {
        let mut ids = Vec::with_capacity(chain.bones.len());
        let mut parent = JointRef::HEAD;
        for (bone, at) in chain.bones.iter().zip(&chain.rest) {
            ids.push(out.add_bone(ExtraBone {
                name: bone.clone(),
                parent,
                rest: Transform::from_translation(*at),
            }));
            parent = JointRef::Named(bone.clone());
        }
        let mut spring = SpringChain::tapered(chain.name, &ids, CHAIN_ROOT, CHAIN_TIP);
        spring.collider_groups = colliders.to_vec();
        spring.center = Some(humanoid_id(HumanoidBone::Head));
        out.chains.push(spring);
    }
    Ok(())
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
