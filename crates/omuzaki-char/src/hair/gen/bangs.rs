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
//! **Clearance** ([`keep_clear`]). [`compile`] makes the prototype's locks as they are. The part then calls
//! [`keep_clear`] with the frame it draws the tubes in (`OutwardFrame`, which reads the torso as well as the head), so a
//! lock cleared here is clear in the mesh that is emitted. Each lock's tube is measured against the head at every ring
//! vertex and on a grid over every triangle, and the rest points are pushed out along the line from the head's centre
//! until all stand at least 1 mm ([`SURFACE_CLEARANCE`]) off it; the spline can cut inside the head where a large hook
//! or sweep turns the lock sharply. The grid spacing comes from the head ([`sample_spacing`]), not from an assumed
//! curvature, and a head too sharp to check is refused rather than checked loosely.
//!
//! **Width.** The prototype's outline `lerp(0.040, w, ss(t, 0, 0.45)) (1 - ss(t, 0.5, 1))^0.7 + 0.0015` is not a
//! [`Taper`]. This uses the closest current `Taper`: [`ROOT_WIDTH`] at the root, `MID_GAIN w + FLOOR` at `TIP_AT`,
//! [`FLOOR`] at the tip. The exact outline waits for #842; [`outline`] is the prototype's formula, and the tests print the
//! largest difference between the two.

use omuzaki_char_api::{BangLayer, BangsGen, Bulge, HairClump, HeadSurface, Taper, Wisp};
use omuzaki_core::glam::{DVec3, Vec3};

use super::cap::smoothstep;
use crate::hair::compile::{CompiledGroup, HairGenCtx};
use crate::hair::tube::{OutwardFrame, RING, TubeError, tube_vertices};
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
/// The least shade the contract's validation allows (a built shade, after the top layer's step, stays above it).
const MIN_SHADE: f64 = 0.5;
const MAX_ANGLE_DEG: f64 = 90.0;
const MAX_OFFSET: f64 = 0.2;
/// The least distance from the skull at either end: half the lock's thickness there ([`THICKNESS`] is `base` at both ends)
/// plus 1 mm, so the tube's surface, not just its centreline, stays outside the head. Mid-lock the belly lifts the
/// centreline by far more than the swell (16 mm against 2.5 mm). The prototype's offsets (4 mm and more) are above it.
const MIN_OFFSET: f64 = THICKNESS.base * 0.5 + SURFACE_CLEARANCE;
/// How far the tube's surface stays off the head beyond its own half-thickness, metres.
pub const SURFACE_CLEARANCE: f64 = 0.001;
/// How close to the clearance target a lock may end, as a fraction of it: the pushes converge geometrically and a
/// last sliver of a micrometre is not worth another pass.
const SETTLED_FRACTION: f32 = 0.1;
/// How many times the clearance pass re-measures the tube and pushes it out.
const CLEARANCE_PASSES: usize = 16;
/// The coarsest spacing of the clearance samples over a triangle of the tube, metres. A head whose smallest curvature
/// radius is 75 mm or more is sampled at this spacing; [`sample_spacing`] goes finer on a head that curves more.
pub const SAMPLE_SPACING: f64 = 0.015;
/// How far a flat triangle may sag under the head between samples, metres: a chord of length `s` over a surface of
/// curvature radius `R` sags `s^2 / (8 R)`, and this stays under the 1 mm [`SURFACE_CLEARANCE`] with room for the
/// radius estimate's error ([`CURVATURE_SAFETY`]).
const SAG_BUDGET: f64 = 0.75 * SURFACE_CLEARANCE;
/// The estimate of the head's smallest curvature radius is taken from a coarse grid and can run high, so the spacing
/// is chosen for this fraction of it.
const CURVATURE_SAFETY: f64 = 0.5;
/// Directions probed for the head's curvature: rings of polar angle (pole to pole) and segments of azimuth around it.
const PROBE_RINGS: usize = 48;
const PROBE_SEGMENTS: usize = 96;
/// The latitude circles probed are those at least this far (sine of the polar angle) from the poles: close to a pole
/// a circle of latitude is tighter than the surface it lies on, which would fail a smooth head. Meridians, which are
/// the surface's own curvature, are probed everywhere.
const PROBE_MIN_SIN_THETA: f64 = 0.5;
/// The sharpest head ([`min_curvature_radius`] times [`CURVATURE_SAFETY`]) a fringe is built on, metres. Under this the
/// sampling would need more than [`MAX_STEPS`] samples along a triangle of an ordinary lock; the build fails instead of
/// checking loosely. A cranium of the proportions `Cranium::check` allows (0.05 to 0.4 m semi-axes, crown radius of
/// curvature 6 mm at worst) is above it.
pub const MIN_CURVATURE_RADIUS: f64 = 0.003;
/// Most grid steps along a triangle's longest side. At the finest spacing ([`MIN_CURVATURE_RADIUS`]: 3 mm) it covers a
/// triangle 0.19 m across, a lock of the widest width the checks allow (0.2 m, plus the tube's thickness) at its widest.
const MAX_STEPS: usize = 64;
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
    Taper {
        start: ROOT_WIDTH,
        mid: (MID_GAIN * w + FLOOR).clamp(MIN_WIDTH, MAX_WIDTH),
        tip: FLOOR,
        tip_at: TIP_AT,
        ..Taper::default()
    }
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
    let (width, shade) = (sane(layer.width, MIN_WIDTH, MAX_WIDTH, 0.064), sane(layer.shade, MIN_SHADE, 1.0, 1.0));
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
            width: (width * (1.0 + WIDTH_JITTER * hj(i + J_TOP_WIDTH))).clamp(MIN_WIDTH, MAX_WIDTH),
            shade: (shade * (1.0 - SHADE_STEP * (i % 2) as f64)).clamp(MIN_SHADE, 1.0),
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

/// The 22 rest points of `lock`, root first: the prototype's, before the clearance pass.
fn rest_points(g: &BangsGen, head: &dyn HeadSurface, lock: &Lock) -> [Vec3; REST_POINTS] {
    let mut controls = [DVec3::ZERO; CONTROL_POINTS];
    control_points(g, head, lock, &mut controls);
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

/// Why the fringe could not be made to clear the head.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ClearanceError {
    /// The head curves more sharply than the sampling can check.
    #[error(
        "the head's smallest curvature radius is about {radius_mm:.1} mm, under the {} mm the fringe's clearance check can \
         follow; widen the cranium",
        MIN_CURVATURE_RADIUS * 1000.0
    )]
    SharpHead {
        /// The head's estimated smallest curvature radius, millimetres (after the safety factor).
        radius_mm: f64,
    },
    /// A triangle of a tube is too large to sample at the spacing this head needs.
    #[error(
        "a bangs triangle needs {steps} clearance samples along a side, over the {MAX_STEPS} allowed (sample spacing \
         {spacing_mm:.1} mm)"
    )]
    TooManySamples {
        /// Steps the triangle needs.
        steps: usize,
        /// The spacing in use, millimetres.
        spacing_mm: f64,
    },
    /// The passes ended with a lock still inside the head.
    #[error("the fringe is still {short_mm:.2} mm short of clearing the head after {CLEARANCE_PASSES} passes")]
    Unsettled {
        /// How far the worst sample stands short of the clearance, millimetres.
        short_mm: f64,
    },
    /// A clump of the group has not the points a lock has.
    #[error("a bangs clump has {0} points, not {REST_POINTS}")]
    WrongPoints(usize),
    /// A tube could not be built to measure.
    #[error("the fringe's tube could not be built: {0}")]
    Tube(#[from] TubeError),
}

/// The head's smallest radius of curvature, metres, as the smallest circumradius of three neighbouring points of a fixed
/// grid of directions ([`PROBE_RINGS`] by [`PROBE_SEGMENTS`]): along every meridian, and along the latitude circles
/// away from the poles ([`PROBE_MIN_SIN_THETA`]). A coarse grid can run above the true minimum on a very sharp crown,
/// so the callers apply [`CURVATURE_SAFETY`]. `f64::INFINITY` for a head the probes find flat.
#[allow(clippy::cast_precision_loss)] // grid indices are tiny
fn min_curvature_radius(head: &dyn HeadSurface) -> f64 {
    let centre = head.centre().as_dvec3();
    let columns = PROBE_SEGMENTS;
    let mut grid = Vec::with_capacity((PROBE_RINGS + 1) * columns);
    for ring in 0..=PROBE_RINGS {
        let theta = ring as f64 / PROBE_RINGS as f64 * std::f64::consts::PI;
        for segment in 0..columns {
            let phi = segment as f64 / columns as f64 * std::f64::consts::TAU;
            let dir = DVec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos());
            grid.push(centre + dir * f64::from(head.radius(dir.as_vec3())));
        }
    }
    let at = |ring: usize, segment: usize| grid[ring * columns + segment % columns];
    let mut least = f64::INFINITY;
    for ring in 0..=PROBE_RINGS {
        for segment in 0..columns {
            if ring > 0 && ring < PROBE_RINGS {
                least = least.min(circumradius(at(ring - 1, segment), at(ring, segment), at(ring + 1, segment)));
            }
            let sin_theta = (ring as f64 / PROBE_RINGS as f64 * std::f64::consts::PI).sin();
            if sin_theta >= PROBE_MIN_SIN_THETA {
                least =
                    least.min(circumradius(at(ring, segment + columns - 1), at(ring, segment), at(ring, segment + 1)));
            }
        }
    }
    least
}

/// The radius of the circle through `a`, `b` and `c`; infinite for points on a line or on top of each other.
fn circumradius(a: DVec3, b: DVec3, c: DVec3) -> f64 {
    let (ab, bc, ca) = ((b - a).length(), (c - b).length(), (a - c).length());
    let twice_area = (b - a).cross(c - a).length();
    if twice_area <= f64::EPSILON * ab.max(bc).max(ca).powi(2) {
        f64::INFINITY
    } else {
        ab * bc * ca / (2.0 * twice_area)
    }
}

/// The spacing of the clearance samples for `head`, metres: the coarsest ([`SAMPLE_SPACING`]) at which a flat triangle
/// sags under the head by no more than [`SAG_BUDGET`], given the head's smallest curvature radius `R` (estimated once
/// here, see [`min_curvature_radius`], and halved by [`CURVATURE_SAFETY`]): `sqrt(8 R SAG_BUDGET)`. For a ball of 50 mm
/// that is 12 mm.
///
/// # Errors
/// [`ClearanceError::SharpHead`] if `R` is under [`MIN_CURVATURE_RADIUS`].
pub fn sample_spacing(head: &dyn HeadSurface) -> Result<f64, ClearanceError> {
    let radius = min_curvature_radius(head) * CURVATURE_SAFETY;
    if radius.is_nan() || radius < MIN_CURVATURE_RADIUS {
        return Err(ClearanceError::SharpHead { radius_mm: radius * 1000.0 });
    }
    Ok((8.0 * radius * SAG_BUDGET).sqrt().min(SAMPLE_SPACING))
}

/// Pushes the rest points of every bangs clump in `clumps` out along the lines from the head's centre until each lock's
/// tube, as [`tube_vertices`] builds it in `frame` (the frame the part draws with), stands at least
/// [`SURFACE_CLEARANCE`] off the head at every sample of its surface: all ring vertices and a barycentric grid over every
/// triangle, at the spacing [`sample_spacing`] chooses for this head. A flat triangle sags under the head between
/// samples by at most `s^2 / (8 R)` (`s` the spacing, `R` the head's curvature radius), which the spacing keeps under
/// the clearance, so a tube clear at the samples is clear everywhere. The cross-section's inward reach (it is not along
/// the radius where the lock turns) and the head's curve between rings are both included. A ring that is already clear is
/// not moved, so the points keep their order. Runs [`CLEARANCE_PASSES`] times at most: a push changes the frames of its
/// neighbours, so the passes converge on a clear tube rather than reach it in one step.
///
/// # Errors
/// [`ClearanceError`] if the head is too sharp to sample, a triangle needs more than [`MAX_STEPS`] steps, a lock is
/// still inside after the passes, or a clump is not a lock.
pub fn keep_clear(
    head: &dyn HeadSurface,
    frame: &OutwardFrame,
    clumps: &mut [HairClump],
) -> Result<(), ClearanceError> {
    let spacing = sample_spacing(head)?;
    for clump in clumps {
        let points = clump.rest.len();
        let rest: &mut [Vec3; REST_POINTS] =
            clump.rest.as_mut_slice().try_into().map_err(|_| ClearanceError::WrongPoints(points))?;
        clear_lock(head, frame, spacing, &clump.width, &clump.thickness, rest)?;
    }
    Ok(())
}

/// [`keep_clear`] for one lock.
#[allow(clippy::cast_possible_truncation)] // a millimetre fits an f32
fn clear_lock(
    head: &dyn HeadSurface,
    frame: &OutwardFrame,
    spacing: f64,
    width: &Taper,
    thickness: &Bulge,
    rest: &mut [Vec3; REST_POINTS],
) -> Result<(), ClearanceError> {
    let centre = head.centre();
    let outward = |p: Vec3| (p - centre).normalize_or_zero();
    let gap = |p: Vec3| {
        let d = p - centre;
        d.length() - head.radius(d.normalize_or_zero())
    };
    let (target, spacing_f) = (SURFACE_CLEARANCE as f32, spacing as f32);
    let mut positions = [Vec3::ZERO; REST_POINTS * RING];
    let mut normals = [Vec3::ZERO; REST_POINTS * RING];
    // A lock counts as clear when it stands within a tenth of the target of it (float noise, not a lock in the skull).
    let settled = target * SETTLED_FRACTION;
    let mut short = 0.0_f32;
    // One measurement more than pushes, so the last push is checked too.
    for pass in 0..=CLEARANCE_PASSES {
        tube_vertices(rest, width, thickness, |p| frame.rest(p), &mut positions, &mut normals)?;
        let mut deficit = [0.0_f32; REST_POINTS];
        for ring in 0..REST_POINTS - 1 {
            for k in 0..RING {
                let at = |r: usize, k: usize| positions[r * RING + k % RING];
                let (a, b, c, d) = (at(ring, k), at(ring, k + 1), at(ring + 1, k), at(ring + 1, k + 1));
                // The two triangles of the quad, as `tube_indices` winds them.
                let worst = triangle_deficit([a, c, b], spacing_f, target, &gap)?.max(triangle_deficit(
                    [b, c, d],
                    spacing_f,
                    target,
                    &gap,
                )?);
                deficit[ring] = deficit[ring].max(worst);
                deficit[ring + 1] = deficit[ring + 1].max(worst);
            }
        }
        short = deficit.iter().copied().fold(f32::MIN, f32::max);
        if short <= settled {
            return Ok(());
        }
        if pass == CLEARANCE_PASSES {
            break;
        }
        for (point, d) in rest.iter_mut().zip(deficit) {
            if d > 0.0 {
                *point += outward(*point) * d;
            }
        }
    }
    Err(ClearanceError::Unsettled { short_mm: f64::from(short) * 1000.0 })
}

/// How far the worst of the samples over the triangle `tri` stands short of `target` off the head: its three corners
/// and a barycentric grid whose spacing is at most `spacing`. Negative when every sample clears it.
///
/// # Errors
/// [`ClearanceError::TooManySamples`] if the longest side needs more than [`MAX_STEPS`] steps.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // a few dozen steps
fn triangle_deficit(
    tri: [Vec3; 3],
    spacing: f32,
    target: f32,
    gap: &impl Fn(Vec3) -> f32,
) -> Result<f32, ClearanceError> {
    let [p0, p1, p2] = tri;
    let longest = (p1 - p0).length().max((p2 - p1).length()).max((p0 - p2).length());
    let steps = ((longest / spacing).ceil() as usize).max(1);
    if steps > MAX_STEPS {
        return Err(ClearanceError::TooManySamples { steps, spacing_mm: f64::from(spacing) * 1000.0 });
    }
    let mut worst = f32::MIN;
    for first in 0..=steps {
        for second in 0..=(steps - first) {
            let p = p0 + (p1 - p0) * (first as f32 / steps as f32) + (p2 - p0) * (second as f32 / steps as f32);
            worst = worst.max(target - gap(p));
        }
    }
    Ok(worst)
}

/// The radius that turns a tip height into the polar angle of the tip: the fringe's own `tip_radius` if it is a usable
/// number, else the head's radius straight up (so a style keeps its shape when the head is scaled).
fn tip_radius(g: &BangsGen, head: &dyn HeadSurface) -> f64 {
    let crown = f64::from(head.radius(Vec3::Y));
    g.tip_radius.filter(|r| r.is_finite() && *r > 0.0).unwrap_or(crown).max(1e-6)
}

/// The control points of `lock`, root first. Allocates nothing.
#[allow(clippy::cast_precision_loss)] // point indices are tiny
fn control_points(g: &BangsGen, head: &dyn HeadSurface, lock: &Lock, out: &mut [DVec3; CONTROL_POINTS]) {
    let crown = tip_radius(g, head);
    let tip_theta = (lock.tip_height / crown).clamp(-1.0, 1.0).acos();
    let root_theta = ROOT_THETA_DEG.to_radians();
    let sweep = sane(g.sweep_deg, -MAX_ANGLE_DEG, MAX_ANGLE_DEG, 0.0).to_radians();
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
