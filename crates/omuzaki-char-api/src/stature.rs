//! Real-world stature: a character is built at its real size, in metres.
//!
//! Templates and part kinds are authored against one shared head, so a template's body stands at a different height in
//! the space it is authored in than the person it stands for. A character is built at real size anyway, so that what
//! works in metres agrees with the exported VRM: the body, the head and everything built on them (hair, cloth, garments,
//! accessories, springs, colliders) are in metres, and a simulation is created at real size. The body part states its
//! [`Stature`] when it publishes ([`StatureFact`](crate::StatureFact)) and the build applies one uniform scale to the
//! body and the head, both authored in the template's space, *before* anything else is built: later parts read a body
//! that is already in metres, so no physical constant (gravity, stiffness) is ever rescaled.
//!
//! What this module does *not* do: stage framing, IK reach and light ranges live in other crates. They get a character
//! at real size and can read [`BuiltCharacter::stature`](crate::BuiltCharacter::stature) and the mesh bounds, but this
//! change touches none of them.
//!
//! [`scale_output`] is the one place that knows every length a part output holds. It reaches meshes, the overlay
//! windows of materials, the head surface, the body fit (sections, joints, the bind pose's hips offset), anchors,
//! collider groups, extra bones and spring hit radii. Rotations, weights, texture pixels and the face projection (head
//! local, in the painter's units) are unchanged.

use std::sync::Arc;

use omuzaki_anim_api::{Collider, ColliderGroup, ColliderShape, HumanoidPose, SpringChain};
use omuzaki_core::glam::Vec3;

use crate::error::BuildError;
use crate::ids::{JointRef, PartId};
use crate::kind::{
    Anchor, AnchorsFact, BodyFit, BodyRegion, CollidersFact, FitFact, HeadFact, HeadSurface, PartOutput, Publishes,
    Section,
};
use crate::rig::RigContribution;

/// How tall a character is, and the uniform scale that made it so.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Stature {
    /// The factor that took the body from the space it is authored in to real size (`1` for a body authored in metres).
    pub scale: f32,
    /// The character's height in metres after the scale, crown to sole, for reports and framing.
    pub height_m: f32,
}

impl Stature {
    /// Checks that the scale is a positive, finite factor and the height a positive, finite number of metres.
    ///
    /// # Errors
    /// A message saying which number is unusable.
    pub fn check(&self) -> Result<(), String> {
        if !(self.scale.is_finite() && self.scale > 0.0) {
            return Err(format!("the stature scale must be a positive, finite number (got {})", self.scale));
        }
        if !(self.height_m.is_finite() && self.height_m > 0.0) {
            return Err(format!(
                "the stature height must be a positive, finite number of metres (got {})",
                self.height_m
            ));
        }
        Ok(())
    }
}

/// Scales every length in `output` by `s`, in place. `s == 1` leaves it untouched, bit for bit.
///
/// # Errors
/// [`BuildError::Part`] if the part carries a simulation: it was built at the authored size, and a runtime cannot be
/// rescaled afterwards without changing how it behaves. A part that simulates must be built in metres, which parts
/// after the body are; the body and the head must not simulate.
pub fn scale_output(part: &PartId, output: &mut PartOutput, s: f32) -> Result<(), BuildError> {
    #[allow(clippy::float_cmp)] // exactly 1 means "no scale", and must leave the output bit for bit
    let unscaled = s == 1.0;
    if unscaled {
        return Ok(());
    }
    if output.runtime.is_some() {
        return Err(BuildError::Part {
            part: part.clone(),
            message: "the body and the head are built at the template's size and scaled to the character's stature \
                      afterwards, so they cannot carry a simulation: put the simulation in a part built after them"
                .to_owned(),
        });
    }
    for mesh in &mut output.meshes {
        mesh.geometry.positions.iter_mut().for_each(|p| *p *= s);
    }
    for (_, material) in &mut output.materials {
        for overlay in &mut material.overlays {
            let w = &mut overlay.window;
            (w.min, w.max) = (w.min * s, w.max * s);
            (w.fade_from, w.fade_to) = (w.fade_from * s, w.fade_to * s);
        }
    }
    scale_publishes(&mut output.publishes, s);
    scale_rig(&mut output.rig, s);
    Ok(())
}

fn scale_publishes(p: &mut Publishes, s: f32) {
    for head in p.values_mut::<HeadFact>() {
        *head = Arc::new(ScaledHead { inner: head.clone(), scale: s });
    }
    for fit in p.values_mut::<FitFact>() {
        *fit = Arc::new(ScaledFit { inner: fit.clone(), scale: s });
    }
    for (_, anchor) in p.values_mut::<AnchorsFact>() {
        scale_anchor(anchor, s);
    }
    p.values_mut::<CollidersFact>().for_each(|g| scale_group(g, s));
}

fn scale_anchor(anchor: &mut Anchor, s: f32) {
    anchor.position *= s;
}

fn scale_rig(rig: &mut RigContribution, s: f32) {
    for bone in &mut rig.bones {
        bone.rest.translation *= s;
    }
    rig.chains.iter_mut().for_each(|c| scale_chain(c, s));
    rig.collider_groups.iter_mut().for_each(|g| scale_group(g, s));
}

/// Only the collision radius is a length: stiffness, drag and gravity are physical quantities and stay as authored.
fn scale_chain(chain: &mut SpringChain, s: f32) {
    for joint in &mut chain.joints {
        joint.params.hit_radius *= s;
    }
}

fn scale_group(group: &mut ColliderGroup, s: f32) {
    for Collider { shape, .. } in &mut group.colliders {
        match shape {
            ColliderShape::Sphere { offset, radius } | ColliderShape::SphereInside { offset, radius } => {
                (*offset, *radius) = (*offset * s, *radius * s);
            }
            ColliderShape::Capsule { offset, radius, tail } | ColliderShape::CapsuleInside { offset, radius, tail } => {
                (*offset, *radius, *tail) = (*offset * s, *radius * s, *tail * s);
            }
            // A plane's offset scales; its normal is a direction and does not.
            ColliderShape::Plane { offset, .. } => *offset *= s,
            // `ColliderShape` is `#[non_exhaustive]`: a shape added later must be given its scaling here.
            _ => debug_assert!(false, "scale_group: unscaled collider shape"),
        }
    }
}

/// A body fit seen at `scale` times its authored size.
#[derive(Debug)]
struct ScaledFit {
    inner: Arc<dyn BodyFit>,
    scale: f32,
}

impl BodyFit for ScaledFit {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        self.inner.section(region, t).map(|sec| Section {
            centre: sec.centre * self.scale,
            axis: sec.axis,
            radii: sec.radii * self.scale,
        })
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        self.inner.joint(joint).map(|p| p * self.scale)
    }

    fn envelope(&self) -> bool {
        self.inner.envelope()
    }

    /// The rotations are unchanged; the hips offset is a length and scales with the body.
    fn bind(&self) -> HumanoidPose {
        let mut bind = self.inner.bind();
        bind.hips_offset *= self.scale;
        bind
    }
}

/// A head surface seen at `scale` times its authored size.
#[derive(Debug)]
struct ScaledHead {
    inner: Arc<dyn HeadSurface>,
    scale: f32,
}

impl HeadSurface for ScaledHead {
    fn centre(&self) -> Vec3 {
        self.inner.centre() * self.scale
    }

    fn radius(&self, dir: Vec3) -> f32 {
        self.inner.radius(dir) * self.scale
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::BoneId;

    use super::*;

    #[test]
    fn scaling_a_group_scales_lengths_but_not_a_plane_normal() {
        let (offset, tail) = (Vec3::new(0.0, 0.1, 0.2), Vec3::new(0.0, -0.3, 0.0));
        let shapes = [
            ColliderShape::SphereInside { offset, radius: 0.2 },
            ColliderShape::CapsuleInside { offset, radius: 0.2, tail },
            ColliderShape::Plane { offset, normal: Vec3::Y },
        ];
        let mut group = ColliderGroup {
            name: "g".to_owned(),
            colliders: shapes.iter().map(|&shape| Collider { bone: BoneId(0), shape }).collect(),
        };
        scale_group(&mut group, 2.0);
        let scaled: Vec<_> = group.colliders.iter().map(|c| c.shape).collect();
        assert_eq!(
            scaled,
            [
                ColliderShape::SphereInside { offset: offset * 2.0, radius: 0.4 },
                ColliderShape::CapsuleInside { offset: offset * 2.0, radius: 0.4, tail: tail * 2.0 },
                ColliderShape::Plane { offset: offset * 2.0, normal: Vec3::Y },
            ]
        );
    }
}
