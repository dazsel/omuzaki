//! The shape field: dials, the age's dials and sculpt strokes as one displacement of a part's vertices, applied in
//! [`build_part`](crate::build_part) after the part is built and scaled to its stature.
//!
//! The field is a function of a vertex's position and its [`Membership`] (its skin weights by region): dials sum at
//! the vertex's rest position, then each stroke adds its own displacement in the order it was made (flatten, pinch and
//! smooth read the surface the strokes before them left). Normals are carried by the field's Jacobian, taken by
//! central differences, so every mesh keeps its own hard and soft edges.
//!
//! The body's fit, the head's surface and every anchor a deformed part publishes are moved by the same field
//! ([`DeformedFit`], [`DeformedHead`]), so garments, hair and accessories built afterwards follow without knowing about
//! it.

#![allow(clippy::many_single_char_names)] // vector geometry: points, normals, weights and fractions

use std::collections::BTreeMap;
use std::fmt::{self, Debug};
use std::sync::Arc;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::glam::{Mat3, Vec2, Vec3};
use omuzaki_face_api::FaceLayout;

use super::frame::{BodyFrame, HeadFrame, Membership, PartFrame, RegionGroup, RegionPath, Row};
use super::sculpt::{Brush, PartPoint, SculptStroke};
use super::{DesignDef, bump, find_dial};
use crate::design::dial::{DialEffect, DialSide};
use crate::error::BuildError;
use crate::ids::{JointRef, PartId};
use crate::kind::{
    AnchorsFact, BodyFit, BodyRegion, FitFact, HeadFact, HeadSurface, PartCategory, PartOutput, Published, RegionEnd,
    Section,
};

crate::fact! {
    /// The body's regions before the design layer moved them (published by the body when the design shapes it): the
    /// frame every later part's dials and strokes are placed in.
    pub BodyFrameFact = "body-frame", crate::fact::Last<Arc<BodyFrame>>;
}

crate::register_fact!(BodyFrameFact);
crate::register_fact_probe!(BodyFrameFact);

crate::fact! {
    /// The head's frame before the design layer moved it (published by the head when the design shapes it).
    pub HeadFrameFact = "head-frame", crate::fact::Last<Arc<HeadFrame>>;
}

crate::register_fact!(HeadFrameFact);
crate::register_fact_probe!(HeadFrameFact);

/// How far apart the samples of the field's Jacobian are, as a fraction of the head's or a limb's scale.
const JACOBIAN_STEP: f32 = 2e-3;
/// Offsets the smooth brush averages over, as fractions of its radius.
const SMOOTH_REACH: f32 = 0.35;

// ------------------------------------------------------------------------------------------------ terms

/// One dial effect, placed.
#[derive(Clone, Debug, PartialEq)]
enum Term {
    /// A radial swelling of a body region.
    Body { path: Arc<RegionPath>, group: RegionGroup, t: f32, span: f32, lobes: [(Vec3, f32); 6], all: f32, gain: f32 },
    /// A bump on the head.
    Head { centre: Vec3, aim: Vec3, reach: f32, push: Option<Vec3>, gain: f32 },
}

impl Term {
    fn displace(&self, p: Vec3, m: &Membership) -> Vec3 {
        match self {
            Self::Body { path, group, t, span, lobes, all, gain } => {
                let w = m.of(*group);
                if w <= 1e-5 {
                    return Vec3::ZERO;
                }
                let at = path.locate(p);
                let b = bump((at.row.t - t) / span);
                if b == 0.0 {
                    return Vec3::ZERO;
                }
                let len = at.radial.length();
                if len < 1e-7 {
                    return Vec3::ZERO;
                }
                let r = at.radial / len;
                let mut lobe = *all;
                for (d, weight) in lobes {
                    if *weight == 0.0 {
                        continue;
                    }
                    let in_plane = *d - at.row.axis * d.dot(at.row.axis);
                    let reach = in_plane.length();
                    if reach < 0.3 {
                        continue;
                    }
                    lobe += weight * r.dot(in_plane / reach).max(0.0).powi(2);
                }
                at.radial * (gain * b * lobe * w)
            }
            Self::Head { centre, aim, reach, push, gain } => {
                let w = m.of(RegionGroup::Head);
                if w <= 1e-5 {
                    return Vec3::ZERO;
                }
                let d = p - *centre;
                let dist = d.length();
                if dist < 1e-7 {
                    return Vec3::ZERO;
                }
                let angle = (d / dist).dot(*aim).clamp(-1.0, 1.0).acos();
                let b = bump(angle / reach);
                if b == 0.0 {
                    return Vec3::ZERO;
                }
                push.map_or(d, |v| v * dist) * (gain * b * w)
            }
        }
    }
}

/// One stroke, placed: its point on the surface, the surface's normal there and its sizes in metres.
#[derive(Clone, Debug, PartialEq)]
struct Placed {
    brush: Brush,
    anchor: Vec3,
    normal: Vec3,
    radius: f32,
    /// Metres for the displacing brushes, a `0..=1` fraction for the others.
    strength: f32,
    direction: Vec3,
    /// The group whose vertices it moves (`None`: all of the part's).
    group: Option<RegionGroup>,
}

impl Placed {
    fn falloff(&self, p: Vec3) -> f32 {
        bump(p.distance(self.anchor) / self.radius)
    }

    fn weight(&self, m: &Membership) -> f32 {
        self.group.map_or(1.0, |g| m.of(g))
    }
}

/// The whole displacement of one part.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShapeField {
    terms: Vec<Term>,
    strokes: Vec<Placed>,
    /// The length the Jacobian's step is a fraction of.
    scale: f32,
}

impl ShapeField {
    /// True when it moves nothing.
    pub fn is_empty(&self) -> bool {
        self.terms.is_empty() && self.strokes.is_empty()
    }

    /// The displacement of the rest point `p` with membership `m`.
    pub fn displace(&self, p: Vec3, m: &Membership) -> Vec3 {
        let mut d: Vec3 = self.terms.iter().map(|t| t.displace(p, m)).sum();
        for (i, s) in self.strokes.iter().enumerate() {
            let w = s.weight(m);
            if w <= 1e-5 {
                continue;
            }
            d += self.stroke(i, s, p, d, m) * w;
        }
        d
    }

    /// Stroke `i`'s displacement at rest point `p`, where everything before it moved `p` by `before`.
    fn stroke(&self, i: usize, s: &Placed, p: Vec3, before: Vec3, m: &Membership) -> Vec3 {
        let now = p + before;
        match s.brush {
            Brush::Grab => s.direction * (s.strength * s.falloff(p)),
            Brush::Inflate => s.normal * (s.strength * s.falloff(p)),
            Brush::Crease => -s.normal * (s.strength * bump(p.distance(s.anchor) / (0.45 * s.radius))),
            Brush::Flatten => {
                let h = (now - s.anchor).dot(s.normal);
                // The falloff is read at the rest point, so a full flatten lands its centre on the plane exactly.
                -s.normal * (h * s.strength * s.falloff(p))
            }
            Brush::Pinch => {
                let to = s.anchor - now;
                let along = to - s.normal * to.dot(s.normal);
                along * (0.5 * s.strength * s.falloff(now))
            }
            Brush::Smooth => {
                let f = s.falloff(p);
                if f == 0.0 {
                    return Vec3::ZERO;
                }
                let r = SMOOTH_REACH * s.radius;
                let at = |q: Vec3| self.before_smooth(i, q, m);
                let centre = at(p);
                let around = [Vec3::X, -Vec3::X, Vec3::Y, -Vec3::Y, Vec3::Z, -Vec3::Z]
                    .iter()
                    .map(|o| at(p + *o * r))
                    .sum::<Vec3>()
                    / 6.0;
                (around - centre) * (s.strength * f)
            }
        }
    }

    /// What the strokes before `i` (but not earlier smooth strokes, which would nest) do at `p`.
    fn before_smooth(&self, i: usize, p: Vec3, m: &Membership) -> Vec3 {
        let mut d = Vec3::ZERO;
        for (j, s) in self.strokes.iter().enumerate().take(i) {
            if s.brush == Brush::Smooth {
                continue;
            }
            let w = s.weight(m);
            if w > 1e-5 {
                d += self.stroke(j, s, p, d, m) * w;
            }
        }
        d
    }

    /// The field's map's Jacobian at `p`.
    fn jacobian(&self, p: Vec3, m: &Membership) -> Mat3 {
        let h = (JACOBIAN_STEP * self.scale).max(1e-5);
        let col = |axis: Vec3| {
            let a = self.displace(p + axis * h, m);
            let b = self.displace(p - axis * h, m);
            axis + (a - b) / (2.0 * h)
        };
        Mat3::from_cols(col(Vec3::X), col(Vec3::Y), col(Vec3::Z))
    }

    /// Moves `p` and its normal `n`.
    fn carry(&self, p: Vec3, n: Vec3, m: &Membership) -> (Vec3, Vec3) {
        let d = self.displace(p, m);
        if d == Vec3::ZERO && self.strokes.is_empty() {
            return (p, n);
        }
        let j = self.jacobian(p, m);
        let n2 = if j.determinant() > 1e-6 { (j.inverse().transpose() * n).normalize_or(n) } else { n };
        (p + d, n2)
    }
}

// ------------------------------------------------------------------------------------------------ resolving

/// The dial terms for the body and the head of `def` (the person's settings plus the age's), placed in `body` and
/// `head` where those frames exist yet.
fn dial_terms(def: &DesignDef, body: Option<&BodyFrame>, head: Option<&HeadFrame>) -> Result<Vec<Term>, String> {
    let mut values: BTreeMap<String, (f64, f64, f64)> = BTreeMap::new();
    for (id, s) in &def.dials {
        values.insert(id.to_string(), (s.value, s.left, s.right));
    }
    let library = super::design_library()?;
    if let Some(age) = def.age {
        for dial in &library.dials {
            let v = dial.at_age(age);
            if v != 0.0 {
                values.entry(dial.id.to_string()).or_insert((0.0, 0.0, 0.0)).0 += v;
            }
        }
    }
    let mut terms = Vec::new();
    for (id, (value, left, right)) in values {
        let dial = find_dial(&id)?;
        let clamp = |v: f64| v.clamp(dial.range.0, dial.range.1);
        let sides: Vec<(Option<DialSide>, f64)> = if dial.sided {
            vec![(Some(DialSide::Left), clamp(value + left)), (Some(DialSide::Right), clamp(value + right))]
        } else {
            vec![(None, clamp(value))]
        };
        for effect in &dial.effects {
            for &(side, v) in &sides {
                if v == 0.0 {
                    continue;
                }
                let mirror = side == Some(DialSide::Right);
                if let Some(term) = place_effect(effect, v, mirror, body, head) {
                    terms.push(term);
                }
            }
        }
    }
    Ok(terms)
}

/// One effect at value `v`, mirrored to the right side if `mirror`, or `None` if its frame isn't built yet.
#[allow(clippy::cast_possible_truncation)] // dial values are small
fn place_effect(
    effect: &DialEffect,
    v: f64,
    mirror: bool,
    body: Option<&BodyFrame>,
    head: Option<&HeadFrame>,
) -> Option<Term> {
    let amount_at = |amount: f32, down: Option<f32>| v as f32 * if v < 0.0 { down.unwrap_or(amount) } else { amount };
    match effect {
        DialEffect::Body { region, t, span, lobes, amount, amount_down } => {
            let group = if mirror { region.mirrored() } else { *region };
            let path = body?.path(group)?;
            let all = lobes.all;
            Some(Term::Body {
                path: Arc::new(path.clone()),
                group,
                t: *t,
                span: *span,
                lobes: lobes.directions(group.side()),
                all,
                gain: amount_at(*amount, *amount_down),
            })
        }
        DialEffect::Head { landmark, dir, nudge, reach, push, amount, amount_down } => {
            let head = head?;
            let flip = |v: Vec3| if mirror { Vec3::new(-v.x, v.y, v.z) } else { v };
            let mut aim = match (landmark, dir) {
                (Some(l), _) => head.landmark(if mirror { l.mirrored() } else { *l }),
                (None, Some(d)) => head.to_dir(flip(Vec3::from(*d))),
                (None, None) => return None,
            };
            if let Some(n) = nudge {
                aim = head.to_dir(head.from_dir(aim) + flip(Vec3::from(*n)));
            }
            let side = if mirror { -1.0 } else { 1.0 };
            Some(Term::Head {
                centre: head.centre,
                aim,
                reach: reach.to_radians(),
                push: push.vector(side),
                gain: amount_at(*amount, *amount_down),
            })
        }
    }
}

/// Where a stroke's point falls and the local scale there, before it is placed on the mesh.
fn point_in_frame(
    at: &PartPoint,
    body: Option<&BodyFrame>,
    head: Option<&HeadFrame>,
    part: Option<&PartFrame>,
) -> Option<(Vec3, Vec3, f32, Option<RegionGroup>)> {
    match *at {
        PartPoint::Body { region, t, angle, lift } => {
            let row = body?.path(region)?.at(t);
            let dir = row.direction(angle);
            Some((row.centre + dir * (row.radius_at(angle) * lift), dir, row.scale(), Some(region)))
        }
        PartPoint::Head { dir, lift } => {
            let head = head?;
            let d = head.to_dir(Vec3::from(dir));
            Some((head.centre + d * (head.radius(d) * lift), d, head.scale(), None))
        }
        PartPoint::Box { at } => {
            let part = part?;
            let p = part.point(Vec3::from(at));
            let centre = 0.5 * (part.min + part.max);
            Some((p, (p - centre).normalize_or(Vec3::Z), part.scale(), None))
        }
    }
}

/// Places `strokes` on `output`'s meshes (each point snapped to the nearest vertex of the group it moves).
fn place_strokes(
    strokes: &[SculptStroke],
    output: &PartOutput,
    body: Option<&BodyFrame>,
    head: Option<&HeadFrame>,
) -> Result<Vec<Placed>, String> {
    let part = PartFrame::of(&output.meshes);
    let mut placed = Vec::new();
    for stroke in strokes.iter().flat_map(|s| {
        let mirror = s.symmetric.then(|| s.mirrored());
        std::iter::once(s.clone()).chain(mirror)
    }) {
        let (guess, outward, scale, group) =
            point_in_frame(&stroke.at, body, head, part.as_ref()).ok_or_else(|| {
                "its point is on a frame this part doesn't have (a body point on a part built before the body)"
                    .to_owned()
            })?;
        let (anchor, normal) = snap(output, guess, group).unwrap_or((guess, outward));
        let normal = if normal.dot(outward) < 0.0 && group.is_some() { -normal } else { normal };
        let strength = if stroke.brush.is_fractional() { stroke.strength } else { stroke.strength * scale };
        let direction = stroke.direction.map_or(Vec3::ZERO, |d| Vec3::from(d).normalize_or(Vec3::ZERO));
        placed.push(Placed {
            brush: stroke.brush,
            anchor,
            normal,
            radius: stroke.radius * scale,
            strength,
            direction,
            group: group.filter(|_| matches!(stroke.at, PartPoint::Body { .. })),
        });
    }
    Ok(placed)
}

/// The vertex of `output` nearest `p` (among those mostly in `group`, if given) and its normal.
fn snap(output: &PartOutput, p: Vec3, group: Option<RegionGroup>) -> Option<(Vec3, Vec3)> {
    let mut best: Option<(f32, Vec3, Vec3)> = None;
    for mesh in output.meshes.iter().filter(|m| !m.dynamic) {
        let members = group.map(|_| Membership::of_mesh(mesh, None));
        let g = &mesh.geometry;
        for (i, (q, n)) in g.positions.iter().zip(&g.normals).enumerate() {
            if let (Some(group), Some(members)) = (group, &members)
                && members[i].of(group) < 0.5
            {
                continue;
            }
            let d = q.distance_squared(p);
            if best.is_none_or(|(b, _, _)| d < b) {
                best = Some((d, *q, *n));
            }
        }
    }
    best.map(|(_, q, n)| (q, n))
}

// ------------------------------------------------------------------------------------------------ applying

/// The group a rigid mesh of a part of `category` belongs to.
fn rigid_group(category: PartCategory) -> Option<RegionGroup> {
    match category {
        PartCategory::Head => Some(RegionGroup::Head),
        _ => None,
    }
}

/// Applies `def`'s design to the output of `part` (a `category` part): publishes the body's or the head's frame if
/// the design shapes them, moves the meshes, normals and anchors by the field, and wraps the fit and the head surface
/// the part publishes so later parts see the moved shape. Does nothing when the design moves nothing of this part.
///
/// # Errors
/// [`BuildError::Part`] for a dial the library doesn't have or a stroke whose frame the part doesn't have. `layout`
/// is the character's face layout, which places the head's landmarks.
pub fn apply(
    def: &DesignDef,
    layout: &FaceLayout,
    part: &PartId,
    category: PartCategory,
    output: &mut PartOutput,
    published: &Published,
) -> Result<(), BuildError> {
    let fail = |message: String| BuildError::Part { part: part.clone(), message: format!("design: {message}") };
    let shapes = def.shapes_body() && matches!(category, PartCategory::Body | PartCategory::Head);
    let strokes = def.strokes(part);
    if !shapes && strokes.is_empty() {
        return Ok(());
    }
    let own_fit = output.publishes.values::<FitFact>().find(|f| f.envelope()).cloned();
    let body: Option<Arc<BodyFrame>> = match (&own_fit, published.get::<BodyFrameFact>()) {
        (Some(fit), _) if category == PartCategory::Body => {
            let frame = Arc::new(BodyFrame::of(fit.as_ref()));
            output.publishes.put::<BodyFrameFact>(frame.clone());
            Some(frame)
        }
        (_, Some(frame)) => Some(frame.clone()),
        _ => published.outer_fit().map(|f| Arc::new(BodyFrame::of(f.as_ref()))),
    };
    let own_head = output.publishes.values::<HeadFact>().next().cloned();
    let head: Option<Arc<HeadFrame>> = match (&own_head, published.get::<HeadFrameFact>()) {
        (Some(surface), _) if category == PartCategory::Head => {
            let frame = Arc::new(HeadFrame::of(surface.clone(), layout, published.scale()));
            output.publishes.put::<HeadFrameFact>(frame.clone());
            Some(frame)
        }
        (_, Some(frame)) => Some(frame.clone()),
        _ => None,
    };
    let terms = if shapes { dial_terms(def, body.as_deref(), head.as_deref()).map_err(fail)? } else { Vec::new() };
    let placed = place_strokes(strokes, output, body.as_deref(), head.as_deref())
        .map_err(|e| fail(format!("a sculpt stroke can't be placed: {e}")))?;
    let scale = head.as_ref().map(|h| h.scale()).or_else(|| PartFrame::of(&output.meshes).map(|f| 0.1 * f.scale()));
    let field = Arc::new(ShapeField { terms, strokes: placed, scale: scale.unwrap_or(0.1) });
    if field.is_empty() {
        return Ok(());
    }
    let mut skipped = 0usize;
    for mesh in &mut output.meshes {
        if mesh.dynamic {
            skipped += 1;
            continue;
        }
        let members = Membership::of_mesh(mesh, rigid_group(category));
        let g = &mut mesh.geometry;
        for ((p, n), m) in g.positions.iter_mut().zip(g.normals.iter_mut()).zip(&members) {
            (*p, *n) = field.carry(*p, *n, m);
        }
    }
    if skipped > 0 && !strokes.is_empty() {
        output.warnings.push(format!(
            "design: {skipped} simulated mesh(es) keep their shape: sculpt moves only meshes that don't simulate"
        ));
    }
    for (_, anchor) in output.publishes.values_mut::<AnchorsFact>() {
        let m = anchor_membership(&anchor.bone, category);
        anchor.position += field.displace(anchor.position, &m);
    }
    for fit in output.publishes.values_mut::<FitFact>() {
        if fit.envelope() {
            *fit = Arc::new(DeformedFit { inner: fit.clone(), field: field.clone() });
        }
    }
    for surface in output.publishes.values_mut::<HeadFact>() {
        *surface = Arc::new(DeformedHead { inner: surface.clone(), field: field.clone() });
    }
    Ok(())
}

/// An anchor's membership: wholly in its bone's group.
fn anchor_membership(bone: &JointRef, category: PartCategory) -> Membership {
    match bone {
        JointRef::Humanoid(b) => Membership::only(RegionGroup::of_bone(*b)),
        JointRef::Named(_) => rigid_group(category).map_or_else(Membership::default, Membership::only),
    }
}

// ------------------------------------------------------------------------------------------------ what later parts see

/// A body fit moved by a shape field: each section's four extremes are moved and the ellipse refitted through them.
pub struct DeformedFit {
    inner: Arc<dyn BodyFit>,
    field: Arc<ShapeField>,
}

impl Debug for DeformedFit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeformedFit").field("inner", &self.inner).field("field", &self.field).finish()
    }
}

impl DeformedFit {
    fn moved(&self, region: BodyRegion, s: Section) -> Section {
        let Some(group) = RegionGroup::of_region(region) else { return s };
        let m = Membership::only(group);
        let row = Row::new(region, 0.0, s.centre, s.axis, [s.radii.x, s.radii.y]);
        let at = |p: Vec3| p + self.field.displace(p, &m);
        let (xp, xn) = (at(s.centre + row.across * s.radii.x), at(s.centre - row.across * s.radii.x));
        let (yp, yn) = (at(s.centre + row.through * s.radii.y), at(s.centre - row.through * s.radii.y));
        let centre = 0.25 * (xp + xn + yp + yn);
        let rx = 0.5 * (xp - xn).dot(row.across).abs();
        let ry = 0.5 * (yp - yn).dot(row.through).abs();
        Section { centre, axis: s.axis, radii: Vec2::new(rx.max(1e-5), ry.max(1e-5)) }
    }
}

impl BodyFit for DeformedFit {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        self.inner.section(region, t).map(|s| self.moved(region, s))
    }

    fn cap(&self, region: BodyRegion, end: RegionEnd, height: f32) -> Option<Section> {
        self.inner.cap(region, end, height).map(|s| self.moved(region, s))
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        self.inner.joint(joint)
    }

    fn envelope(&self) -> bool {
        self.inner.envelope()
    }

    fn bind(&self) -> HumanoidPose {
        self.inner.bind()
    }
}

/// A head surface moved by a shape field.
pub struct DeformedHead {
    inner: Arc<dyn HeadSurface>,
    field: Arc<ShapeField>,
}

impl Debug for DeformedHead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeformedHead").field("inner", &self.inner).field("field", &self.field).finish()
    }
}

impl HeadSurface for DeformedHead {
    fn centre(&self) -> Vec3 {
        self.inner.centre()
    }

    fn radius(&self, dir: Vec3) -> f32 {
        let c = self.inner.centre();
        let q = c + dir * self.inner.radius(dir);
        let moved = q + self.field.displace(q, &Membership::only(RegionGroup::Head));
        (moved - c).dot(dir).max(1e-5)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sphere_field(brush: Brush, strength: f32) -> ShapeField {
        ShapeField {
            terms: Vec::new(),
            strokes: vec![Placed {
                brush,
                anchor: Vec3::Z,
                normal: Vec3::Z,
                radius: 0.5,
                strength,
                direction: Vec3::Y,
                group: None,
            }],
            scale: 1.0,
        }
    }

    #[test]
    fn inflate_pushes_along_the_normal_and_fades_to_nothing_at_its_radius() {
        let f = sphere_field(Brush::Inflate, 0.1);
        let m = Membership::default();
        assert!((f.displace(Vec3::Z, &m) - Vec3::Z * 0.1).length() < 1e-6);
        assert_eq!(f.displace(Vec3::new(0.0, 0.6, 1.0), &m), Vec3::ZERO);
    }

    #[test]
    fn flatten_pulls_toward_the_plane_of_its_point() {
        let mut f = sphere_field(Brush::Inflate, 0.1);
        f.strokes.push(Placed { brush: Brush::Flatten, strength: 1.0, ..f.strokes[0].clone() });
        let m = Membership::default();
        // The inflate moved the point 0.1 off the plane; a full flatten at its centre brings it back.
        assert!(f.displace(Vec3::Z, &m).length() < 1e-5);
    }

    #[test]
    fn smooth_relaxes_the_strokes_before_it() {
        let mut f = sphere_field(Brush::Crease, 0.05);
        let m = Membership::default();
        let before = f.displace(Vec3::Z, &m).length();
        f.strokes.push(Placed { brush: Brush::Smooth, strength: 1.0, ..f.strokes[0].clone() });
        let after = f.displace(Vec3::Z, &m).length();
        assert!(after < 0.8 * before, "{after} vs {before}");
    }

    #[test]
    fn normals_follow_the_field() {
        // A grab out of the surface raises a hill, so the normal on its side tilts away from the hill's centre; a
        // grab along the surface slides it in its own plane, so the normal stays.
        let mut f = sphere_field(Brush::Grab, 0.2);
        let m = Membership::default();
        let (_, n) = f.carry(Vec3::new(0.0, -0.2, 1.0), Vec3::Z, &m);
        assert!((n - Vec3::Z).length() < 1e-4, "{n}");
        f.strokes[0].direction = Vec3::Z;
        let (_, n) = f.carry(Vec3::new(0.0, -0.2, 1.0), Vec3::Z, &m);
        assert!(n.y < -0.05, "{n}");
        assert!((n.length() - 1.0).abs() < 1e-5);
    }
}
