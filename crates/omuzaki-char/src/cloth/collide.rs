//! What a garment's cloth collides with (#333): the body under it, the hair's collider proxies, and the garment layer
//! below it, as one [`ColliderSet`] built once per garment and placed from the pose every frame.
//!
//! # The body
//!
//! [`ColliderSet::add_body`] reads the [`BodyFit`] the garment wraps (the bare body, or the envelope of the layer
//! below) and fits, in bind space:
//!
//! - the **torso** as three capsules lying across the body (hips, waist, chest): a stadium inside each elliptical
//!   section, so the flat front and the wide sides are both covered;
//! - a **hips** sphere at the base of the torso;
//! - each **leg** as an upper (hip to knee) and a lower (knee to ankle) capsule, and each **arm** likewise through the
//!   elbow;
//! - a **neck** sphere and a **head** sphere.
//!
//! Each shape starts from the sections it spans and is shrunk until no sampled point of it lies more than
//! [`FIT_TOLERANCE`] outside the body ([`depth_outside`]): colliders sit inside the skin, and the contacts add the
//! cloth's thickness and margin. The head has no section in a [`BodyFit`] (the head part owns it), so its sphere is
//! sized from the top of the neck and is the one shape not checked. A region the fit lacks (a creature) adds nothing.
//!
//! Every body shape follows one bone (the thigh capsule its upper leg, a torso capsule the spine bone under it) through
//! [`PoseSource::skin_matrix`](omuzaki_char_api::PoseSource::skin_matrix), whose matrices the runtime reads once a
//! frame anyway.
//!
//! # Hair
//!
//! [`ColliderSet::add_hair`] takes the collider groups the hair part published ([`BuildCtx::colliders`]): spheres and
//! capsules on humanoid bones, in the bone's space. Humanoid bones rest with identity rotation (the VRM normalised
//! convention) at their joints, so the bind-space shape is the joint plus the offset. Inside shapes and planes are for
//! spring bones and are skipped.
//!
//! # The layer below
//!
//! A robe over a skirt must not pass through it, but two runtimes never read each other's state. Instead
//! [`ColliderSet::add_shell`] turns the inner garment's own grid into a **shell**: rings of capsules around it at a few
//! rows, each end skinned with the inner grid's weights at that vertex, pushed out along the rest normal so the
//! capsules' outer surface sits `offset` outside the inner garment (chord sag included). The outer garment collides with
//! that shell as with the body.
//!
//! # Cost
//!
//! Built once per garment (bind-space shapes fixed; [`ColliderSet::place`] only transforms them). The count is in the
//! runtime's [`RuntimeCost::colliders`](omuzaki_char_api::RuntimeCost) and, by kind, in [`ColliderSet::counts`].
//!
//! [`BuildCtx::colliders`]: omuzaki_char_api::BuildCtx::colliders

// Geometry in the textbook's single-letter names (points `p`, `a`, `b`; section axes `x`, `y`).
#![allow(clippy::many_single_char_names)]

use omuzaki_anim_api::{ColliderGroup, ColliderShape, HumanoidBone};
use omuzaki_char_api::{BodyFit, BodyRegion, JointRef, Section, humanoid_of};
use omuzaki_core::glam::{Affine3A, Vec3};

use super::contact::{ClothEnvironment, Shape};
use super::quality::KeyHasher;
use super::runtime::{ClothGrid, ClothRuntimeError, SkinWeights};
use crate::template::layout::section_basis;

/// How far a body collider may stick out of the body, metres, after fitting.
pub const FIT_TOLERANCE: f32 = 0.0005;

/// Sections sampled along each body region.
const SAMPLES: usize = 48;

/// Where a collider comes from.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ColliderKind {
    /// The body (or the envelope) under the garment.
    Body,
    /// A hair collider proxy.
    Hair,
    /// The shell of the garment layer below.
    Layer,
}

/// Colliders by kind.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ColliderCounts {
    /// Body shapes.
    pub body: usize,
    /// Hair proxies.
    pub hair: usize,
    /// Shell capsules of the layer below.
    pub layer: usize,
}

/// How a shape follows the skeleton.
#[derive(Copy, Clone, Debug, PartialEq)]
enum Bind {
    /// Both ends follow one joint (an index into the set's joints).
    Rigid(u16),
    /// Each end is skinned with its own weights (a shell capsule).
    Skinned(SkinWeights, SkinWeights),
}

/// A capsule (a sphere when both ends meet) in bind space and how it follows the pose.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct BoundShape {
    /// One end of the axis, bind space.
    pub a: Vec3,
    /// The other end.
    pub b: Vec3,
    /// The radius, metres.
    pub radius: f32,
    /// Where it comes from.
    pub kind: ColliderKind,
    bind: Bind,
}

impl BoundShape {
    /// The joint a rigid shape follows (an index into [`ColliderSet::joints`]); `None` for a skinned shell capsule.
    pub fn joint(&self) -> Option<usize> {
        match self.bind {
            Bind::Rigid(j) => Some(usize::from(j)),
            Bind::Skinned(..) => None,
        }
    }
}

/// A garment's colliders (see the [module docs](self)).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ColliderSet {
    shapes: Vec<BoundShape>,
    joints: Vec<JointRef>,
}

impl ColliderSet {
    /// No colliders.
    pub fn new() -> Self {
        Self::default()
    }

    /// The shapes, in placing order.
    pub fn shapes(&self) -> &[BoundShape] {
        &self.shapes
    }

    /// The joints the shapes follow.
    pub fn joints(&self) -> &[JointRef] {
        &self.joints
    }

    /// How many shapes.
    pub fn len(&self) -> usize {
        self.shapes.len()
    }

    /// Whether there are none.
    pub fn is_empty(&self) -> bool {
        self.shapes.is_empty()
    }

    /// Shapes by kind.
    pub fn counts(&self) -> ColliderCounts {
        let mut counts = ColliderCounts::default();
        for s in &self.shapes {
            match s.kind {
                ColliderKind::Body => counts.body += 1,
                ColliderKind::Hair => counts.hair += 1,
                ColliderKind::Layer => counts.layer += 1,
            }
        }
        counts
    }

    fn joint_index(&mut self, joint: &JointRef) -> Option<u16> {
        let i = self.joints.iter().position(|j| j == joint).unwrap_or_else(|| {
            self.joints.push(joint.clone());
            self.joints.len() - 1
        });
        u16::try_from(i).ok()
    }

    /// Adds a capsule from `a` to `b` (bind space) of `radius` that follows `joint`. Returns whether it was added (a
    /// non-finite or non-positive size is refused).
    pub fn add_capsule(&mut self, a: Vec3, b: Vec3, radius: f32, joint: &JointRef, kind: ColliderKind) -> bool {
        if !(a.is_finite() && b.is_finite() && radius.is_finite() && radius > 0.0) {
            return false;
        }
        let Some(j) = self.joint_index(joint) else {
            return false;
        };
        self.shapes.push(BoundShape { a, b, radius, kind, bind: Bind::Rigid(j) });
        true
    }

    /// Adds the body colliders fitted to `fit` (see the [module docs](self)); returns how many.
    pub fn add_body(&mut self, fit: &dyn BodyFit) -> usize {
        let body = Body::sample(fit);
        let before = self.shapes.len();
        let joint_of = |bone| JointRef::Humanoid(bone);
        // Torso: three capsules across the body, each on the spine bone below its centre.
        if let Some(torso) = body.region(BodyRegion::Torso) {
            for t in [0.14, 0.46, 0.78] {
                let s = torso.at(t);
                let (x, _) = section_basis(BodyRegion::Torso, s.axis);
                let (rx, ry) = (s.radii.x.max(s.radii.y), s.radii.x.min(s.radii.y));
                let along = if s.radii.x >= s.radii.y { x } else { s.axis.cross(x).normalize_or_zero() };
                let r = 0.85 * ry;
                let a = ((rx * rx - ry * ry).max(0.0) * (1.0 - 0.85 * 0.85)).sqrt();
                let bone = spine_bone(fit, s.centre);
                self.fit_add(&body, s.centre - along * a, s.centre + along * a, r, &joint_of(bone));
            }
            let s = torso.at(0.04);
            self.fit_add(&body, s.centre, s.centre, 0.9 * s.radii.min_element(), &joint_of(HumanoidBone::Hips));
        }
        let limbs = [
            (BodyRegion::LegL, HumanoidBone::LeftUpperLeg, HumanoidBone::LeftLowerLeg),
            (BodyRegion::LegR, HumanoidBone::RightUpperLeg, HumanoidBone::RightLowerLeg),
            (BodyRegion::ArmL, HumanoidBone::LeftUpperArm, HumanoidBone::LeftLowerArm),
            (BodyRegion::ArmR, HumanoidBone::RightUpperArm, HumanoidBone::RightLowerArm),
        ];
        for (region, upper, lower) in limbs {
            let Some(limb) = body.region(region) else { continue };
            let bend = fit.joint(&joint_of(lower)).map_or(0.5, |p| limb.nearest_t(p)).clamp(0.2, 0.8);
            for (t0, t1, bone) in [(0.03, bend, upper), (bend, 0.97, lower)] {
                let (a, b) = (limb.at(t0).centre, limb.at(t1).centre);
                let r = limb.inscribed(t0, t1, a, b);
                self.fit_add(&body, a, b, r, &joint_of(bone));
            }
        }
        if let Some(neck) = body.region(BodyRegion::Neck) {
            let s = neck.at(0.5);
            self.fit_add(&body, s.centre, s.centre, s.radii.min_element(), &joint_of(HumanoidBone::Neck));
            let top = neck.at(1.0);
            let r = 1.2 * top.radii.max_element();
            let centre = fit.joint(&JointRef::HEAD).unwrap_or(top.centre) + Vec3::Y * r;
            self.add_capsule(centre, centre, r, &JointRef::HEAD, ColliderKind::Body);
        }
        self.shapes.len() - before
    }

    /// Adds a body shape after shrinking it inside the body.
    fn fit_add(&mut self, body: &Body, a: Vec3, b: Vec3, radius: f32, joint: &JointRef) {
        if let Some(r) = body.fit(a, b, radius) {
            self.add_capsule(a, b, r, joint, ColliderKind::Body);
        }
    }

    /// Adds the hair's published collider proxies (spheres and capsules on humanoid bones; see the
    /// [module docs](self)), placed with the joints of `fit`. Returns how many.
    pub fn add_hair(&mut self, groups: &[ColliderGroup], fit: &dyn BodyFit) -> usize {
        let mut added = 0;
        for collider in groups.iter().flat_map(|g| &g.colliders) {
            let Some(bone) = humanoid_of(collider.bone) else { continue };
            let joint = JointRef::Humanoid(bone);
            let Some(origin) = fit.joint(&joint) else { continue };
            let (a, b, radius) = match collider.shape {
                ColliderShape::Sphere { offset, radius } => (offset, offset, radius),
                ColliderShape::Capsule { offset, radius, tail } => (offset, tail, radius),
                _ => continue,
            };
            if self.add_capsule(origin + a, origin + b, radius, &joint, ColliderKind::Hair) {
                added += 1;
            }
        }
        added
    }

    /// Adds the shell of an inner garment's `grid`: `rings` rows of `segments` capsules of `radius` whose outer surface
    /// sits `offset` outside the inner garment (see the [module docs](self)). Returns how many.
    ///
    /// # Errors
    /// [`ClothRuntimeError`] for a malformed grid, fewer than 1 ring or 3 segments (2 for an open grid), more than the
    /// grid has, or a radius or offset that is not finite and positive.
    pub fn add_shell(
        &mut self,
        grid: &ClothGrid,
        rings: usize,
        segments: usize,
        offset: f32,
        radius: f32,
    ) -> Result<usize, ClothRuntimeError> {
        let skin = super::runtime::checked_skin(grid)?;
        let min_segments = if grid.wrap { 3 } else { 2 };
        if rings == 0 || rings > grid.rows || segments < min_segments || segments > grid.cols {
            return Err(ClothRuntimeError::Spec(
                "a shell needs 1 ring to one per row, and 3 segments (2 open) to one per column",
            ));
        }
        if !(offset.is_finite() && offset >= 0.0 && radius.is_finite() && radius > 0.0) {
            return Err(ClothRuntimeError::Spec("a shell's offset must be finite and >= 0 and its radius > 0"));
        }
        let normals = super::runtime::rest_normals(grid);
        let joint_map: Vec<Option<u16>> = grid.joints.iter().map(|j| self.joint_index(j)).collect();
        let remap = |s: &SkinWeights| {
            let mut out = *s;
            for (j, &w) in out.joints.iter_mut().zip(&s.weights) {
                *j = if w > 0.0 { joint_map[usize::from(*j)].unwrap_or(0) } else { 0 };
            }
            out
        };
        let rows: Vec<usize> = spread(rings, grid.rows, false);
        let cols: Vec<usize> = spread(segments, grid.cols, grid.wrap);
        let mut added = 0;
        let links = if grid.wrap { segments } else { segments - 1 };
        for &r in &rows {
            // Axis points: inward of the target surface by the radius, then out by the worst chord sag of the ring.
            let at = |c: usize, push: f32| {
                let v = grid.index(r, c);
                grid.positions[v] + normals[v] * (offset - radius + push)
            };
            let mut push = 0.0_f32;
            for _ in 0..3 {
                let mut worst = 0.0_f32;
                for k in 0..links {
                    let (c0, c1) = (cols[k], cols[(k + 1) % segments]);
                    let span = if c1 > c0 { c1 - c0 } else { c1 + grid.cols - c0 };
                    let (a, b) = (at(c0, push), at(c1, push));
                    for step in 1..span {
                        let v = grid.index(r, (c0 + step) % grid.cols);
                        let target = grid.positions[v] + normals[v] * offset;
                        worst = worst.max(distance_to_segment(target, a, b) - radius);
                    }
                }
                if worst <= 1e-5 {
                    break;
                }
                push += worst;
            }
            for k in 0..links {
                let (c0, c1) = (cols[k], cols[(k + 1) % segments]);
                let (v0, v1) = (grid.index(r, c0), grid.index(r, c1));
                let (a, b) = (at(c0, push), at(c1, push));
                if a.is_finite() && b.is_finite() {
                    let bind = Bind::Skinned(remap(&skin[v0]), remap(&skin[v1]));
                    self.shapes.push(BoundShape { a, b, radius, kind: ColliderKind::Layer, bind });
                    added += 1;
                }
            }
        }
        Ok(added)
    }

    /// Re-indexes the shapes onto `joints` (appending the set's joints it lacks), so the runtime reads every matrix once.
    pub(crate) fn link(&mut self, joints: &mut Vec<JointRef>) -> Result<(), ClothRuntimeError> {
        let mut map = Vec::with_capacity(self.joints.len());
        for joint in &self.joints {
            let i = joints.iter().position(|j| j == joint).unwrap_or_else(|| {
                joints.push(joint.clone());
                joints.len() - 1
            });
            map.push(u16::try_from(i).map_err(|_| ClothRuntimeError::Spec("more than 65535 joints"))?);
        }
        let fix = |s: &mut SkinWeights| {
            for (j, &w) in s.joints.iter_mut().zip(&s.weights) {
                *j = if w > 0.0 { map[usize::from(*j)] } else { 0 };
            }
        };
        for shape in &mut self.shapes {
            match &mut shape.bind {
                Bind::Rigid(j) => *j = map[usize::from(*j)],
                Bind::Skinned(a, b) => {
                    fix(a);
                    fix(b);
                }
            }
        }
        self.joints.clone_from(joints);
        Ok(())
    }

    /// Shape `i` placed by `matrices` (one per joint of [`ColliderSet::joints`]).
    pub fn placed(&self, i: usize, matrices: &[Affine3A]) -> Option<Shape> {
        let s = self.shapes.get(i)?;
        let (a, b, scale) = match s.bind {
            Bind::Rigid(j) => {
                let m = matrices.get(usize::from(j))?;
                (m.transform_point3(s.a), m.transform_point3(s.b), libm::cbrtf(m.matrix3.determinant().abs()))
            }
            Bind::Skinned(wa, wb) => (wa.skin(matrices, s.a.into()).into(), wb.skin(matrices, s.b.into()).into(), 1.0),
        };
        Some(Shape::capsule(a, b, s.radius * scale))
    }

    /// Places every shape in `env` for the frame that ends at `matrices` (calls `begin_frame` first). A shape the
    /// matrices make non-finite is skipped, and so is everything after it (slots fill in order).
    pub(crate) fn place(&self, matrices: &[Affine3A], env: &mut ClothEnvironment) {
        env.begin_frame();
        for i in 0..self.shapes.len() {
            match self.placed(i, matrices) {
                Some(shape) if env.place(i, shape).is_ok() => {}
                _ => {
                    env.truncate(i);
                    break;
                }
            }
        }
    }

    /// Adds the shapes to a key ([`ClothRuntime::key`](super::runtime::ClothRuntime::key)).
    pub fn hash_into(&self, h: &mut KeyHasher) {
        h.usize(self.shapes.len());
        for s in &self.shapes {
            h.vec3(s.a).vec3(s.b).f32(s.radius).str(&format!("{:?}{:?}", s.kind, s.bind));
        }
        for j in &self.joints {
            h.str(&format!("{j:?}"));
        }
    }
}

/// The spine bone the point `p` sits on: of the hips, spine, chest and upper chest the fit has, the highest whose
/// joint is not above `p`.
fn spine_bone(fit: &dyn BodyFit, p: Vec3) -> HumanoidBone {
    let mut best = HumanoidBone::Hips;
    for bone in [HumanoidBone::Spine, HumanoidBone::Chest, HumanoidBone::UpperChest] {
        if fit.joint(&JointRef::Humanoid(bone)).is_some_and(|j| j.y <= p.y) {
            best = bone;
        }
    }
    best
}

/// `n` indices spread evenly over `m`: both ends for an open line, the seam's spacing for a wrapped one.
fn spread(n: usize, m: usize, wrap: bool) -> Vec<usize> {
    if wrap || n < 2 {
        (0..n).map(|j| j * m / n.max(1)).collect()
    } else {
        (0..n).map(|j| (j * (m - 1) * 2 + (n - 1)) / (2 * (n - 1))).collect()
    }
}

/// The distance from `p` to the segment `a`-`b`.
fn distance_to_segment(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let e = b - a;
    let l2 = e.length_squared();
    let s = if l2 > 1e-12 { ((p - a).dot(e) / l2).clamp(0.0, 1.0) } else { 0.0 };
    p.distance(a + e * s)
}

/// A body region's sections, sampled once.
struct Region {
    region: BodyRegion,
    sections: Vec<(f32, Section, Vec3, Vec3)>,
}

impl Region {
    /// The sampled section nearest `t`.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // indices
    fn at(&self, t: f32) -> Section {
        let i = (t.clamp(0.0, 1.0) * (self.sections.len() - 1) as f32).round() as usize;
        self.sections[i.min(self.sections.len() - 1)].1
    }

    /// The `t` of the section whose centre is nearest `p`.
    fn nearest_t(&self, p: Vec3) -> f32 {
        self.sections
            .iter()
            .map(|(t, s, ..)| (*t, s.centre.distance_squared(p)))
            .fold((0.5, f32::MAX), |best, (t, d)| if d < best.1 { (t, d) } else { best })
            .0
    }

    /// The largest radius about the axis `a`-`b` inside every section from `t0` to `t1`: each section's smaller
    /// half-axis less the axis's offset from its centre.
    fn inscribed(&self, t0: f32, t1: f32, a: Vec3, b: Vec3) -> f32 {
        self.sections
            .iter()
            .filter(|(t, ..)| (t0..=t1).contains(t))
            .map(|(_, s, ..)| s.radii.min_element() - distance_to_segment(s.centre, a, b))
            .fold(f32::MAX, f32::min)
    }

    /// How far `p` lies outside the region (negative inside), along the ray from the nearest section's centre, plus
    /// any distance beyond the region's ends.
    fn depth(&self, p: Vec3) -> f32 {
        let mut best: Option<(f32, usize)> = None;
        for (i, (_, s, ..)) in self.sections.iter().enumerate() {
            let along = (p - s.centre).dot(s.axis).abs();
            if best.is_none_or(|(b, _)| along < b) {
                best = Some((along, i));
            }
        }
        let Some((_, i)) = best else { return f32::MAX };
        let (_, s, x, y) = self.sections[i];
        let q = p - s.centre;
        let (u, v) = (q.dot(x), q.dot(y));
        let (rx, ry) = (s.radii.x.max(1e-6), s.radii.y.max(1e-6));
        let rho = ((u / rx).powi(2) + (v / ry).powi(2)).sqrt();
        let planar = (u * u + v * v).sqrt();
        let radial = if rho > 1e-9 { planar * (1.0 - 1.0 / rho) } else { -rx.min(ry) };
        // Beyond an end: how far past the first or last section's plane.
        let (first, last) = (&self.sections[0].1, &self.sections[self.sections.len() - 1].1);
        let before = -(p - first.centre).dot(first.axis);
        let after = (p - last.centre).dot(last.axis);
        radial.max(before).max(after)
    }
}

/// The body regions of a fit, sampled.
struct Body {
    regions: Vec<Region>,
}

impl Body {
    #[allow(clippy::cast_precision_loss)] // sample counts
    fn sample(fit: &dyn BodyFit) -> Self {
        let regions = [
            BodyRegion::Torso,
            BodyRegion::Neck,
            BodyRegion::ArmL,
            BodyRegion::ArmR,
            BodyRegion::LegL,
            BodyRegion::LegR,
        ]
        .into_iter()
        .filter_map(|region| {
            let sections: Vec<_> = (0..SAMPLES)
                .filter_map(|i| {
                    let t = i as f32 / (SAMPLES - 1) as f32;
                    let s = fit.section(region, t)?;
                    let ok = s.centre.is_finite()
                        && s.axis.is_finite()
                        && s.radii.is_finite()
                        && s.radii.min_element() > 0.0;
                    ok.then(|| {
                        let (x, y) = section_basis(region, s.axis);
                        (t, s, x, y)
                    })
                })
                .collect();
            (sections.len() == SAMPLES).then_some(Region { region, sections })
        })
        .collect();
        Self { regions }
    }

    fn region(&self, region: BodyRegion) -> Option<&Region> {
        self.regions.iter().find(|r| r.region == region)
    }

    fn depth(&self, p: Vec3) -> f32 {
        self.regions.iter().map(|r| r.depth(p)).fold(f32::MAX, f32::min)
    }

    /// The largest radius up to `radius` for which the capsule `a`-`b` lies inside the body within [`FIT_TOLERANCE`]
    /// at every sampled point; `None` if it would be under 5 mm.
    fn fit(&self, a: Vec3, b: Vec3, radius: f32) -> Option<f32> {
        if !(radius.is_finite() && radius > 0.0 && a.is_finite() && b.is_finite()) {
            return None;
        }
        let mut r = radius;
        for _ in 0..24 {
            let excess = capsule_points(a, b, r).map(|p| self.depth(p)).fold(f32::MIN, f32::max) - FIT_TOLERANCE;
            if excess <= 0.0 {
                return (r >= 0.005).then_some(r);
            }
            r -= excess + 1e-4;
            if r < 0.005 {
                return None;
            }
        }
        None
    }
}

/// Points on and in the capsule `a`-`b` of radius `r`: 9 stations along the axis, 26 directions from each.
#[allow(clippy::cast_precision_loss)] // small counts
pub fn capsule_points(a: Vec3, b: Vec3, r: f32) -> impl Iterator<Item = Vec3> {
    let directions = (0..27_u8).filter(|&i| i != 13).map(|i| {
        let d = Vec3::new(f32::from(i % 3) - 1.0, f32::from(i / 3 % 3) - 1.0, f32::from(i / 9) - 1.0);
        d.normalize()
    });
    let directions: Vec<Vec3> = directions.collect();
    (0..9).flat_map(move |k| {
        let q = a.lerp(b, k as f32 / 8.0);
        directions.clone().into_iter().map(move |d| q + d * r)
    })
}

/// How far `p` (bind space) lies outside the body `fit` describes, metres (negative inside), measured along the ray
/// from the nearest section's centre: an over-estimate of the true distance outside, so a check against it is
/// conservative. The head is not part of a [`BodyFit`] and counts as outside.
pub fn depth_outside(fit: &dyn BodyFit, p: Vec3) -> f32 {
    Body::sample(fit).depth(p)
}

/// A sampler for [`depth_outside`] that keeps the sections between calls.
pub struct BodyDepth(Body);

impl BodyDepth {
    /// Samples `fit` once.
    pub fn new(fit: &dyn BodyFit) -> Self {
        Self(Body::sample(fit))
    }

    /// As [`depth_outside`].
    pub fn depth(&self, p: Vec3) -> f32 {
        self.0.depth(p)
    }
}
