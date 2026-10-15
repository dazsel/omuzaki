//! Bangs generator: the close-up fringe (port of `parts/hair.js`, lines 330 to 355).
//!
//! **One lock** ([`control_points`]) is a pointed clump of 11 control points resampled to [`REST_POINTS`] by the geometry
//! kit's Catmull-Rom. Its root sits [`ROOT_THETA_DEG`] from the top of the head; the polar angle falls to the tip angle
//! `acos(tip height / crown radius)` as `t^FALL_EXPONENT` (the crown radius is the head's radius straight up, as
//! [`part_sweep`](super::part_sweep) uses it, so a style keeps its shape when the head is scaled; the prototype uses one
//! fixed head radius). The azimuth is `phi * lerp(0.3, 1, ss(t, 0, 0.65)) - sweep * t^2 - u * hook * ss(t, 0.75, 1)`
//! with `u = phi / spread`, so the fringe sweeps to one side and each tip hooks inward. The distance from the head is
//! `lerp(root_offset, tip_offset, t) + 0.016 sin(pi min(1, 1.15 t))`.
//!
//! **Layers and wisps.** Layers build in [`BangsGen::layers`] order (under to over), then the wisps. The last layer is
//! the top layer: pointed clumps spread from `-spread` to `+spread` with a fixed jitter in position, width, shade and
//! hook, and its outer tips drop by [`TOP_DROP`]. Every earlier layer is an under-layer: broad, short clumps at the
//! centres of equal cells (the prototype's `(i + 0.5) / n`), tips dropping by [`UNDER_DROP`] at the outer clumps, no
//! jitter but in tip height. Each clump's tip height comes from `tip_heights` (the last value repeats). The jitter is the
//! prototype's fixed `hj(k) = 0.5 sin(12.9898 k + 1.7)`; the generator draws no random numbers, so the long hair's
//! stream is untouched.
//!
//! **Width.** The prototype's outline `lerp(0.040, w, ss(t, 0, 0.45)) (1 - ss(t, 0.5, 1))^0.7 + 0.0015` is not a
//! [`Taper`]. This uses the closest current `Taper`: [`ROOT_WIDTH`] at the root, `MID_GAIN w + FLOOR` at `TIP_AT`,
//! [`FLOOR`] at the tip. The exact outline waits for #842; [`outline`] is the prototype's formula, and the tests print the
//! largest difference between the two.

use omuzaki_char_api::{BangLayer, BangsGen, Bulge, HeadSurface, Taper, Wisp};
use omuzaki_core::glam::{DVec3, Vec3};

use super::cap::smoothstep;
use crate::hair::compile::{CompiledGroup, HairGenCtx};
use crate::kit::catmull::spaced_points_into;

/// Control points per lock, before the Catmull-Rom resample.
pub const CONTROL_POINTS: usize = 11;
/// Points per lock after the resample.
pub const REST_POINTS: usize = 22;
/// Polar angle of every root, degrees from the top of the head.
pub const ROOT_THETA_DEG: f64 = 14.0;
/// Exponent of the fall from the root angle to the tip angle.
pub const FALL_EXPONENT: f64 = 0.8;
/// How much of the azimuth the root keeps: the lock starts this fraction of the way to its tip azimuth.
pub const ROOT_AZIMUTH: f64 = 0.3;
/// Height of the bulge above the root-to-tip offset, metres.
pub const BELLY: f64 = 0.016;
/// Ratio of the belly's reach: it peaks and falls again at `t = 1 / BELLY_REACH`.
const BELLY_REACH: f64 = 1.15;
/// Top layer: the angular jitter of a clump's position, degrees (times `hj`).
pub const POSITION_JITTER_DEG: f64 = 3.0;
/// Top layer: how far the outer clumps' tips drop, metres (`ss(|u|, 0.5, 1)` of it).
pub const TOP_DROP: f64 = 0.045;
/// Top layer: the jitter of a tip's height, metres (times `hj`).
pub const TOP_TIP_JITTER: f64 = 0.004;
/// Top layer: the jitter of a clump's width, as a fraction of the width (the prototype's 0.014 of 0.064; times `hj`).
pub const WIDTH_JITTER: f64 = 0.014 / 0.064;
/// Top layer: shade lost by every other clump.
pub const SHADE_STEP: f64 = 0.035;
/// Under-layer: how far the outer clumps' tips drop, metres (`ss(|u|, 0.5, 1)` of it).
pub const UNDER_DROP: f64 = 0.03;
/// Under-layer: the jitter of a tip's height, metres (times `hj`).
pub const UNDER_TIP_JITTER: f64 = 0.006;
/// Wisps: distance from the head at the root, metres.
pub const WISP_ROOT_OFFSET: f64 = 0.048;
/// Wisps: distance from the head at the tip, metres.
pub const WISP_TIP_OFFSET: f64 = 0.010;
/// Wisps: shade.
pub const WISP_SHADE: f64 = 0.98;
// Offsets of the prototype's `hj(i + k)` calls (the top layer's position uses `hj(i)`).
const J_TOP_TIP: usize = 7;
const J_TOP_WIDTH: usize = 3;
const J_TOP_HOOK: usize = 11;
const J_UNDER_TIP: usize = 20;
/// Limits applied to the unvalidated parameters (the [`Taper`] validation's width range, and generous angle and offset
/// ranges): widths in metres, angles in degrees, offsets in metres.
const MIN_WIDTH: f64 = 0.001;
const MAX_WIDTH: f64 = 0.3;
const MAX_ANGLE_DEG: f64 = 90.0;
const MAX_OFFSET: f64 = 0.2;
/// The least distance from the skull, so a clump never sits in it.
const MIN_OFFSET: f64 = 0.002;
/// Thickness of every lock (the prototype's `0.005 + 0.005 sin(pi t)`).
pub const THICKNESS: Bulge = Bulge { base: 0.005, swell: 0.005 };
/// Width fit: the outline's width at the root, metres, including [`FLOOR`].
pub const ROOT_WIDTH: f64 = 0.040 + FLOOR;
/// Width fit: the thinnest the outline gets (the prototype's `+ 0.0015`), also the tip width.
pub const FLOOR: f64 = 0.0015;
/// Width fit: the widest part is `MID_GAIN` times the layer's width (plus [`FLOOR`]).
pub const MID_GAIN: f64 = 1.05;
/// Width fit: where along the lock the widest part sits.
pub const TIP_AT: f64 = 0.57;

/// `x` limited to `lo..=hi`, or `fallback` if it is not finite. The contract's validation covers the counts, spread and
/// shade only; the rest reaches the generator as it was typed, so it is checked where it is read.
fn sane(x: f64, lo: f64, hi: f64, fallback: f64) -> f64 {
    if x.is_finite() { x.clamp(lo, hi) } else { fallback }
}

/// The prototype's `hj(k)`: the fixed jitter in `-0.5..0.5` for index `k`.
fn hj(k: usize) -> f64 {
    #[allow(clippy::cast_precision_loss)] // k is a small clump index
    let k = k as f64;
    0.5 * (12.9898 * k + 1.7).sin()
}

/// The [`Taper`] that stands in for the prototype's outline of a lock of width `w`.
pub fn taper(w: f64) -> Taper {
    Taper { start: ROOT_WIDTH, mid: MID_GAIN * w + FLOOR, tip: FLOOR, tip_at: TIP_AT }
}

/// The prototype's outline of a lock of width `w` at `t`: `lerp(0.040, w, ss(t, 0, 0.45)) (1 - ss(t, 0.5, 1))^0.7 + 0.0015`.
pub fn outline(w: f64, t: f64) -> f64 {
    let base = 0.040 + (w - 0.040) * smoothstep(t, 0.0, 0.45);
    base * (1.0 - smoothstep(t, 0.5, 1.0)).powf(0.7) + FLOOR
}

/// One lock to build.
struct Lock {
    /// Azimuth of the tip, radians.
    phi: f64,
    /// Tip height above the head centre, metres.
    tip_height: f64,
    root_offset: f64,
    tip_offset: f64,
    width: f64,
    shade: f64,
    /// Inward hook of the tip, degrees.
    hook_deg: f64,
    /// `phi / spread`.
    u: f64,
}

/// Compiles the group: the layers under to over, then the wisps.
pub fn compile(g: &BangsGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let mut clumps = Vec::with_capacity(clump_count(g));
    let mut push = |lock: &Lock| {
        let rest = rest_points(g, ctx.head, lock);
        clumps.push(ctx.clump(rest.to_vec(), taper(lock.width), THICKNESS, lock.shade));
    };
    let last = g.layers.len().saturating_sub(1);
    for (n, layer) in g.layers.iter().enumerate() {
        for i in 0..usize::from(layer.count) {
            push(&layer_lock(layer, n == last, i));
        }
    }
    for w in &g.wisps {
        push(&wisp_lock(w));
    }
    CompiledGroup::tubes(clumps)
}

/// How many clumps [`compile`] makes: every layer's count plus the wisps.
pub fn clump_count(g: &BangsGen) -> usize {
    g.layers.iter().map(|l| usize::from(l.count)).sum::<usize>() + g.wisps.len()
}

/// The lock `i` of `layer`, which is the top layer if `top`.
#[allow(clippy::cast_precision_loss)] // clump indices are small
fn layer_lock(layer: &BangLayer, top: bool, i: usize) -> Lock {
    let n = usize::from(layer.count);
    let tip = sane(layer.tip_heights.get(i).or(layer.tip_heights.last()).copied().unwrap_or(0.0), -1.0, 1.0, 0.0);
    let spread = sane(layer.spread_deg, 5.0, 90.0, 44.0).to_radians();
    let (width, shade) = (sane(layer.width, MIN_WIDTH, MAX_WIDTH, 0.064), sane(layer.shade, 0.5, 1.0, 1.0));
    let (hook_deg, root_offset, tip_offset) = (
        sane(layer.hook_deg, -MAX_ANGLE_DEG, MAX_ANGLE_DEG, 0.0),
        sane(layer.root_offset, MIN_OFFSET, MAX_OFFSET, 0.046),
        sane(layer.tip_offset, MIN_OFFSET, MAX_OFFSET, 0.009),
    );
    if top {
        let u = if n > 1 { i as f64 / (n - 1) as f64 * 2.0 - 1.0 } else { 0.0 };
        let drop = TOP_DROP * smoothstep(u.abs(), 0.5, 1.0);
        let phi = u * spread + hj(i) * POSITION_JITTER_DEG.to_radians();
        Lock {
            phi,
            tip_height: tip - drop + TOP_TIP_JITTER * hj(i + J_TOP_TIP),
            root_offset,
            tip_offset,
            width: width * (1.0 + WIDTH_JITTER * hj(i + J_TOP_WIDTH)),
            shade: shade * (1.0 - SHADE_STEP * (i % 2) as f64),
            hook_deg: hook_deg * (1.0 + hj(i + J_TOP_HOOK)),
            u: ratio(phi, spread),
        }
    } else {
        let u = (i as f64 + 0.5) / n as f64 * 2.0 - 1.0;
        let drop = UNDER_DROP * smoothstep(u.abs(), 0.5, 1.0);
        Lock {
            phi: u * spread,
            tip_height: tip - drop + UNDER_TIP_JITTER * hj(i + J_UNDER_TIP),
            root_offset,
            tip_offset,
            width,
            shade,
            hook_deg,
            u,
        }
    }
}

/// A wisp: no hook, so the spread it is measured against does not matter.
fn wisp_lock(w: &Wisp) -> Lock {
    Lock {
        phi: sane(w.phi_deg, -180.0, 180.0, 0.0).to_radians(),
        tip_height: sane(w.tip_height, -1.0, 1.0, 0.0),
        root_offset: WISP_ROOT_OFFSET,
        tip_offset: WISP_TIP_OFFSET,
        width: sane(w.width, MIN_WIDTH, MAX_WIDTH, 0.022),
        shade: WISP_SHADE,
        hook_deg: 0.0,
        u: 0.0,
    }
}

/// `a / b`, or 0 if `b` is not positive.
fn ratio(a: f64, b: f64) -> f64 {
    if b > 0.0 { a / b } else { 0.0 }
}

/// The 22 rest points of `lock`, root first.
fn rest_points(g: &BangsGen, head: &dyn HeadSurface, lock: &Lock) -> [Vec3; REST_POINTS] {
    let mut controls = [DVec3::ZERO; CONTROL_POINTS];
    control_points(g.sweep_deg, head, lock, &mut controls);
    let mut spaced = [DVec3::ZERO; REST_POINTS];
    // The resample refuses only a non-finite or zero-length path; take the nearest control points then, so the clump
    // count the dispatch checks never changes.
    if spaced_points_into(&controls, &mut spaced).is_err() {
        for (k, p) in spaced.iter_mut().enumerate() {
            *p = controls[(k * (CONTROL_POINTS - 1) + REST_POINTS / 2) / (REST_POINTS - 1)];
        }
    }
    spaced.map(DVec3::as_vec3)
}

/// The control points of `lock`, root first. Allocates nothing.
#[allow(clippy::cast_precision_loss)] // point indices are tiny
fn control_points(sweep_deg: f64, head: &dyn HeadSurface, lock: &Lock, out: &mut [DVec3; CONTROL_POINTS]) {
    let crown = f64::from(head.radius(Vec3::Y)).max(1e-6);
    let tip_theta = (lock.tip_height / crown).clamp(-1.0, 1.0).acos();
    let root_theta = ROOT_THETA_DEG.to_radians();
    let sweep = sane(sweep_deg, -MAX_ANGLE_DEG, MAX_ANGLE_DEG, 0.0).to_radians();
    let hook = lock.hook_deg.to_radians();
    for (k, point) in out.iter_mut().enumerate() {
        let t = k as f64 / (CONTROL_POINTS - 1) as f64;
        let theta = root_theta + (tip_theta - root_theta) * t.powf(FALL_EXPONENT);
        let phi = lock.phi * (ROOT_AZIMUTH + (1.0 - ROOT_AZIMUTH) * smoothstep(t, 0.0, 0.65))
            - sweep * t * t
            - lock.u * hook * smoothstep(t, 0.75, 1.0);
        let offset = lock.root_offset
            + (lock.tip_offset - lock.root_offset) * t
            + BELLY * (std::f64::consts::PI * (t * BELLY_REACH).min(1.0)).sin();
        let dir = DVec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos());
        *point = head.centre().as_dvec3() + dir * (f64::from(head.radius(dir.as_vec3())) + offset);
    }
}
