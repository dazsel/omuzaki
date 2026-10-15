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

use std::sync::Arc;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{BodyFit, BodyRegion, JointRef};
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
/// [`GarmentError::NoSection`] if the fit has none there; [`GarmentError::Invalid`] for a non-finite `t` or `offset`, an
/// exponent outside [`MIN_EXPONENT`]..=[`MAX_EXPONENT`], or a ring whose radii come out below [`MIN_RADIUS`] or
/// not finite.
pub fn ring_at(
    fit: &dyn BodyFit,
    region: BodyRegion,
    t: f64,
    exponent: f64,
    offset: f64,
) -> Result<Ring, GarmentError> {
    if !t.is_finite() || !offset.is_finite() {
        return Err(GarmentError::invalid("ring", format!("t = {t} and offset = {offset} must be finite")));
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
    Ok(Ring {
        region,
        t,
        centre: section.centre.as_dvec3(),
        axis,
        across: across.as_dvec3(),
        through: through.as_dvec3(),
        radii,
        exponent,
    })
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
        Ok(Self { fit, region, exponent, offset, shape: None, weights })
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
        ring_at(self.fit.as_ref(), self.region, t, self.exponent, self.offset)
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
    /// As [`Surface::point`]; [`GarmentError::Invalid`] where the surface is degenerate (a tangent of no length).
    pub fn at(&self, t: f64, phi: f64) -> Result<SurfacePoint, GarmentError> {
        let ring = self.ring(t)?;
        let point = self.shaped(&ring, t, phi)?;
        let (t0, t1) = ((t - STEP).max(0.0), (t + STEP).min(1.0));
        let along = (self.point(t1, phi)? - self.point(t0, phi)?).try_normalize();
        let around = (self.shaped(&ring, t, phi + STEP)? - self.shaped(&ring, t, phi - STEP)?).try_normalize();
        let (Some(along), Some(around)) = (along, around) else {
            return Err(GarmentError::invalid("surface", format!("degenerate at t = {t}, phi = {phi}")));
        };
        let mut normal = around.cross(along).normalize_or_zero();
        if normal.dot(point - ring.centre) < 0.0 {
            normal = -normal;
        }
        Ok(SurfacePoint { point, normal, along, around })
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
