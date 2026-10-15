//! `mock-body` and `mock-garment`: a torso tube and layered garments that wrap whatever is inside them, through the
//! published [`BodyFit`] stack. The body also publishes the humanoid joints (the 15 VRM requires, plus chest and neck)
//! and a head-and-neck collider group, so the mock builds a valid rig.

use std::sync::Arc;

use omuzaki_anim_api::{Collider, ColliderGroup, ColliderShape, HumanoidBone};
use omuzaki_char_api::{
    BodyFit, BodyParams, BodyRegion, BuildCtx, BuildError, CollidersFact, FitFact, JointRef, MaterialName,
    MaterialSpec, PartCategory, PartKind, PartMesh, PartMeta, PartOutput, Publishes, RecolourCtx, Recoloured, Section,
    humanoid_id,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_face_api::{Check, ColourRef, Validate, narrow};
use omuzaki_render_api::{Shading, Skin};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{shapes, util};

/// The bare torso: a tapered ellipse from the hips to the neck base, sized by the body proportions.
#[derive(Debug, Clone)]
pub struct TorsoFit {
    base_y: f32,
    top_y: f32,
    hip: Vec2,
    shoulder: Vec2,
}

impl TorsoFit {
    /// The torso of `params`: template defaults times the proportion sliders.
    pub fn new(params: &BodyParams) -> Self {
        let p = &params.proportions;
        let (base_y, length) = (0.33, 0.2 * narrow(p.torso_length));
        Self {
            base_y,
            top_y: base_y + length,
            hip: Vec2::new(0.085 * narrow(p.hip_width), 0.07),
            shoulder: Vec2::new(0.09 * narrow(p.shoulder_width), 0.06),
        }
    }
}

impl TorsoFit {
    /// The bind-space joint of `bone`: a T-pose with the legs under the hips and the arms out from the shoulders.
    fn joint_at(&self, bone: HumanoidBone) -> Option<Vec3> {
        use HumanoidBone as B;
        let (base, top) = (self.base_y, self.top_y);
        let on_axis = |t: f32| Vec3::new(0.0, base + (top - base) * t, 0.0);
        let leg = |side: f32, y: f32| Vec3::new(side * self.hip.x * 0.6, y, 0.0);
        let arm = |side: f32, reach: f32| Vec3::new(side * (self.shoulder.x + reach), top - 0.02, 0.0);
        Some(match bone {
            B::Hips => on_axis(0.0),
            B::Spine => on_axis(0.35),
            B::Chest => on_axis(0.7),
            B::Neck => on_axis(1.0),
            B::Head => on_axis(1.0) + Vec3::Y * 0.05,
            B::LeftUpperLeg => leg(1.0, base),
            B::LeftLowerLeg => leg(1.0, base * 0.55),
            B::LeftFoot => leg(1.0, 0.04),
            B::RightUpperLeg => leg(-1.0, base),
            B::RightLowerLeg => leg(-1.0, base * 0.55),
            B::RightFoot => leg(-1.0, 0.04),
            B::LeftUpperArm => arm(1.0, 0.0),
            B::LeftLowerArm => arm(1.0, 0.1),
            B::LeftHand => arm(1.0, 0.2),
            B::RightUpperArm => arm(-1.0, 0.0),
            B::RightLowerArm => arm(-1.0, 0.1),
            B::RightHand => arm(-1.0, 0.2),
            _ => return None,
        })
    }

    /// The colliders hair and cloth keep out of: the head and the neck, on the bones that carry them.
    fn colliders() -> ColliderGroup {
        let head = ColliderShape::Sphere { offset: Vec3::new(0.0, 0.2, -0.02), radius: 0.19 };
        let neck = ColliderShape::Capsule {
            offset: Vec3::new(0.0, -0.04, 0.0),
            radius: 0.05,
            tail: Vec3::new(0.0, 0.02, 0.0),
        };
        ColliderGroup {
            name: "head-neck".to_owned(),
            colliders: vec![
                Collider { bone: humanoid_id(HumanoidBone::Head), shape: head },
                Collider { bone: humanoid_id(HumanoidBone::Neck), shape: neck },
            ],
        }
    }
}

impl BodyFit for TorsoFit {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        (region == BodyRegion::Torso).then(|| Section {
            centre: Vec3::new(0.0, self.base_y + (self.top_y - self.base_y) * t, 0.0),
            axis: Vec3::Y,
            radii: self.hip.lerp(self.shoulder, t),
        })
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        match joint {
            JointRef::Humanoid(bone) => self.joint_at(*bone),
            JointRef::Named(_) => None,
        }
    }
}

/// Another fit pushed outward by `ease` metres: the envelope of a garment.
#[derive(Debug, Clone)]
pub struct OffsetFit {
    inner: Arc<dyn BodyFit>,
    ease: f32,
}

impl BodyFit for OffsetFit {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        self.inner.section(region, t).map(|s| Section { radii: s.radii + Vec2::splat(self.ease), ..s })
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        self.inner.joint(joint)
    }
}

fn toon(base: Colour, shade: Colour) -> MaterialSpec {
    let mut spec = MaterialSpec::toon(base);
    if let omuzaki_render_api::Shading::Toon(t) = &mut spec.shading {
        t.shade = shade;
    }
    spec
}

/// The body's skin material. `colour` resolves the palette slots, so `build` and `recolour` share this one function and
/// cannot drift apart.
fn skin_material(
    params: &BodyParams,
    colour: impl Fn(&ColourRef) -> Result<Colour, BuildError>,
) -> Result<MaterialSpec, BuildError> {
    let mut material = toon(colour(&params.skin.base)?, colour(&params.skin.shade)?);
    if let Shading::Toon(t) = &mut material.shading {
        t.skin = Some(Skin::default());
    }
    Ok(material)
}

fn torso_mesh(
    ctx: &BuildCtx<'_>,
    fit: &dyn BodyFit,
    name: &str,
    material: &str,
) -> Result<(PartMesh, MaterialName), BuildError> {
    let (rings, segments) = util::detail(ctx.quality, 5, 10);
    let geometry =
        shapes::tube(fit, BodyRegion::Torso, rings, segments).ok_or_else(|| ctx.fail("the fit has no torso"))?;
    let id = util::material(ctx, material)?;
    let mesh = PartMesh {
        name: name.to_owned(),
        geometry,
        joint_bones: vec![JointRef::HIPS, JointRef::CHEST],
        material: id.clone(),
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    };
    Ok((mesh, id))
}

/// The `mock-body` kind: the bare torso, from the real [`BodyParams`].
pub struct MockBody;

impl PartKind for MockBody {
    type Params = BodyParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "mock-body",
            summary: "Build a torso tube and publish its fit.",
            category: PartCategory::Body,
            requires: &[],
        }
    }

    fn build(params: &BodyParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let fit = Arc::new(TorsoFit::new(params));
        let (mesh, id) = torso_mesh(ctx, fit.as_ref(), "torso", "mock-body-skin")?;
        let material = skin_material(params, |r| ctx.colour(r))?;
        Ok(PartOutput::new()
            .with_meshes(vec![mesh])
            .with_materials(vec![(id, material)])
            .with_publishes(Publishes::default().with::<FitFact>(fit).with::<CollidersFact>(TorsoFit::colliders())))
    }

    fn recolour(params: &BodyParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        let [(_, material)] = output.materials.as_mut_slice() else { return Ok(Recoloured::NeedsRebuild) };
        *material = skin_material(params, |r| ctx.colour(r))?;
        Ok(Recoloured::Done)
    }
}

omuzaki_char_api::register_part_kind!(MockBody);

/// Parameters of `mock-garment`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct GarmentParams {
    /// How far the garment stands off whatever is under it, in metres.
    pub ease: f64,
    /// Cloth colour.
    pub colour: ColourRef,
}

impl Default for GarmentParams {
    fn default() -> Self {
        Self { ease: 0.004, colour: ColourRef::rgb(0xf5, 0xf2, 0xed) }
    }
}

impl Validate for GarmentParams {
    fn check(&self, c: &mut Check) {
        c.range("ease", self.ease, 0.0005, 0.05);
    }
}

/// A garment's cloth material (shared by `build` and `recolour`, like [`skin_material`]).
fn garment_material(
    params: &GarmentParams,
    colour: impl Fn(&ColourRef) -> Result<Colour, BuildError>,
) -> Result<MaterialSpec, BuildError> {
    Ok(toon(colour(&params.colour)?, Colour::rgb(0xc6, 0xc2, 0xd6)))
}

/// The `mock-garment` kind: wraps the outermost thing under it and publishes its own envelope for the next layer.
pub struct MockGarment;

impl PartKind for MockGarment {
    type Params = GarmentParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "mock-garment",
            summary: "Wrap the torso in a layer of cloth.",
            category: PartCategory::Garment,
            requires: &["mock-body"],
        }
    }

    fn build(params: &GarmentParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let envelope = Arc::new(OffsetFit { inner: ctx.fit()?, ease: narrow(params.ease) });
        let name = ctx.part.to_string();
        let (mesh, id) = torso_mesh(ctx, envelope.as_ref(), &name, &format!("mock-garment-{name}"))?;
        let material = garment_material(params, |r| ctx.colour(r))?;
        Ok(PartOutput::new()
            .with_meshes(vec![mesh])
            .with_materials(vec![(id, material)])
            .with_publishes(Publishes::default().with::<FitFact>(envelope)))
    }

    fn recolour(
        params: &GarmentParams,
        output: &mut PartOutput,
        ctx: &RecolourCtx<'_>,
    ) -> Result<Recoloured, BuildError> {
        let [(_, material)] = output.materials.as_mut_slice() else { return Ok(Recoloured::NeedsRebuild) };
        *material = garment_material(params, |r| ctx.colour(r))?;
        Ok(Recoloured::Done)
    }
}

omuzaki_char_api::register_part_kind!(MockGarment);
