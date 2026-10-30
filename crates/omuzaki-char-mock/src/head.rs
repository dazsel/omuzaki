//! `mock-head`: an ellipsoid head built from the real [`HeadParams`], with the face window and anchors.

use std::sync::Arc;

use omuzaki_char_api::{
    Anchor, AnchorsFact, BuildCtx, BuildError, Cranium, FaceFact, FaceOverlay, HeadFact, HeadParams, HeadSurface,
    JointRef, MaterialSpec, PartCategory, PartKind, PartMesh, PartMeta, PartOutput, Publishes, RecolourCtx, Recoloured,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_face_api::{ColourRef, FaceLayerId, narrow};
use omuzaki_render_api::{AlphaMode, Outline, Rim, Shading, Skin, Toon};

use crate::{shapes, util};

/// The cranium ellipsoid as a head surface, so hair sits exactly on the mesh the head part drew.
#[derive(Debug, Clone)]
pub struct EllipsoidHead {
    centre: Vec3,
    /// `half_width`, `half_depth_front`, `half_depth_back`, `half_height_up`, `half_height_down_back`,
    /// `half_height_down_front`, narrowed to the `f32` the geometry uses.
    radii: [f32; 6],
}

impl EllipsoidHead {
    /// The surface of `cranium`.
    pub fn new(cranium: &Cranium) -> Self {
        let [x, y, z] = cranium.centre().map(narrow);
        Self {
            centre: Vec3::new(x, y, z),
            radii: [
                cranium.half_width,
                cranium.half_depth_front,
                cranium.half_depth_back,
                cranium.half_height_up,
                cranium.half_height_down_back,
                cranium.half_height_down_front,
            ]
            .map(narrow),
        }
    }
}

impl HeadSurface for EllipsoidHead {
    fn centre(&self) -> Vec3 {
        self.centre
    }

    fn radius(&self, dir: Vec3) -> f32 {
        let [half_width, depth_front, depth_back, up, down_back, down_front] = self.radii;
        // Below the equator the head is longer toward the chin than toward the nape; blend smoothly (a hard switch on
        // the sign of z leaves a seam at the bottom pole).
        let ry = if dir.y >= 0.0 {
            up
        } else {
            let t = ((dir.z + 0.35) / 0.9).clamp(0.0, 1.0);
            down_back + (down_front - down_back) * (t * t * (3.0 - 2.0 * t))
        };
        let rz = if dir.z >= 0.0 { depth_front } else { depth_back };
        1.0 / ((dir.x / half_width).powi(2) + (dir.y / ry).powi(2) + (dir.z / rz).powi(2)).sqrt()
    }
}

/// The head's skin material, with the face overlays it was given. `colour` resolves the palette slots, so `build` and
/// `recolour` share this one function and cannot drift apart. The overlays come from the published face window, which
/// doesn't depend on colour: a recolour keeps the ones the build made.
fn skin_material(
    params: &HeadParams,
    overlays: Vec<FaceOverlay>,
    colour: impl Fn(&ColourRef) -> Result<Colour, BuildError>,
) -> Result<MaterialSpec, BuildError> {
    Ok(MaterialSpec {
        base: colour(&params.skin.base)?,
        base_texture: None,
        shading: Shading::Toon(Toon {
            shade: colour(&params.skin.shade)?,
            shift: -0.28,
            softness: 0.28,
            rim: Rim { colour: colour(&params.skin.rim)?, power: 3.0, strength: 0.1 },
            emissive: Colour::BLACK,
            skin: Some(Skin { floor: 0.66, hue_keep: 0.55 }),
        }),
        overlays,
        alpha: AlphaMode::Opaque,
        double_sided: false,
        vertex_colours: false,
        outline: Some(Outline { colour: colour(&params.skin.line)?, width: 0.0014 }),
        receives_shadows: false,
        hair_shadow: omuzaki_render_api::HairShadow::NONE,
        uv_transform: omuzaki_render_api::UvTransform::IDENTITY,
    })
}

/// The `mock-head` kind.
pub struct MockHead;

impl PartKind for MockHead {
    type Params = HeadParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "mock-head",
            summary: "Build an ellipsoid head with a face window.",
            category: PartCategory::Head,
            requires: &[],
        }
    }

    fn build(params: &HeadParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let surface = Arc::new(EllipsoidHead::new(&params.cranium));
        let (rings, segments) = util::detail(ctx.quality, 10, 16);
        let mut geometry = shapes::ellipsoid(surface.centre(), &|d| surface.radius(d), rings, segments, 180.0);
        geometry.joints = vec![[0; 4]; geometry.positions.len()];
        geometry.weights = vec![[1.0, 0.0, 0.0, 0.0]; geometry.positions.len()];

        let projection = params.face_projection();
        let material_name = util::material(ctx, "mock-head-skin")?;
        // The head publishes the projection, so it builds its own overlay for the painted face.
        let overlays = vec![FaceOverlay::of(&projection, surface.centre(), FaceLayerId::Base)];
        let material = skin_material(params, overlays, |r| ctx.colour(r))?;
        let mesh = PartMesh {
            name: "head".to_owned(),
            geometry,
            joint_bones: vec![JointRef::HEAD],
            material: material_name.clone(),
            casts_shadow: true,
            render_order: 0,
            dynamic: false,
        };
        let top = Anchor {
            bone: JointRef::HEAD,
            position: surface.centre() + Vec3::Y * narrow(params.cranium.half_height_up),
            rotation: Quat::IDENTITY,
        };
        let anchor = (util::anchor(ctx, "top")?, top);
        Ok(PartOutput::new().with_meshes(vec![mesh]).with_materials(vec![(material_name, material)]).with_publishes(
            Publishes::default().with::<HeadFact>(surface).with::<FaceFact>(projection).with::<AnchorsFact>(anchor),
        ))
    }

    fn recolour(params: &HeadParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        let [(_, material)] = output.materials.as_mut_slice() else { return Ok(Recoloured::NeedsRebuild) };
        *material = skin_material(params, std::mem::take(&mut material.overlays), |r| ctx.colour(r))?;
        Ok(Recoloured::Done)
    }
}

omuzaki_char_api::register_part_kind!(MockHead);
