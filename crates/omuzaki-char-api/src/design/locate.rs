//! Locating a point on a built part in the coordinates a sculpt stroke or a decal is stored in: what `part.sculpt.stroke`
//! and `part.decal.add` call when they are given a point in bind space (a click in the viewport, an agent's guess).
//!
//! The coordinates are those of the frame the stroke is placed in when the part builds ([`super::deform`]): the bare
//! body's regions, the bare head's proportions, or the part's own box. So the parts before `part` are built as the
//! character stands, and `part` itself without its design, to read the frame its strokes meet.

use std::sync::Arc;

use omuzaki_core::glam::Vec3;

use super::deform::{BodyFrameFact, HeadFrameFact};
use super::frame::{BodyFrame, HeadFrame, Membership, PartFrame, RegionGroup};
use super::sculpt::PartPoint;
use crate::build::{Assembler, BuildOptions, build_part, plan};
use crate::character::CharacterDef;
use crate::error::BuildError;
use crate::ids::{CharId, PartId};
use crate::kind::{FitFact, HeadFact, PartCategory, find_kind};
use crate::services::BuildServices;

/// Where a bind-space `point` (metres) lies on `part` of the character `def`, as a [`PartPoint`]: on a body region for
/// the body and for a garment over it, in the head's proportions for the head, in the part's box otherwise. The point
/// need not be on the surface: its distance out is kept as the point's `lift`.
///
/// # Errors
/// A [`BuildError`] of the parts before `part`, or [`BuildError::Part`] if the character has no such part (or it is
/// disabled) or it builds no mesh.
pub fn locate(
    id: &CharId,
    def: &CharacterDef,
    part: &PartId,
    point: Vec3,
    options: &BuildOptions,
    services: &BuildServices,
) -> Result<PartPoint, BuildError> {
    let fail = |message: &str| BuildError::Part { part: part.clone(), message: message.to_owned() };
    let order = plan(def)?;
    if !order.contains(part) {
        return Err(fail("the part is not built (it is disabled, or under a disabled part)"));
    }
    let mut assembler = Assembler::new(id, def, options);
    for p in order.iter().take_while(|p| *p != part) {
        let output = build_part(def, p, options, services, assembler.published())?;
        assembler.add(p, output)?;
    }
    let published = assembler.published();
    // The part as its generator made it: its frame is the one its strokes are placed in.
    let mut bare = def.clone();
    bare.design.sculpt.remove(part);
    let kind = def.parts.get(part).map(|p| p.kind.clone()).ok_or_else(|| fail("the character has no such part"))?;
    let category = find_kind(kind.as_str()).map_or(PartCategory::Accessory, |reg| (reg.meta)().category);
    if matches!(category, PartCategory::Body | PartCategory::Head) {
        bare.design.dials.clear();
        bare.design.age = None;
    }
    let output = build_part(&bare, part, options, services, published)?;
    let body = match category {
        PartCategory::Body => {
            output.publishes.values::<FitFact>().find(|f| f.envelope()).map(|f| Arc::new(BodyFrame::of(f.as_ref())))
        }
        _ => published
            .get::<BodyFrameFact>()
            .clone()
            .or_else(|| published.outer_fit().map(|f| Arc::new(BodyFrame::of(f.as_ref())))),
    };
    if category == PartCategory::Head {
        let surface = output
            .publishes
            .values::<HeadFact>()
            .next()
            .cloned()
            .ok_or_else(|| fail("the head publishes no surface"))?;
        let head = HeadFrame::of(surface, &def.face.layout, published.scale());
        return Ok(head_point(&head, point));
    }
    let nearest = nearest_group(&output.meshes, point, category);
    if let (Some(body), Some(group), true) =
        (body, nearest, matches!(category, PartCategory::Body | PartCategory::Garment))
    {
        let group = if group == RegionGroup::Head { RegionGroup::Neck } else { group };
        if let Some(path) = body.path(group) {
            let at = path.locate(point);
            let angle = at.row.angle_of(at.radial);
            let lift = at.radial.length() / at.row.radius_at(angle).max(1e-6);
            return Ok(PartPoint::Body { region: group, t: at.row.t, angle, lift: lift.clamp(0.05, 3.9) });
        }
    }
    if let Some(head) = published.get::<HeadFrameFact>() {
        if category == PartCategory::Hair {
            return Ok(head_point(head, point));
        }
    }
    let frame = PartFrame::of(&output.meshes).ok_or_else(|| fail("the part builds no mesh"))?;
    Ok(PartPoint::Box { at: frame.coordinates(point).clamp(Vec3::ZERO, Vec3::ONE).to_array() })
}

/// `point` in the head's proportions.
fn head_point(head: &HeadFrame, point: Vec3) -> PartPoint {
    let offset = point - head.centre;
    let d = offset.normalize_or(Vec3::Z);
    let lift = offset.length() / head.radius(d).max(1e-6);
    PartPoint::Head { dir: head.from_dir(d).to_array(), lift: lift.clamp(0.05, 3.9) }
}

/// The region of the vertex nearest `point` (its strongest membership), among the part's meshes that don't simulate.
fn nearest_group(meshes: &[crate::geometry::PartMesh], point: Vec3, category: PartCategory) -> Option<RegionGroup> {
    let rigid = (category == PartCategory::Head).then_some(RegionGroup::Head);
    let mut best: Option<(f32, RegionGroup)> = None;
    for mesh in meshes.iter().filter(|m| !m.dynamic) {
        let members: Vec<Membership> = Membership::of_mesh(mesh, rigid);
        for (p, m) in mesh.geometry.positions.iter().zip(&members) {
            let d = p.distance_squared(point);
            if best.is_none_or(|(b, _)| d < b) {
                let group = RegionGroup::ALL.into_iter().max_by(|a, b| m.of(*a).total_cmp(&m.of(*b)))?;
                best = Some((d, group));
            }
        }
    }
    best.map(|(_, g)| g)
}
