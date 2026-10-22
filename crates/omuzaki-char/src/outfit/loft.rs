//! Garment lofting over the body-fit sections: rings, surfaces and their skin weights.
//!
//! The port of the prototype's torso section with ease, `tp(y, ph, off)` (`uniform.js` 23-35), generalised from the
//! torso to every [`BodyRegion`]: a [`Ring`] is the section of a region at `t` along its axis, pushed out by an offset
//! and cut as a superellipse, and a [`Surface`] is the rings of one region as a function of `(t, phi)`. Lofting a
//! surface ([`Surface::loft`]) samples it into a grid whose rows are rings, wound outward, with UVs that count fabric
//! tiles and skin weights from the region's bones ([`RegionWeights`], the prototype's `wTorso` over any region).
//!
//! Angles follow the kit's [`loft_y`](crate::kit::loft::loft_y): `phi = 0` is the front of the section and `pi / 2` its
//! `across` side, which is the character's left (+x) for every region.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, BodyRegion, JointRef, RegionEnd, Section};
use omuzaki_core::glam::{DVec2, DVec3, Vec3};

use super::common::GarmentError;
use crate::kit::curve::{TAU, sup};
use crate::kit::grid::{Grid, GridOptions, MeshBuilder, Skin};
use crate::kit::skin::{MAX_INFLUENCES, blend_between, limit_influences, normalise};
use crate::template::layout::section_basis;

/// The roundest cut a garment may have: an ellipse. A rounder (diamond-like) cut would dip inside the section and so
/// into the layer below.
pub const MIN_EXPONENT: f64 = 2.0;
/// The squarest cut a garment may have.
pub const MAX_EXPONENT: f64 = 8.0;
/// A cut a little squarer than an ellipse: the prototype's torso sections (`uniform.js` `TE`).
pub const SOFT_SQUARE: f64 = 2.2;
/// The smallest radius a ring may have, metres: less is a collapsed section the normals can't be taken of.
pub const MIN_RADIUS: f64 = 1e-4;
/// Half the width of the hand-over between two bones along a region, as a fraction of the region (each side of the
/// joint).
pub const BLEND_HALF_WIDTH: f64 = 0.08;
/// The most rings or segments a loft may have.
pub const MAX_LOFT_SIZE: usize = 512;
/// Rings sampled along a region to find where a point's section plane is ([`Surface::project`]).
const PROJECT_SAMPLES: usize = 64;
/// Bisections that refine it: far below a micrometre on any region.
const PROJECT_BISECTIONS: usize = 40;
/// The step of the finite differences that give a surface's tangents, in `t` and in radians.
const STEP: f64 = 1e-4;

/// Checks a superellipse exponent is in [`MIN_EXPONENT`]..=[`MAX_EXPONENT`].
///
/// # Errors
/// [`GarmentError::Invalid`] naming the range.
pub fn check_exponent(exponent: f64) -> Result<(), GarmentError> {
    if exponent.is_finite() && (MIN_EXPONENT..=MAX_EXPONENT).contains(&exponent) {
        Ok(())
    } else {
        Err(GarmentError::invalid("exponent", format!("{exponent} isn't in {MIN_EXPONENT}..={MAX_EXPONENT}")))
    }
}

/// One section of a region, pushed out and cut as a superellipse: the prototype's `tp` at one height.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Ring {
    /// The region.
    pub region: BodyRegion,
    /// Where along it, `0..=1`.
    pub t: f64,
    /// The section's centre.
    pub centre: DVec3,
    /// The region's axis here (unit).
    pub axis: DVec3,
    /// The direction of `radii.x` (across the body, unit).
    pub across: DVec3,
    /// The direction of `radii.y` (to the front, or up for a foot; unit).
    pub through: DVec3,
    /// The half-extents with the offset added.
    pub radii: DVec2,
    /// The superellipse exponent (2 for an ellipse).
    pub exponent: f64,
}

impl Ring {
    /// The ring's point in its own plane at angle `phi`: `(across, through)` coordinates.
    fn local(&self, phi: f64) -> DVec2 {
        DVec2::new(sup(phi.sin(), self.exponent) * self.radii.x, sup(phi.cos(), self.exponent) * self.radii.y)
    }

    /// Where the ray from the centre through `q` (in the ring's plane, `(across, through)`) meets the ring: the angle
    /// of that point, and how far `q` lies outside it along the ray (negative inside). The superellipse
    /// `|x/rx|^e + |y/ry|^e = 1` meets the ray at `s = 1 / (|x̂/rx|^e + |ŷ/ry|^e)^(1/e)`, and its angle has
    /// `sin phi = ±|x/rx|^(e/2)` and `cos phi = ±|y/ry|^(e/2)`, the inverse of [`Ring::point`]'s `sup`. At the centre
    /// itself the ray is taken toward the front.
    pub fn along_ray(&self, q: DVec2) -> (f64, f64) {
        let e = self.exponent;
        let length = q.length();
        let dir = if length > 0.0 { q / length } else { DVec2::Y };
        let s = ((dir.x / self.radii.x).abs().powf(e) + (dir.y / self.radii.y).abs().powf(e)).powf(-1.0 / e);
        let on = dir * s;
        let sin = on.x.signum() * (on.x / self.radii.x).abs().powf(e / 2.0);
        let cos = on.y.signum() * (on.y / self.radii.y).abs().powf(e / 2.0);
        (sin.atan2(cos), length - s)
    }

    /// The point at angle `phi`.
    pub fn point(&self, phi: f64) -> DVec3 {
        let q = self.local(phi);
        self.centre + self.across * q.x + self.through * q.y
    }

    /// The outward normal at angle `phi`, in the ring's plane: the gradient of the superellipse, as `tp` takes it, so
    /// rings offset from one another stay parallel.
    pub fn normal(&self, phi: f64) -> DVec3 {
        let q = self.local(phi);
        let e = self.exponent;
        let gx = q.x.signum() * (q.x / self.radii.x).abs().powf(e - 1.0) / self.radii.x;
        let gy = q.y.signum() * (q.y / self.radii.y).abs().powf(e - 1.0) / self.radii.y;
        let g = DVec2::new(if q.x == 0.0 { 0.0 } else { gx }, if q.y == 0.0 { 0.0 } else { gy });
        let g = g.try_normalize().unwrap_or(DVec2::Y);
        (self.across * g.x + self.through * g.y).normalize_or_zero()
    }
}

/// The section of `region` of `fit` at `t`, with `offset` added to both radii and cut with `exponent`.
///
/// # Errors
/// [`GarmentError::NoSection`] if the fit has none there; [`GarmentError::Invalid`] for a `t` outside `0..=1` (where a
/// fit's sections are defined) or a non-finite `offset`, an
/// exponent outside [`MIN_EXPONENT`]..=[`MAX_EXPONENT`], or a ring whose radii come out below [`MIN_RADIUS`] or
/// not finite.
pub fn ring_at(
    fit: &dyn BodyFit,
    region: BodyRegion,
    t: f64,
    exponent: f64,
    offset: f64,
) -> Result<Ring, GarmentError> {
    if !(0.0..=1.0).contains(&t) || !offset.is_finite() {
        return Err(GarmentError::invalid("ring", format!("t = {t} must be in 0..=1 and offset = {offset} finite")));
    }
    check_exponent(exponent)?;
    #[allow(clippy::cast_possible_truncation)] // a fit is sampled in f32
    let section = fit.section(region, t as f32).ok_or(GarmentError::NoSection { region, t })?;
    let finite = section.centre.is_finite() && section.axis.is_finite() && section.radii.is_finite();
    let axis = section.axis.as_dvec3().try_normalize().filter(|_| finite);
    let Some(axis) = axis else {
        return Err(GarmentError::invalid("section", format!("{region:?} at t = {t} is not finite or has no axis")));
    };
    let radii = section.radii.as_dvec2() + DVec2::splat(offset);
    if !(radii.is_finite() && radii.min_element() >= MIN_RADIUS) {
        return Err(GarmentError::invalid(
            "ring",
            format!("{region:?} at t = {t}: radii {radii} are below {MIN_RADIUS} m with offset {offset}"),
        ));
    }
    let (across, through) = section_basis(region, axis.as_vec3());
    let mut ring = Ring {
        region,
        t,
        centre: section.centre.as_dvec3(),
        axis,
        across: across.as_dvec3(),
        through: through.as_dvec3(),
        radii,
        exponent,
    };
    // A ring inside the body's sections (a negative offset) stays inside them.
    match region {
        _ if offset < 0.0 => {}
        BodyRegion::Torso => {
            for leg in [BodyRegion::LegL, BodyRegion::LegR] {
                clear_region(fit, &mut ring, leg, offset);
            }
            // The arms' roots make the shoulders' tops and the armpits' walls.
            for arm in [BodyRegion::ArmL, BodyRegion::ArmR] {
                clear_root(fit, &mut ring, arm, offset);
            }
        }
        BodyRegion::Neck => {
            for arm in [BodyRegion::ArmL, BodyRegion::ArmR] {
                clear_root(fit, &mut ring, arm, offset);
            }
        }
        // The pelvis's floor lies between the thighs, below the torso's first section: each leg clears its own side.
        BodyRegion::LegL => clear_cap(fit, &mut ring, BodyRegion::Torso, RegionEnd::Start, offset, Some(1.0)),
        BodyRegion::LegR => clear_cap(fit, &mut ring, BodyRegion::Torso, RegionEnd::Start, offset, Some(-1.0)),
        _ => {}
    }
    Ok(ring)
}

/// Points taken round a neighbour's section that a ring clears.
const CLEAR_SAMPLES: usize = 36;
/// Widths tried between a ring's own and its whole scale when it grows to clear a neighbour.
const CLEAR_WIDTHS: usize = 32;
/// Steps that find a neighbour's section through a ring's centre, and how near its plane must pass, metres.
const CLEAR_STEPS: usize = 16;
const CLEAR_TOLERANCE: f64 = 1e-6;

/// Grows `ring` until it clears, by `offset`, `neighbour` where the ring's plane cuts it: the neighbour's surface
/// pushed out by `offset`, cut by the ring's plane. The thighs rise into the pelvis, so a ring round the hips goes
/// round the tops of the thighs, and their domes, as well as the pelvis, as cloth does. The neighbour is taken in slices
/// within `offset` of the ring's plane, each pushed out by as much as a ball of that radius reaches at its distance:
/// its sections ([`BodyFit::section`]) along its span and its caps' slices ([`BodyFit::cap`]) past its ends, one
/// surface, so a ring passing from the one to the other doesn't jump. A neighbour the ring's plane doesn't come within
/// `offset` of (a ring above the thighs' domes) leaves it as it is.
fn clear_region(fit: &dyn BodyFit, ring: &mut Ring, neighbour: BodyRegion, offset: f64) {
    #[allow(clippy::cast_possible_truncation)] // a fit is sampled in f32
    let section = |t: f64| fit.section(neighbour, t as f32).filter(|s| s.centre.is_finite() && s.axis.is_finite());
    let (Some(first), Some(last)) = (section(0.0), section(1.0)) else { return };
    let height = |s: &Section| (ring.centre - s.centre.as_dvec3()).dot(s.axis.as_dvec3());
    let length = first.centre.distance(last.centre);
    if !(length.is_finite() && length > 0.0) {
        return;
    }
    let length = f64::from(length);
    // Where along the neighbour the ring's plane lies: before its start or past its end by its height over that end's
    // plane, as a share of its length.
    let (h0, h1) = (height(&first), height(&last));
    let at = if h0 < 0.0 {
        h0 / length
    } else if h1 > 0.0 {
        1.0 + h1 / length
    } else {
        // A limb is near straight: the false position between the bracketing heights (Illinois: the end that stays
        // has its height halved, so neither end sticks) lands within a hair in a few steps.
        let ((mut lo, mut h_lo), (mut hi, mut h_hi)) = ((0.0, h0), (1.0, h1));
        let mut t = 0.0;
        let mut last_side = 0;
        for _ in 0..CLEAR_STEPS {
            t = lo + (hi - lo) * h_lo / (h_lo - h_hi);
            let Some(h) = section(t).map(|s| height(&s)) else { return };
            if h.abs() < CLEAR_TOLERANCE {
                break;
            }
            if h.signum() == h_lo.signum() {
                (lo, h_lo) = (t, h);
                if last_side == -1 {
                    h_hi /= 2.0;
                }
                last_side = -1;
            } else {
                (hi, h_hi) = (t, h);
                if last_side == 1 {
                    h_lo /= 2.0;
                }
                last_side = 1;
            }
        }
        t
    };
    // Within `offset` of the plane, each slice pushed out by a ball's reach at its distance.
    let ball = (0..CAP_HEIGHTS).map(|k| {
        let u = 2.0 * crate::kit::curve::fraction(k, CAP_HEIGHTS - 1) - 1.0;
        (u * offset, offset * (1.0 - u * u).max(0.0).sqrt())
    });
    let mut points = Vec::new();
    for (d, grow) in ball {
        let t = at + d / length;
        #[allow(clippy::cast_possible_truncation)] // a fit is sampled in f32
        let slice = if t < 0.0 {
            fit.cap(neighbour, RegionEnd::Start, (-t * length) as f32)
        } else if t > 1.0 {
            fit.cap(neighbour, RegionEnd::End, ((t - 1.0) * length) as f32)
        } else {
            section(t)
        };
        if let Some(slice) = slice.filter(|s| s.centre.is_finite() && s.radii.is_finite()) {
            points.extend(section_points(ring, neighbour, &slice, grow, None));
        }
    }
    grow_to_hold(ring, &points);
}


/// Heights either side of a ring's plane at which a cap's slices are taken, to find the cap's offset surface there.
const CAP_HEIGHTS: usize = 7;

/// Grows `ring` until it clears, by `offset`, the cap of `neighbour` past `end` where the ring's plane cuts it; with
/// `side`, only the part of it on that side of the body's midplane (`+1` the character's left, +x). What a ring must
/// hold is the cap's surface pushed out by `offset`, cut by its plane: the slices ([`BodyFit::cap`]) within
/// `offset` of the plane, each pushed out by as much as a ball of that radius reaches at its distance, so a ring above a
/// dome's top clears it less and less until it clears it no more.
fn clear_cap(
    fit: &dyn BodyFit,
    ring: &mut Ring,
    neighbour: BodyRegion,
    end: RegionEnd,
    offset: f64,
    side: Option<f64>,
) {
    let t = match end {
        RegionEnd::Start => 0.0,
        RegionEnd::End => 1.0,
    };
    let Some(s) = fit.section(neighbour, t).filter(|s| s.centre.is_finite() && s.axis.is_finite()) else { return };
    let outward = match end {
        RegionEnd::Start => -s.axis.as_dvec3(),
        RegionEnd::End => s.axis.as_dvec3(),
    };
    let height = (ring.centre - s.centre.as_dvec3()).dot(outward.normalize_or_zero());
    if height <= 0.0 {
        return;
    }
    let mut points = Vec::new();
    for k in 0..CAP_HEIGHTS {
        let u = 2.0 * crate::kit::curve::fraction(k, CAP_HEIGHTS - 1) - 1.0;
        let h = height + u * offset;
        #[allow(clippy::cast_possible_truncation)] // a fit is sampled in f32
        let slice = if h < 0.0 { None } else { fit.cap(neighbour, end, h as f32) };
        if let Some(slice) = slice {
            points.extend(section_points(ring, neighbour, &slice, offset * (1.0 - u * u).max(0.0).sqrt(), side));
        }
    }
    grow_to_hold(ring, &points);
}

/// Slices a root is taken in, and points round each.
const ROOT_LEVELS: usize = 24;
const ROOT_SAMPLES: usize = 32;
/// Steps of the search for the top of a root, and the furthest it looks, metres.
const ROOT_STEPS: usize = 14;
/// How much further than the root's own reach a ring's plane must pass to be clear of it: the slices' ellipses may
/// stand a little wide of the start's.
const CAP_SLACK: f64 = 1.0;
const ROOT_LIMIT: f32 = 0.1;

/// How far `neighbour`'s cap past its start rises ([`BodyFit::cap`]), metres; 0 where it has none.
fn root_top(fit: &dyn BodyFit, neighbour: BodyRegion) -> f32 {
    let has = |h: f32| fit.cap(neighbour, RegionEnd::Start, h).is_some();
    if !has(0.0) {
        return 0.0;
    }
    if has(ROOT_LIMIT) {
        return ROOT_LIMIT;
    }
    let (mut lo, mut hi) = (0.0_f32, ROOT_LIMIT);
    for _ in 0..ROOT_STEPS {
        let mid = 0.5 * (lo + hi);
        if has(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Grows `ring` until it clears, by `offset`, the root of `neighbour` where the ring's plane cuts it: the cap past the
/// neighbour's start, which leans and curls into the region the ring goes round (an arm's root rising into the shoulder,
/// across the torso's rings). The root is lofted from its start's section through its slices to a point `offset` past
/// its top, and the ring holds every point where that loft crosses its plane.
fn clear_root(fit: &dyn BodyFit, ring: &mut Ring, neighbour: BodyRegion, offset: f64) {
    let top = root_top(fit, neighbour);
    let Some(start) = fit.section(neighbour, 0.0).filter(|s| s.centre.is_finite() && s.axis.is_finite()) else {
        return;
    };
    if top <= 0.0 {
        return;
    }
    // A ring whose plane passes clear of the ball round the whole root (its start's section and everything within its
    // top of it) has nothing of it to hold.
    let reach = f64::from(top + start.radii.max_element()) + offset.abs();
    let below = ring.axis.dot(start.centre.as_dvec3() - ring.centre);
    if below.abs() > reach * (1.0 + CAP_SLACK) {
        return;
    }
    #[allow(clippy::cast_precision_loss)] // small counts
    let levels: Vec<Section> = std::iter::once(Some(start))
        .chain(
            (0..ROOT_LEVELS)
                .map(|k| fit.cap(neighbour, RegionEnd::Start, top * (k as f32 + 0.5) / ROOT_LEVELS as f32)),
        )
        .flatten()
        .filter(|s| s.centre.is_finite() && s.radii.is_finite())
        .collect();
    let height = |p: DVec3| (p - ring.centre).dot(ring.axis);
    // Only the slices whose ellipses can reach the ring's plane, and those either side of where the plane passes
    // between two of them, are taken round.
    let near: Vec<bool> = levels
        .iter()
        .map(|s| height(s.centre.as_dvec3()).abs() <= f64::from(s.radii.max_element()) + offset.abs())
        .collect();
    let crossed = |i: usize| {
        near[i] || near.get(i + 1).copied().unwrap_or(false) || (i > 0 && near[i - 1]) || {
            let h = |k: usize| levels.get(k).map(|s| height(s.centre.as_dvec3()));
            matches!((h(i), h(i + 1)), (Some(a), Some(b)) if (a < 0.0) != (b < 0.0))
                || (i > 0 && matches!((h(i - 1), h(i)), (Some(a), Some(b)) if (a < 0.0) != (b < 0.0)))
        }
    };
    let rings: Vec<Option<Vec<DVec3>>> = levels
        .iter()
        .enumerate()
        .map(|(i, s)| {
            crossed(i).then(|| {
                let (across, through) = section_basis(neighbour, s.axis);
                let (centre, across, through) = (s.centre.as_dvec3(), across.as_dvec3(), through.as_dvec3());
                let radii = s.radii.as_dvec2() + DVec2::splat(offset);
                (0..ROOT_SAMPLES)
                    .map(|k| {
                        let (sin, cos) = (TAU * crate::kit::curve::fraction(k, ROOT_SAMPLES)).sin_cos();
                        centre + across * (radii.x * cos) + through * (radii.y * sin)
                    })
                    .collect()
            })
        })
        .collect();
    let Some(last) = levels.last() else { return };
    let apex = last.centre.as_dvec3() - start.axis.as_dvec3().normalize_or_zero() * offset;
    let mut points = Vec::new();
    let mut cross = |a: DVec3, b: DVec3| {
        let (ha, hb) = (height(a), height(b));
        if (ha < 0.0) != (hb < 0.0) {
            let d = a.lerp(b, ha / (ha - hb)) - ring.centre;
            points.push(DVec2::new(d.dot(ring.across), d.dot(ring.through)));
        }
    };
    for pair in rings.windows(2) {
        if let (Some(a), Some(b)) = (&pair[0], &pair[1]) {
            for (a, b) in a.iter().zip(b) {
                cross(*a, *b);
            }
        }
    }
    if let Some(Some(last)) = rings.last() {
        for a in last {
            cross(*a, apex);
        }
    }
    grow_to_hold(ring, &points);
}

/// The points round `section` of `region`, with `offset` added to its radii, in the plane of `ring`; with `side`, only
/// those on that side of the body's midplane.
fn section_points(ring: &Ring, region: BodyRegion, section: &Section, offset: f64, side: Option<f64>) -> Vec<DVec2> {
    let (across, through) = section_basis(region, section.axis);
    let (centre, across, through) = (section.centre.as_dvec3(), across.as_dvec3(), through.as_dvec3());
    let radii = section.radii.as_dvec2() + DVec2::splat(offset);
    (0..CLEAR_SAMPLES)
        .filter_map(|k| {
            let (sin, cos) = (TAU * crate::kit::curve::fraction(k, CLEAR_SAMPLES)).sin_cos();
            let p = centre + across * (radii.x * cos) + through * (radii.y * sin);
            let d = p - ring.centre;
            (!side.is_some_and(|side| p.x * side < 0.0)).then(|| DVec2::new(d.dot(ring.across), d.dot(ring.through)))
        })
        .collect()
}

/// Grows `ring`, keeping its shape and centre, until it holds every one of `points` (in its plane): of the rings that
/// do, the one of least area, widened and deepened by as little as each needs.
fn grow_to_hold(ring: &mut Ring, points: &[DVec2]) {
    let e = ring.exponent;
    let r = ring.radii;
    let level = |q: DVec2, r: DVec2| ((q.x / r.x).abs().powf(e) + (q.y / r.y).abs().powf(e)).powf(1.0 / e);
    // The ring is symmetric across both its axes, and holds a point whenever it holds one further out both ways: only
    // the points no other passes in both `|x|` and `|y|` matter. And as it only grows, those it holds already it holds
    // at every size tried, so they are dropped first (a point that passes one outside the ring is outside it too, so
    // dropping them first leaves the same frontier, and most points are inside).
    let mut points: Vec<DVec2> =
        points.iter().filter(|q| q.is_finite()).map(|q| q.abs()).filter(|q| level(*q, r) > 1.0).collect();
    points.sort_unstable_by(|a, b| b.x.total_cmp(&a.x).then(b.y.total_cmp(&a.y)));
    let mut highest = f64::NEG_INFINITY;
    points.retain(|q| {
        let keep = q.y > highest;
        highest = highest.max(q.y);
        keep
    });
    let Some(scale) = points.iter().map(|q| level(*q, r)).reduce(f64::max) else { return };
    // For each width from the ring's own to the whole scale's, the least depth that holds every point there: a point
    // needs `|q_y| / (1 - |q_x / r_x|^e)^(1/e)`, and can't be held at a width it lies beyond.
    let depth = |width: f64| {
        points.iter().try_fold(r.y, |most, q| {
            let room = 1.0 - (q.x / width).abs().powf(e);
            (room > 0.0).then(|| most.max(q.y.abs() / room.powf(1.0 / e)))
        })
    };
    ring.radii = (0..=CLEAR_WIDTHS)
        .filter_map(|k| {
            let width = r.x * (1.0 + (scale - 1.0) * crate::kit::curve::fraction(k, CLEAR_WIDTHS));
            depth(width).map(|d| DVec2::new(width, d))
        })
        .min_by(|a, b| (a.x * a.y).total_cmp(&(b.x * b.y)))
        .unwrap_or(r * scale);
}

// -------------------------------------------------------------------------------------------------------- weights

/// The skin weights of a region along its axis: the bones that move it, each at the fraction of the region where its
/// joint sits, handing over from one to the next across [`BLEND_HALF_WIDTH`] either side of each joint. The weights are
/// the prototype's chained smoothsteps (`wTorso`): bone `k` gets the product of the earlier hand-overs times what it
/// keeps of its own, so they sum to 1.
#[derive(Clone, Debug, PartialEq)]
pub struct RegionWeights {
    /// The bones in order along the region, with the fraction where each one's joint sits.
    chain: Vec<(JointRef, f64)>,
}

/// The bones that move `region`, in order along it.
fn bones_of(region: BodyRegion) -> Result<&'static [HumanoidBone], GarmentError> {
    use HumanoidBone as B;
    Ok(match region {
        BodyRegion::Torso => &[B::Hips, B::Spine, B::Chest, B::UpperChest, B::Neck],
        BodyRegion::Neck => &[B::Neck, B::Head],
        BodyRegion::ArmL => &[B::LeftUpperArm, B::LeftLowerArm, B::LeftHand],
        BodyRegion::ArmR => &[B::RightUpperArm, B::RightLowerArm, B::RightHand],
        BodyRegion::LegL => &[B::LeftUpperLeg, B::LeftLowerLeg, B::LeftFoot],
        BodyRegion::LegR => &[B::RightUpperLeg, B::RightLowerLeg, B::RightFoot],
        BodyRegion::FootL => &[B::LeftFoot, B::LeftToes],
        BodyRegion::FootR => &[B::RightFoot, B::RightToes],
        other => return Err(GarmentError::invalid("region", format!("{other:?} has no garment weights yet"))),
    })
}

impl RegionWeights {
    /// The weights of `region` of `fit`: each bone's joint projected onto the line from the region's start to its end.
    /// Bones the fit doesn't have (an optional upper chest) are left out.
    ///
    /// # Errors
    /// [`GarmentError::NoSection`] if the region has no start or end; [`GarmentError::Invalid`] if it has no length,
    /// none of its bones, or is a region without weights.
    pub fn of(fit: &dyn BodyFit, region: BodyRegion) -> Result<Self, GarmentError> {
        let end = |t: f32| {
            fit.section(region, t)
                .map(|s| s.centre.as_dvec3())
                .ok_or(GarmentError::NoSection { region, t: f64::from(t) })
        };
        let (start, stop) = (end(0.0)?, end(1.0)?);
        let along = stop - start;
        let length_sq = along.length_squared();
        if !(length_sq.is_finite() && length_sq > MIN_RADIUS * MIN_RADIUS) {
            return Err(GarmentError::invalid("region", format!("{region:?} has no length")));
        }
        let mut chain = Vec::new();
        for &bone in bones_of(region)? {
            let joint = JointRef::Humanoid(bone);
            if let Some(p) = fit.joint(&joint).map(Vec3::as_dvec3).filter(|p| p.is_finite()) {
                chain.push((joint, ((p - start).dot(along) / length_sq).clamp(0.0, 1.0)));
            }
        }
        if chain.is_empty() {
            return Err(GarmentError::invalid("region", format!("the fit has none of the bones of {region:?}")));
        }
        // Stable: two joints at one fraction keep the region's bone order.
        chain.sort_by(|a, b| a.1.total_cmp(&b.1));
        Ok(Self { chain })
    }

    /// The bones and where their joints sit, in order along the region.
    pub fn chain(&self) -> &[(JointRef, f64)] {
        &self.chain
    }

    /// The weights at `t`: at most [`MAX_INFLUENCES`], normalised.
    #[allow(clippy::cast_possible_truncation)] // weights are in 0..=1; the mesh stores f32
    pub fn at(&self, t: f64) -> Vec<(JointRef, f32)> {
        let mut blend = Vec::with_capacity(self.chain.len());
        let mut reach = 1.0;
        for (k, (joint, _)) in self.chain.iter().enumerate() {
            let handed = match self.chain.get(k + 1) {
                Some((next, at)) => {
                    blend_between(joint.clone(), next.clone(), at - BLEND_HALF_WIDTH, at + BLEND_HALF_WIDTH).amount(t)
                }
                None => 0.0,
            };
            blend.push((joint.clone(), (reach * (1.0 - handed)) as f32));
            reach *= handed;
        }
        limit_influences(&mut blend, MAX_INFLUENCES);
        normalise(&mut blend);
        blend
    }
}

// -------------------------------------------------------------------------------------------------------- surfaces

/// A garment's own shaping of a ring (a flare, a hem's curve, a drape): the point at `(t, phi)` given the plain ring
/// there. It must be a pure function, and stay outside the ring for the garment to keep its ease.
pub type ShapeFn = dyn Fn(&Ring, f64, f64) -> DVec3 + Send + Sync;

/// A point on a surface with its frame.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SurfacePoint {
    /// The point.
    pub point: DVec3,
    /// The outward unit normal.
    pub normal: DVec3,
    /// The unit tangent along the region (growing `t`).
    pub along: DVec3,
    /// The unit tangent around it (growing `phi`).
    pub around: DVec3,
}

/// Where a point lies over a [`Surface`] ([`Surface::project`]).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Projection {
    /// Where along the region.
    pub t: f64,
    /// The angle round it.
    pub phi: f64,
    /// How far outside the plain ring, metres, along the ray from its centre (negative inside).
    pub outside: f64,
}

/// One region of a fit wrapped by a garment: rings of `region` pushed out by `offset`, cut with `exponent`, optionally
/// reshaped. Every point of a garment's surface, its lofted mesh and the trims on it come from here, so a trim placed by
/// `(t, phi)` sits on the mesh.
#[derive(Clone)]
pub struct Surface {
    fit: Arc<dyn BodyFit>,
    region: BodyRegion,
    exponent: f64,
    offset: f64,
    shape: Option<Arc<ShapeFn>>,
    weights: RegionWeights,
    /// The plain rings already worked out, by `t`: clearing a neighbour searches the fit, and a garment asks for the
    /// same rings many times over (every projection walks the same samples).
    rings: Arc<Mutex<HashMap<u64, Ring>>>,
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Surface")
            .field("region", &self.region)
            .field("exponent", &self.exponent)
            .field("offset", &self.offset)
            .field("shaped", &self.shape.is_some())
            .finish_non_exhaustive()
    }
}

impl Surface {
    /// The rings of `region` of `fit`, `offset` metres out (a garment's ease), cut with `exponent`.
    ///
    /// # Errors
    /// [`GarmentError`] for an exponent out of range, a non-finite offset, or a region [`RegionWeights::of`] refuses.
    pub fn new(fit: Arc<dyn BodyFit>, region: BodyRegion, exponent: f64, offset: f64) -> Result<Self, GarmentError> {
        check_exponent(exponent)?;
        if !offset.is_finite() {
            return Err(GarmentError::invalid("offset", format!("{offset} must be finite")));
        }
        let weights = RegionWeights::of(fit.as_ref(), region)?;
        Ok(Self { fit, region, exponent, offset, shape: None, weights, rings: Arc::default() })
    }

    /// The same surface reshaped by `shape`.
    #[must_use]
    pub fn with_shape(mut self, shape: impl Fn(&Ring, f64, f64) -> DVec3 + Send + Sync + 'static) -> Self {
        self.shape = Some(Arc::new(shape));
        self
    }

    /// The region.
    pub fn region(&self) -> BodyRegion {
        self.region
    }

    /// The superellipse exponent of its rings.
    pub fn exponent(&self) -> f64 {
        self.exponent
    }

    /// The offset from the fit, metres.
    pub fn offset(&self) -> f64 {
        self.offset
    }

    /// The region's weights.
    pub fn weights(&self) -> &RegionWeights {
        &self.weights
    }

    /// The plain ring at `t` (before any shaping).
    ///
    /// # Errors
    /// As [`ring_at`].
    pub fn ring(&self, t: f64) -> Result<Ring, GarmentError> {
        if let Some(ring) = self.rings.lock().ok().and_then(|rings| rings.get(&t.to_bits()).copied()) {
            return Ok(ring);
        }
        let ring = ring_at(self.fit.as_ref(), self.region, t, self.exponent, self.offset)?;
        if let Ok(mut rings) = self.rings.lock() {
            rings.insert(t.to_bits(), ring);
        }
        Ok(ring)
    }

    /// The point at `(t, phi)`.
    ///
    /// # Errors
    /// As [`ring_at`]; [`GarmentError::Invalid`] if the shaping gives a non-finite point.
    pub fn point(&self, t: f64, phi: f64) -> Result<DVec3, GarmentError> {
        let ring = self.ring(t)?;
        self.shaped(&ring, t, phi)
    }

    fn shaped(&self, ring: &Ring, t: f64, phi: f64) -> Result<DVec3, GarmentError> {
        let p = self.shape.as_ref().map_or_else(|| ring.point(phi), |shape| shape(ring, t, phi));
        if p.is_finite() {
            Ok(p)
        } else {
            Err(GarmentError::invalid("shape", format!("the point at t = {t}, phi = {phi} is not finite")))
        }
    }

    /// The point at `(t, phi)` with its frame. The tangents are central differences (one-sided at `t` = 0 and 1), and
    /// the normal is their cross product turned away from the ring's centre.
    ///
    /// # Errors
    /// As [`Surface::point`]; [`GarmentError::Invalid`] where the surface is degenerate (a tangent of no length, or
    /// two parallel tangents).
    pub fn at(&self, t: f64, phi: f64) -> Result<SurfacePoint, GarmentError> {
        let ring = self.ring(t)?;
        let point = self.shaped(&ring, t, phi)?;
        let (t0, t1) = ((t - STEP).max(0.0), (t + STEP).min(1.0));
        let along = (self.point(t1, phi)? - self.point(t0, phi)?).try_normalize();
        let around = (self.shaped(&ring, t, phi + STEP)? - self.shaped(&ring, t, phi - STEP)?).try_normalize();
        let (Some(along), Some(around)) = (along, around) else {
            return Err(GarmentError::invalid("surface", format!("degenerate at t = {t}, phi = {phi}")));
        };
        let Some(mut normal) = around.cross(along).try_normalize() else {
            return Err(GarmentError::invalid("surface", format!("its tangents are parallel at t = {t}, phi = {phi}")));
        };
        if normal.dot(point - ring.centre) < 0.0 {
            normal = -normal;
        }
        Ok(SurfacePoint { point, normal, along, around })
    }

    /// Where `p` lies over the surface: of the places along the region whose section plane passes through `p`, the
    /// one where `p` is nearest the plain ring, with the angle `p` is at in that plane and how far outside the ring it
    /// is along the ray from the centre (negative inside). `None` if no section plane passes through `p`. The garment's
    /// own shaping is not taken into account.
    ///
    /// # Errors
    /// As [`ring_at`], for a section the region lacks.
    #[allow(clippy::many_single_char_names)] // a point, two rings, their heights: the geometry's own names
    pub fn project(&self, p: DVec3) -> Result<Option<Projection>, GarmentError> {
        let height = |ring: &Ring| (p - ring.centre).dot(ring.axis);
        let mut rings = Vec::with_capacity(PROJECT_SAMPLES + 1);
        for k in 0..=PROJECT_SAMPLES {
            rings.push(self.ring(crate::kit::curve::fraction(k, PROJECT_SAMPLES))?);
        }
        let mut best: Option<Projection> = None;
        for pair in rings.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            let (ha, hb) = (height(a), height(b));
            if ha.signum() == hb.signum() && ha != 0.0 {
                continue;
            }
            let (mut lo, mut hi) = (a.t, b.t);
            for _ in 0..PROJECT_BISECTIONS {
                let mid = f64::midpoint(lo, hi);
                if height(&self.ring(mid)?).signum() == ha.signum() {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            let t = f64::midpoint(lo, hi);
            let ring = self.ring(t)?;
            let d = p - ring.centre;
            let q = DVec2::new(d.dot(ring.across), d.dot(ring.through));
            let (phi, outside) = ring.along_ray(q);
            if best.is_none_or(|b| outside.abs() < b.outside.abs()) {
                best = Some(Projection { t, phi, outside });
            }
        }
        Ok(best)
    }

    /// The surface between `t0` and `t1` as a grid of `rings` rows and `segs` segments round a full turn (the last
    /// column repeats the first).
    ///
    /// # Errors
    /// As [`Surface::loft_span`].
    pub fn loft(&self, t0: f64, t1: f64, rings: usize, segs: usize) -> Result<Loft, GarmentError> {
        self.loft_span((t0, t1), (0.0, TAU), rings, segs)
    }

    /// The surface between `t.0` and `t.1` and the angles `phi.0` to `phi.1` (a panel; a full turn wraps), as a grid of
    /// `rings` rows and `segs + 1` columns.
    ///
    /// # Errors
    /// [`GarmentError::Invalid`] for fewer than 2 rings or 1 segment, more than [`MAX_LOFT_SIZE`], a `t` outside
    /// `0..=1` or an empty range; otherwise as [`Surface::point`].
    pub fn loft_span(&self, t: (f64, f64), phi: (f64, f64), rings: usize, segs: usize) -> Result<Loft, GarmentError> {
        if !(2..=MAX_LOFT_SIZE).contains(&rings) || !(1..=MAX_LOFT_SIZE).contains(&segs) {
            return Err(GarmentError::invalid(
                "loft",
                format!(
                    "{rings} rings and {segs} segments: use 2..={MAX_LOFT_SIZE} rings and 1..={MAX_LOFT_SIZE} segments"
                ),
            ));
        }
        let in_unit = |v: f64| v.is_finite() && (0.0..=1.0).contains(&v);
        if !(in_unit(t.0) && in_unit(t.1) && t.0 < t.1) {
            return Err(GarmentError::invalid("loft", format!("t from {} to {} must rise within 0..=1", t.0, t.1)));
        }
        if !(phi.0.is_finite() && phi.1.is_finite() && phi.0 < phi.1 && phi.1 - phi.0 <= TAU) {
            return Err(GarmentError::invalid(
                "loft",
                format!("phi from {} to {} must rise by at most a turn", phi.0, phi.1),
            ));
        }
        let full_turn = (phi.1 - phi.0 - TAU).abs() < 1e-12;
        let step = |k: usize, n: usize, (a, b): (f64, f64)| a + (b - a) * crate::kit::curve::fraction(k, n);
        let mut ts = Vec::with_capacity(rings);
        let mut centres = Vec::with_capacity(rings);
        let mut points = Vec::with_capacity(rings * (segs + 1));
        for i in 0..rings {
            let ti = step(i, rings - 1, t);
            let ring = self.ring(ti)?;
            for j in 0..=segs {
                // A full turn repeats its first point exactly, so the seam welds.
                let pj = if full_turn && j == segs { phi.0 } else { step(j, segs, phi) };
                points.push(self.shaped(&ring, ti, pj)?);
            }
            ts.push(ti);
            centres.push(ring.centre);
        }
        let grid = Grid::new(rings, segs + 1, points)?;
        Ok(Loft { grid, ts, centres, phi, full_turn, skins: Vec::new(), weights: self.weights.clone() }.with_skins())
    }
}

/// [`Surface::loft`] in one call: the rings of `region` of `fit` from `t0` to `t1`, `rings` rows by `segs` segments,
/// each point from `shape(ring, t, phi)` (pass `|ring, _, phi| ring.point(phi)` for the plain rings), at `offset` with
/// `exponent`.
///
/// # Errors
/// As [`Surface::new`] and [`Surface::loft`].
#[allow(clippy::too_many_arguments)] // the prototype's loft signature plus the offset and cut every garment chooses
pub fn loft_region(
    fit: Arc<dyn BodyFit>,
    region: BodyRegion,
    (t0, t1): (f64, f64),
    rings: usize,
    segs: usize,
    exponent: f64,
    offset: f64,
    shape: impl Fn(&Ring, f64, f64) -> DVec3 + Send + Sync + 'static,
) -> Result<Loft, GarmentError> {
    Surface::new(fit, region, exponent, offset)?.with_shape(shape).loft(t0, t1, rings, segs)
}

/// `n` as a float; grid sizes are far below 2^53.
#[allow(clippy::cast_precision_loss)]
fn count(n: usize) -> f64 {
    n as f64
}

/// A lofted surface: its grid, where each row sits along the region, and the skin of each row.
#[derive(Clone, Debug)]
pub struct Loft {
    /// The points: one row per ring.
    pub grid: Grid,
    /// The `t` of each row.
    pub ts: Vec<f64>,
    /// The centre of each row's ring: the inside the normals point away from.
    pub centres: Vec<DVec3>,
    /// The angles the columns span.
    pub phi: (f64, f64),
    /// Whether the columns go round a full turn (the last repeats the first).
    pub full_turn: bool,
    skins: Vec<Vec<(JointRef, f32)>>,
    weights: RegionWeights,
}

impl Loft {
    fn with_skins(mut self) -> Self {
        self.skins = self.ts.iter().map(|&t| self.weights.at(t)).collect();
        self
    }

    /// The skin weights of row `i`.
    pub fn skin(&self, i: usize) -> &[(JointRef, f32)] {
        self.skins.get(i).map_or(&[], Vec::as_slice)
    }

    /// UVs that count fabric tiles of `tile` metres: `u` round the ring (rounded to whole tiles on a full turn, so the
    /// seam matches) and `v` along the region by the length of the centre line. `None` gives `0..=1` both ways.
    pub fn uvs(&self, tile: Option<f64>) -> Vec<DVec2> {
        let (rows, cols) = (self.grid.rows(), self.grid.cols());
        let Some(tile) = tile.filter(|t| t.is_finite() && *t > 0.0) else {
            return (0..rows * cols)
                .map(|k| {
                    DVec2::new(
                        crate::kit::curve::fraction(k % cols, cols - 1),
                        crate::kit::curve::fraction(k / cols, rows - 1),
                    )
                })
                .collect();
        };
        let width: f64 = (0..rows)
            .map(|i| (1..cols).map(|j| self.grid.at(i, j).distance(self.grid.at(i, j - 1))).sum::<f64>())
            .sum::<f64>()
            / count(rows);
        let across = if self.full_turn { (width / tile).round().max(1.0) } else { width / tile };
        let mut v = 0.0;
        let mut out = Vec::with_capacity(rows * cols);
        for i in 0..rows {
            if i > 0 {
                v += self.centres[i].distance(self.centres[i - 1]) / tile;
            }
            out.extend((0..cols).map(|j| DVec2::new(across * crate::kit::curve::fraction(j, cols - 1), v)));
        }
        out
    }

    /// Adds the loft to `builder`: wound outward from the ring centres, wrapped on a full turn, skinned by row, with
    /// the UVs of [`Loft::uvs`] for `tile`.
    ///
    /// # Errors
    /// As [`MeshBuilder::add_grid`].
    pub fn add_to(&self, builder: &mut MeshBuilder, tile: Option<f64>) -> Result<(), GarmentError> {
        let uvs = self.uvs(tile);
        let cols = self.grid.cols();
        let inside = |i: usize, _: usize| self.centres[i];
        let uv = |i: usize, j: usize| uvs[i * cols + j];
        let skin = |_: DVec3, i: usize, _: usize| self.skin(i).to_vec();
        let options = GridOptions {
            wrap_j: self.full_turn,
            inside: Some(&inside),
            uv: Some(&uv),
            skin: Skin::Per(&skin),
            ..GridOptions::default()
        };
        builder.add_grid(&self.grid, &options)?;
        Ok(())
    }
}
