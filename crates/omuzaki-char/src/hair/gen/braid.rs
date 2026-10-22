//! Braid-crown generator: interleaved strands that follow a path round the back of the head (port of `hair.js`
//! section 4, the braid crown).
//!
//! **The path.** Per side (`+x` first, then `-x`) there are [`CONTROL_POINTS`] control points at azimuth
//! `side · lerp(path_from_deg, path_to_deg, t)`, `t = k / (CONTROL_POINTS - 1)`. The braid line's height at that azimuth
//! blends from [`BraidGen::height_front`] to [`BraidGen::height_back`] as the part sweep's does
//! ([`braid_direction_with`]), and each point sits on the head surface in that direction, `LIFT + RISE · smoothstep(t, 0,
//! 0.2)` metres out. The path is resampled to [`REST_POINTS`] by the geometry kit's Catmull-Rom.
//!
//! **The strands.** Per side, [`BraidGen::strands`] strands with phase `ph0 = k · 2π / strands`. At path point `i` of
//! `n`, with tangent `T` from the neighbouring points, outward `O = normalize(p - head centre)`, binormal
//! `B = normalize(T × O)` and `s = i / (n - 1) · lobes · π`, the strand point is
//! `p + B · across · sin(s + ph0) + O · (out · sin(2 (s + ph0)) + 0.005)`. The `-x` side uses the same `B` rule, as the
//! prototype does, and that is deliberate: the cross product flips under a reflection in `x`, so the `-x` side's
//! sideways swing is not a mirror image of the `+x` side's. Its strands are the `+x` strands mirrored in `x` with the
//! `across` term's sign negated. The `outward` and `out` terms do mirror exactly.
//!
//! **Width and thickness.** A strand does not taper to a point. Its width is [`BraidGen::strand_width`] along its
//! length, with a linear ramp up from half of it over the first 4 % and a linear ramp down to 0.4 of it over the last
//! 8 %: a [`TaperShape::Plateau`] [`Taper`] (the prototype's `0.024 · ramp(0.5 → 1, t < 0.04) · ramp(1 → 0.4, t >
//! 0.92)`). Its thickness is [`BraidGen::strand_thickness`] at both ends, with no swell.
//!
//! **Shade** falls by 0.02 per strand from 0.98. The generator draws no random numbers.

use std::f64::consts::{PI, TAU};

use omuzaki_char_api::{BraidGen, Bulge, HeadSurface, Taper, TaperShape};
use omuzaki_core::glam::{DVec3, Vec3};

use super::cap::smoothstep;
use super::part_sweep::braid_direction_with;
use crate::hair::compile::{CompiledGroup, HairGenCtx};
use crate::kit::catmull::spaced_points_into;

/// Control points per side of the path, before the Catmull-Rom resample.
pub const CONTROL_POINTS: usize = 23;
/// Points per strand after the resample.
pub const REST_POINTS: usize = 80;
/// Height of the path above the head surface at its root, metres.
pub const LIFT: f64 = 0.036;
/// Extra height the path gains over the first [`RISE_REACH`] of its length, metres.
pub const RISE: f64 = 0.004;
/// The part of the path, as a fraction of its length, over which [`RISE`] is gained.
pub const RISE_REACH: (f64, f64) = (0.0, 0.2);
/// Height of a strand above its path point, metres.
const STRAND_LIFT: f64 = 0.005;
/// Shade of the first strand; each later strand is [`SHADE_STEP`] darker.
const SHADE: f64 = 0.98;
/// Shade lost per strand.
const SHADE_STEP: f64 = 0.02;
/// Fraction of the strand width at the root.
const ROOT_FRACTION: f64 = 0.5;
/// Fraction of the strand width left at the tip.
const TIP_FRACTION: f64 = 0.4;
/// Length of the root ramp, as a fraction of the strand.
const RAMP_IN: f64 = 0.04;
/// Length of the tip ramp, as a fraction of the strand.
const RAMP_OUT: f64 = 0.08;

/// Compiles the group: [`BraidGen::strands`] strands per side, `+x` side first.
pub fn compile(g: &BraidGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let g = &sanitised(g);
    let mut clumps = Vec::with_capacity(clump_count(g));
    let mut controls = [DVec3::ZERO; CONTROL_POINTS];
    let mut path = [DVec3::ZERO; REST_POINTS];
    let width = Taper::new(ROOT_FRACTION * g.strand_width, g.strand_width, TIP_FRACTION * g.strand_width)
        .with_shape(TaperShape::plateau(RAMP_IN, RAMP_OUT));
    let thickness = Bulge { base: g.strand_thickness, swell: 0.0 };
    for side in [1.0, -1.0] {
        control_points(g, ctx.head, side, &mut controls);
        // The resample refuses only a non-finite or zero-length path; take the nearest control points then, so the
        // clump count the dispatch checks never changes.
        if spaced_points_into(&controls, &mut path).is_err() {
            for (k, p) in path.iter_mut().enumerate() {
                *p = controls[(k * (CONTROL_POINTS - 1) + REST_POINTS / 2) / (REST_POINTS - 1)];
            }
        }
        for k in 0..g.strands {
            let rest = strand(g, ctx.head.centre().as_dvec3(), &path, k);
            let shade = SHADE - SHADE_STEP * f64::from(k);
            clumps.push(ctx.clump(rest, width, thickness, shade));
        }
    }
    CompiledGroup::tubes(clumps)
}

/// `g` with every number that is not finite replaced by its default, so a bad file draws a plain braid instead of
/// non-finite points. The strand count is untouched, so [`clump_count`] holds.
fn sanitised(g: &BraidGen) -> BraidGen {
    let d = BraidGen::default();
    let fix = |x: f64, default: f64| if x.is_finite() { x } else { default };
    BraidGen {
        path_from_deg: fix(g.path_from_deg, d.path_from_deg),
        path_to_deg: fix(g.path_to_deg, d.path_to_deg),
        height_front: fix(g.height_front, d.height_front),
        height_back: fix(g.height_back, d.height_back),
        strands: g.strands,
        lobes: fix(g.lobes, d.lobes),
        across: fix(g.across, d.across),
        out: fix(g.out, d.out),
        strand_width: fix(g.strand_width, d.strand_width),
        strand_thickness: fix(g.strand_thickness, d.strand_thickness),
        height_radius: g.height_radius.filter(|r| r.is_finite()),
    }
}

/// How many clumps [`compile`] makes: [`BraidGen::strands`] on each side.
pub fn clump_count(g: &BraidGen) -> usize {
    2 * usize::from(g.strands)
}

/// The control points of the path on `side` (`+1` for +x, `-1` for -x), root first, written into `out`. Allocates
/// nothing.
#[allow(clippy::cast_precision_loss)] // point indices are small
fn control_points(g: &BraidGen, head: &dyn HeadSurface, side: f64, out: &mut [DVec3; CONTROL_POINTS]) {
    for (k, point) in out.iter_mut().enumerate() {
        let t = k as f64 / (CONTROL_POINTS - 1) as f64;
        let phi = side * (g.path_from_deg + (g.path_to_deg - g.path_from_deg) * t).to_radians();
        let crown = g.height_radius.unwrap_or_else(|| f64::from(head.radius(Vec3::Y)));
        let dir = braid_direction_with(g.height_front, g.height_back, crown, phi);
        let offset = LIFT + RISE * smoothstep(t, RISE_REACH.0, RISE_REACH.1);
        *point = on_head(head, dir, offset);
    }
}

/// The point on the head in direction `dir`, `offset` metres out.
fn on_head(head: &dyn HeadSurface, dir: DVec3, offset: f64) -> DVec3 {
    head.centre().as_dvec3() + dir * (f64::from(head.radius(dir.as_vec3())) + offset)
}

/// Strand `k` of a side: its `path.len()` rest points, root first.
#[allow(clippy::cast_precision_loss)] // point indices are small
fn strand(g: &BraidGen, centre: DVec3, path: &[DVec3], k: u8) -> Vec<Vec3> {
    let n = path.len();
    let ph0 = f64::from(k) * TAU / f64::from(g.strands);
    (0..n)
        .map(|i| {
            let before = path[i.saturating_sub(1)];
            let after = path[(i + 1).min(n - 1)];
            let tangent = (after - before).normalize_or_zero();
            let outward = (path[i] - centre).normalize_or_zero();
            let binormal = tangent.cross(outward).normalize_or_zero();
            let along = i as f64 / (n - 1) as f64 * g.lobes * PI + ph0;
            let point =
                path[i] + binormal * g.across * along.sin() + outward * (g.out * (2.0 * along).sin() + STRAND_LIFT);
            point.as_vec3()
        })
        .collect()
}
