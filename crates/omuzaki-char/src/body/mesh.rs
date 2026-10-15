//! The body mesh: every region of a [`BodyLayout`] lofted from its sections.
//!
//! A region (torso, arm, leg, foot) is a stack of elliptical **rings**, one per row, taken from the layout at evenly
//! spaced fractions along the region's axis. [`ring_grid`] turns the rings into the prototype's point grid: the
//! rings, then a rounded **cap** past each end (a quarter ellipse that closes the region; the prototype's `dome` on the
//! thigh and its `base` under the boot). [`surface`] turns the grid into a mesh: smooth normals, wound outward, and
//! each collapsed end row replaced by one pole vertex and a triangle fan, so no triangle is degenerate.
//!
//! The loft is the port of `body.js` `buildLeg` (`loftY` over `LEG.cx/cz/rx/rz`, with the `dome` and `bot` caps) and of
//! `uniform.js`'s torso loft, generalised to any axis: a ring's frame comes from the layout's
//! [`section_basis`], so a tilted arm or a leaning foot lofts the same way as a vertical leg. Regions are separate
//! closed surfaces; where they overlap (a thigh inside the pelvis, an arm inside the shoulder) the cap lies inside the
//! other region, and [`junction`] shapes the rings so the outline runs through the joint without a step.
//!
//! Skin weights come from the capsule kit ([`SegmentSet::skin_mesh`]) over the bones that can move each region.

use omuzaki_char_api::{BodyRegion, JointRef, MaterialName, PartMesh};
use omuzaki_core::glam::{DVec2, DVec3, Vec2};
use omuzaki_core::mesh::MeshData;

use super::junction;
use crate::kit::KitError;
use crate::kit::curve::{TAU, fraction};
use crate::kit::grid::{Grid, GridOptions, grid_normals};
use crate::kit::skin::{CapsuleParams, SegmentSet, smooth};
use crate::template::layout::{BodyLayout, section_basis};

/// How finely a body is tessellated.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    /// Segments once round a ring.
    pub around: usize,
    /// Rings along the torso, between its caps.
    pub torso_rows: usize,
    /// Rings along an arm.
    pub arm_rows: usize,
    /// Rings along a leg.
    pub leg_rows: usize,
    /// Rings along a foot.
    pub foot_rows: usize,
    /// Rings in each cap, the last one collapsed to a pole.
    pub cap_rings: usize,
}

impl Detail {
    /// Real-time use: about 5 600 triangles for a whole body (the §10 budget leaves 6 000).
    pub const LIVE: Self = Self { around: 22, torso_rows: 14, arm_rows: 12, leg_rows: 22, foot_rows: 8, cap_rings: 2 };
    /// Offline renders and export: twice as many rings and segments, and rounder caps.
    pub const OFFLINE: Self =
        Self { around: 44, torso_rows: 28, arm_rows: 24, leg_rows: 44, foot_rows: 16, cap_rings: 6 };

    /// The detail for a build quality.
    pub const fn of(quality: omuzaki_char_api::Quality) -> Self {
        match quality {
            omuzaki_char_api::Quality::Live => Self::LIVE,
            omuzaki_char_api::Quality::Offline => Self::OFFLINE,
        }
    }

    /// Rings along `region`, between its caps.
    pub fn rows(&self, region: BodyRegion) -> usize {
        match region {
            BodyRegion::Torso | BodyRegion::Neck => self.torso_rows,
            BodyRegion::ArmL | BodyRegion::ArmR => self.arm_rows,
            BodyRegion::LegL | BodyRegion::LegR => self.leg_rows,
            _ => self.foot_rows,
        }
    }
}

/// One elliptical cross-section in space: where a row of the grid lies.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Ring {
    /// Centre of the ellipse.
    pub centre: DVec3,
    /// The region's axis here (unit), towards the region's end.
    pub axis: DVec3,
    /// Direction of `radii.x`.
    pub across: DVec3,
    /// Direction of `radii.y`.
    pub through: DVec3,
    /// Half-extents along `across` and `through`.
    pub radii: DVec2,
}

impl Ring {
    /// The point at angle `phi`: 0 is `through` (the front, +Z, for an upright region) and a quarter turn is `across`
    /// (+X), like the prototype's `[cx + rx sin(ph), y, cz + rz cos(ph)]`.
    pub fn point(&self, phi: f64) -> DVec3 {
        let (s, c) = phi.sin_cos();
        self.centre + self.across * (self.radii.x * s) + self.through * (self.radii.y * c)
    }
}

/// How a region closes past one end: a quarter ellipse `length` deep, in `rings` rings.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Cap {
    /// How far past the end ring the pole lies, metres.
    pub length: f64,
    /// Rings, the last one a point. At least 1.
    pub rings: usize,
}

/// A cap's depth as a fraction of the end ring's mean half-width, for ends a garment or a neighbour hides, and for
/// ends that show.
const HIDDEN_CAP: f64 = 0.75;
const SHOWN_CAP: f64 = 0.45;

/// The rings of `region` at `rows` fractions from its start to its end, crowded towards the ends ([`clustered`]).
///
/// # Errors
/// [`KitError`] if the layout has no such region or `rows < 2`.
pub fn sample_rings(layout: &BodyLayout, region: BodyRegion, rows: usize) -> Result<Vec<Ring>, KitError> {
    if rows < 2 {
        return Err(KitError::TooFew { what: "rings of a region", need: 2, got: rows });
    }
    let ts: Vec<f64> = (0..rows).map(|k| clustered(fraction(k, rows - 1))).collect();
    rings_at(layout, region, &ts)
}

/// The rings of `region` at the given fractions of its length (clamped to `0..=1`), with its centre line rounded at
/// corners.
///
/// # Errors
/// [`KitError::Degenerate`] if the layout has no such region.
pub fn rings_at(layout: &BodyLayout, region: BodyRegion, ts: &[f64]) -> Result<Vec<Ring>, KitError> {
    let Some(r) = layout.region(region) else {
        return Err(KitError::Degenerate { what: "a region the layout does not have" });
    };
    let length = r.length();
    Ok(ts
        .iter()
        .map(|&t| {
            let mut s = r.section(t);
            let t = t.clamp(0.0, 1.0);
            // Only the polyline's corner is rounded: the section curves' offsets from the centre line are left as they are.
            s.centre += smoothed_centre(|u| r.path_point(u), t, length) - r.path_point(t);
            let (across, through) = section_basis(region, s.axis.as_vec3());
            Ring {
                centre: s.centre,
                axis: s.axis,
                across: across.as_dvec3(),
                through: through.as_dvec3(),
                radii: s.radii,
            }
        })
        .collect())
}

/// How strongly rows crowd towards a region's ends: the end spacing is `1 - CLUSTER` of the even spacing. The ends are
/// where regions meet and the outline bends, so that is where the rings are needed.
const CLUSTER: f64 = 0.4;

/// The fraction along a region of the ring at even fraction `t`: `t - CLUSTER sin(2 pi t) / 2 pi`, which is monotone,
/// fixes `0`, `1/2` and `1`, and spaces the rows closer at both ends.
fn clustered(t: f64) -> f64 {
    t - CLUSTER * (TAU * t).sin() / TAU
}

/// How far, in metres, a region's centre line is rounded at a corner (the layout's axis blends over the same window; its
/// centre line stays a polyline, so a bent knee or elbow would show a corner in the outline).
const CORNER_WINDOW: f64 = 0.02;
/// Samples averaged across the window.
const CORNER_SAMPLES: usize = 9;

/// The centre of the ring at fraction `t`, averaged over [`CORNER_WINDOW`] either side (less near an end, so the end
/// rings stay where the layout puts them): a box filter, which turns a polyline's corner into a smooth bend and leaves a
/// straight run alone.
fn smoothed_centre(centre: impl Fn(f64) -> DVec3, t: f64, length: f64) -> DVec3 {
    let half = (CORNER_WINDOW / length).min(t).min(1.0 - t);
    if half <= 0.0 {
        return centre(t);
    }
    let sum: DVec3 = (0..CORNER_SAMPLES).map(|i| centre(t - half + 2.0 * half * fraction(i, CORNER_SAMPLES - 1))).sum();
    sum / fraction(CORNER_SAMPLES, 1)
}

/// The caps a region gets: the start of the torso, an arm and a leg lies inside the body (hidden), the end of an arm
/// or a leg lies in a hand or a foot (hidden by the foot or the hand); a torso's end lies inside the neck; a foot's
/// ends show, so they are rounder.
pub fn caps_of(region: BodyRegion, rings: &[Ring], cap_rings: usize) -> (Cap, Cap) {
    let width = |ring: &Ring| f64::midpoint(ring.radii.x, ring.radii.y);
    let first = rings.first().map_or(0.0, width);
    let last = rings.last().map_or(0.0, width);
    let (start, end) = match region {
        BodyRegion::FootL | BodyRegion::FootR => (SHOWN_CAP, SHOWN_CAP),
        _ => (HIDDEN_CAP, SHOWN_CAP),
    };
    (Cap { length: start * first, rings: cap_rings }, Cap { length: end * last, rings: cap_rings })
}

/// The ring a cap's `k`-th row (of `cap.rings`) lies on, past `end` in direction `outward` (`+1` past the end of the
/// axis, `-1` before its start): a quarter ellipse, so the radii fall with the cosine of the angle and the centre
/// advances with its sine; the last row is the pole.
fn cap_ring(end: &Ring, cap: Cap, k: usize, outward: f64) -> Ring {
    let angle = fraction(k, cap.rings) * std::f64::consts::FRAC_PI_2;
    let (advance, shrink) = if k == cap.rings { (cap.length, 0.0) } else { (cap.length * angle.sin(), angle.cos()) };
    Ring { centre: end.centre + end.axis * (outward * advance), radii: end.radii * shrink, ..*end }
}

/// The prototype's point grid: the rings with a cap past each end, `around + 1` points per row (the last repeats the
/// first). Rows run from the start cap's pole to the end cap's pole.
///
/// # Errors
/// [`KitError`] for fewer than 2 rings, `around < 3`, a cap with no rings, or a non-finite point.
pub fn ring_grid(rings: &[Ring], start: Cap, end: Cap, around: usize) -> Result<(Grid, Vec<DVec3>), KitError> {
    let (Some(first), Some(last)) = (rings.first(), rings.last()) else {
        return Err(KitError::TooFew { what: "rings of a region", need: 2, got: 0 });
    };
    if rings.len() < 2 || around < 3 || start.rings < 1 || end.rings < 1 {
        return Err(KitError::TooFew { what: "rings, segments or cap rings", need: 2, got: rings.len().min(around) });
    }
    let all: Vec<Ring> = (1..=start.rings)
        .rev()
        .map(|k| cap_ring(first, start, k, -1.0))
        .chain(rings.iter().copied())
        .chain((1..=end.rings).map(|k| cap_ring(last, end, k, 1.0)))
        .collect();
    let centres = all.iter().map(|r| r.centre).collect();
    let grid = Grid::from_fn(all.len(), around + 1, |i, j| all[i].point(fraction(j, around) * TAU))?;
    Ok((grid, centres))
}

/// `k / n` as the `f32` of a texture coordinate.
#[allow(clippy::cast_possible_truncation)] // a fraction in 0..=1; f32 is the mesh's precision
fn unit_f32(k: usize, n: usize) -> f32 {
    fraction(k, n) as f32
}

/// A region's mesh from its point grid: smooth normals oriented away from the row centres, the collapsed first and last
/// rows replaced by single pole vertices with a triangle fan around them, UVs (`u` round, `v` along), and vertex
/// colours from `shade` when given. Unskinned: [`skin_region`] weights it.
///
/// # Errors
/// [`KitError`] from the normals (a grid wrapped round with fewer than 3 columns).
pub fn surface(grid: &Grid, centres: &[DVec3]) -> Result<MeshData, KitError> {
    let inside = |i: usize, _: usize| centres[i];
    let normals = grid_normals(grid, &GridOptions { wrap_j: true, inside: Some(&inside), ..GridOptions::default() })?;
    let (rows, cols) = (grid.rows(), grid.cols());
    let pole = |i: usize| (0..cols).all(|j| grid.at(i, j).distance_squared(grid.at(i, 0)) < 1e-18);
    let (top_pole, bottom_pole) = (pole(0), pole(rows - 1));
    // The rows that keep all their points; a collapsed end row becomes one vertex instead.
    let (first, last) = (usize::from(top_pole), rows - usize::from(bottom_pole));
    let capacity = (last - first) * cols + usize::from(top_pole) + usize::from(bottom_pole);
    let mut mesh = MeshData::default();
    mesh.positions.reserve(capacity);
    let mut push = |p: DVec3, n: DVec3, uv: Vec2| {
        mesh.positions.push(p.as_vec3());
        mesh.normals.push(n.as_vec3());
        mesh.uvs.push(uv);
    };
    if top_pole {
        push(grid.at(0, 0), normals.normals[0], Vec2::new(0.5, 0.0));
    }
    for i in first..last {
        for j in 0..cols {
            push(grid.at(i, j), normals.normals[i * cols + j], Vec2::new(unit_f32(j, cols - 1), unit_f32(i, rows - 1)));
        }
    }
    if bottom_pole {
        push(grid.at(rows - 1, 0), normals.normals[(rows - 1) * cols], Vec2::new(0.5, 1.0));
    }
    let index = |n: usize| u32::try_from(n).map_err(|_| KitError::TooMany { what: "vertices", max: u32::MAX as usize });
    // The index of the first point of ring `i`, and of the poles.
    let ring = |i: usize| index(usize::from(top_pole) + (i - first) * cols);
    let (top, bottom) = (0_u32, index(capacity - 1)?);
    let flipped = normals.flipped;
    for i in first..last - 1 {
        let (above, below) = (ring(i)?, ring(i + 1)?);
        for j in 0..index(cols - 1)? {
            let (a, b) = (above + j, below + j);
            mesh.indices.extend(if flipped { [a, a + 1, b, b, a + 1, b + 1] } else { [a, b, a + 1, b, b + 1, a + 1] });
        }
    }
    for j in 0..index(cols - 1)? {
        if top_pole {
            let a = ring(first)? + j;
            mesh.indices.extend(if flipped { [a, top, a + 1] } else { [a, a + 1, top] });
        }
        if bottom_pole {
            let a = ring(last - 1)? + j;
            mesh.indices.extend(if flipped { [a, a + 1, bottom] } else { [a, bottom, a + 1] });
        }
    }
    Ok(mesh)
}

/// The bones whose capsules compete for the vertices of `region`: the ones that can move it or the body it lies in.
pub fn bones_of(region: BodyRegion) -> Vec<JointRef> {
    use omuzaki_anim_api::HumanoidBone as B;
    let side = |left: bool, l: B| if left { l } else { l.mirrored() };
    let mut bones: Vec<B> = match region {
        BodyRegion::Torso | BodyRegion::Neck => vec![B::Hips, B::Spine, B::Chest, B::UpperChest, B::Neck],
        BodyRegion::ArmL | BodyRegion::ArmR => {
            let left = region == BodyRegion::ArmL;
            vec![
                side(left, B::LeftShoulder),
                side(left, B::LeftUpperArm),
                side(left, B::LeftLowerArm),
                side(left, B::LeftHand),
            ]
        }
        BodyRegion::LegL | BodyRegion::LegR => {
            let left = region == BodyRegion::LegL;
            vec![
                side(left, B::LeftUpperLeg),
                side(left, B::LeftLowerLeg),
                side(left, B::LeftFoot),
                side(left, B::LeftToes),
            ]
        }
        _ => {
            let left = region == BodyRegion::FootL;
            vec![side(left, B::LeftLowerLeg), side(left, B::LeftFoot), side(left, B::LeftToes)]
        }
    };
    bones.dedup();
    bones.into_iter().map(JointRef::Humanoid).collect()
}

/// Skins `mesh` to the bones of `region` with the capsule kit.
///
/// # Errors
/// [`KitError`] from the kit.
pub fn skin_region(
    segments: &SegmentSet,
    region: BodyRegion,
    mesh: &mut PartMesh,
    params: CapsuleParams,
) -> Result<(), KitError> {
    segments.skin_mesh(mesh, &bones_of(region), params)?;
    let arm = match region {
        BodyRegion::ArmL => Some(true),
        BodyRegion::ArmR => Some(false),
        _ => None,
    };
    let pair = arm.map(|left| {
        let side = |l: omuzaki_anim_api::HumanoidBone| JointRef::Humanoid(if left { l } else { l.mirrored() });
        (side(omuzaki_anim_api::HumanoidBone::LeftShoulder), side(omuzaki_anim_api::HumanoidBone::LeftUpperArm))
    });
    if let Some((shoulder, upper_arm)) = &pair {
        reshape_hand_over(mesh, shoulder, upper_arm, |share| {
            let x = ((share - 0.5) / (2.0 * HAND_OVER_HALF_BAND) + 0.5).clamp(0.0, 1.0);
            x * x * (3.0 - 2.0 * x)
        });
    }
    smooth(mesh, SMOOTH_PASSES);
    if let Some((shoulder, upper_arm)) = &pair {
        reshape_hand_over(mesh, shoulder, upper_arm, |share| if share < HAND_OVER_TAIL { 0.0 } else { share });
    }
    Ok(())
}

/// Passes of the kit's weight smoothing after the capsule weights: they take the sharpest hand-overs (where four bones
/// meet behind a shoulder) down to what a body of this tessellation can show.
const SMOOTH_PASSES: u32 = 4;

/// The half-width of the arm's shoulder-to-upper-arm hand-over, in the shoulder's share of the weight the two bones hold
/// together: before the smoothing, that share is passed through a smoothstep running from `0.5 - HAND_OVER_HALF_BAND`
/// to `0.5 + HAND_OVER_HALF_BAND`. The capsule fall-off alone leaves the shoulder a share of 0.25 to 0.45 halfway down
/// the upper arm, which a raised arm blends a still clavicle into over most of its length (#709). The smoothing that
/// follows keeps the crease soft: it is the kit's, and by its own rule ends no sharper than the sharpened weights began.
const HAND_OVER_HALF_BAND: f32 = 0.1;

/// The share under which the smoothing's tail of shoulder weight is dropped after it, so the shoulder bone's weight is
/// exactly zero a few rings down the arm and not a faint trace along it.
const HAND_OVER_TAIL: f32 = 0.1;

/// Replaces, at every vertex that has both bones, the shoulder's share of their joint weight by `shape(share)`: the
/// weight the two bones hold together stays, so every sum and every other bone's weight is unchanged. A vertex with only
/// one of the two bones is left alone.
fn reshape_hand_over(mesh: &mut PartMesh, shoulder: &JointRef, upper_arm: &JointRef, shape: impl Fn(f32) -> f32) {
    let slot = |j: &JointRef| mesh.joint_bones.iter().position(|b| b == j).and_then(|i| u16::try_from(i).ok());
    let (Some(shoulder), Some(upper)) = (slot(shoulder), slot(upper_arm)) else { return };
    let g = &mut mesh.geometry;
    for (joints, weights) in g.joints.iter().zip(g.weights.iter_mut()) {
        let on =
            |bone: u16| -> f32 { joints.iter().zip(&*weights).filter(|(j, _)| **j == bone).map(|(_, w)| *w).sum() };
        let (s, u) = (on(shoulder), on(upper));
        let both = s + u;
        if s <= 0.0 || u <= 0.0 {
            continue;
        }
        let kept = shape(s / both).clamp(0.0, 1.0);
        let (new_s, new_u) = (both * kept, both * (1.0 - kept));
        // The first slot of each bone takes its new weight; a duplicate slot (never produced by the kit) is zeroed.
        let (mut seen_s, mut seen_u) = (false, false);
        for (j, w) in joints.iter().zip(weights.iter_mut()) {
            if *j == shoulder && *w > 0.0 {
                *w = if std::mem::replace(&mut seen_s, true) { 0.0 } else { new_s };
            } else if *j == upper && *w > 0.0 {
                *w = if std::mem::replace(&mut seen_u, true) { 0.0 } else { new_u };
            }
        }
    }
}

/// The capsule falloff for `region` in a body of this size. Limbs use the prototype's `1 / (d + e)^4` with `e` = 12 mm
/// (the legs of `body.js` were skinned so), grown with the body (the head base's height over soft-sd's): a sharp hand-over
/// keeps a bent joint's volume. The torso's bones lie close together, so a tessellated torso hands over between them in
/// a few rings and needs a gentler fall-off to stay within the smooth-weights bound.
pub fn capsule_params(layout: &BodyLayout, region: BodyRegion) -> CapsuleParams {
    let unit = layout.head_base().map_or(1.0, |p| p.y / SOFT_SD_HEAD_BASE);
    let (power, softening) = match region {
        BodyRegion::Torso | BodyRegion::Neck => (TORSO_POWER, TORSO_SOFTENING),
        _ => (CapsuleParams::PROTOTYPE.power, CapsuleParams::PROTOTYPE.softening),
    };
    CapsuleParams::new(power, softening * unit).unwrap_or_default()
}

/// The height of soft-sd's head base: the unit the body's size is measured in.
const SOFT_SD_HEAD_BASE: f64 = 0.56;
const TORSO_POWER: f64 = 3.0;
const TORSO_SOFTENING: f64 = 0.02;

/// The name of a region's mesh.
pub fn name_of(region: BodyRegion) -> &'static str {
    match region {
        BodyRegion::Torso => "torso",
        BodyRegion::Neck => "neck",
        BodyRegion::ArmL => "arm.L",
        BodyRegion::ArmR => "arm.R",
        BodyRegion::LegL => "leg.L",
        BodyRegion::LegR => "leg.R",
        BodyRegion::FootL => "foot.L",
        _ => "foot.R",
    }
}

/// The regions the body part builds, in mesh order.
pub const REGIONS: [BodyRegion; 7] = [
    BodyRegion::Torso,
    BodyRegion::ArmL,
    BodyRegion::ArmR,
    BodyRegion::LegL,
    BodyRegion::LegR,
    BodyRegion::FootL,
    BodyRegion::FootR,
];

/// Every region of `layout` as a skinned, closed mesh using `material`.
///
/// # Errors
/// [`KitError`] if the layout lacks a region or a mesh breaks a rule.
pub fn build_regions(
    layout: &BodyLayout,
    segments: &SegmentSet,
    detail: Detail,
    material: &MaterialName,
) -> Result<Vec<(BodyRegion, PartMesh)>, KitError> {
    let mut rings = REGIONS
        .iter()
        .map(|&region| sample_rings(layout, region, detail.rows(region)).map(|r| (region, r)))
        .collect::<Result<Vec<_>, _>>()?;
    junction::shape(&mut rings, (detail.arm_rows / 3).max(3), (detail.leg_rows / 3).max(6));
    rings
        .into_iter()
        .map(|(region, rings)| {
            let (start, end) = caps_of(region, &rings, detail.cap_rings);
            let (grid, centres) = ring_grid(&rings, start, end, detail.around)?;
            let geometry = surface(&grid, &centres)?;
            let mut mesh = PartMesh {
                name: name_of(region).to_owned(),
                geometry,
                joint_bones: Vec::new(),
                material: material.clone(),
                casts_shadow: true,
                render_order: 0,
                dynamic: false,
            };
            skin_region(segments, region, &mut mesh, capsule_params(layout, region))?;
            mesh.validate()?;
            Ok((region, mesh))
        })
        .collect()
}

/// Every bone any region is skinned to, once.
pub fn all_bones() -> Vec<JointRef> {
    let mut bones: Vec<JointRef> = Vec::new();
    for bone in REGIONS.iter().flat_map(|&r| bones_of(r)) {
        if !bones.contains(&bone) {
            bones.push(bone);
        }
    }
    bones
}
