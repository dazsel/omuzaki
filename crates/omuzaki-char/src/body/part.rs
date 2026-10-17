//! The `body` part kind: its layout from the character's template, its meshes, its skin material, and what it publishes
//! (the template fit with the bind pose, and the character's real-world stature).

use std::sync::Arc;

use omuzaki_char_api::{
    BodyParams, BuildCtx, BuildError, FitFact, KindProvenance, MaterialName, MaterialSpec, PartCategory, PartKind,
    PartMesh, PartMeta, PartOutput, Publishes, StatureFact, register_part_kind,
};
use omuzaki_core::colour::Colour;
use omuzaki_render_api::{Outline, Rim, Shading, Skin, Toon};

use super::{hands, mesh};
use crate::kit::skin::SegmentSet;
use crate::template::registry;
use crate::template::skeleton::TemplateFit;

/// The name of the body's one material.
const SKIN_MATERIAL: &str = "body-skin";
/// The skin's outline, metres at the renderer's reference distance (`body.js` `skin-hand`).
const OUTLINE_WIDTH: f32 = 0.0016;

/// The `body` kind: the bare body under the clothes, built from the character's template.
pub struct Body;

impl PartKind for Body {
    type Params = BodyParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "body",
            summary: "Build the bare torso, arms, legs and feet from the body template and publish its fit.",
            category: PartCategory::Body,
            requires: &[],
        }
    }

    fn provenance() -> KindProvenance {
        KindProvenance::ProjectOwned
    }

    fn build(params: &BodyParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let template = registry::find(ctx.template.as_str())
            .ok_or_else(|| ctx.fail(format!("there is no body template `{}`", ctx.template)))?;
        let layout = template
            .try_layout(&params.proportions, &params.shape, &params.hand)
            .map_err(|e| ctx.fail(format!("template `{}` cannot build a layout: {e}", template.id())))?;
        let stature = template.stature(&layout);
        let fit = Arc::new(TemplateFit::new(layout));
        let material_id = MaterialName::new(SKIN_MATERIAL).map_err(|e| ctx.fail(format!("internal id: {e}")))?;
        let to_build_error = |e: crate::kit::KitError| ctx.fail(e.to_string());
        let segments = SegmentSet::from_fit(fit.as_ref(), &mesh::all_bones()).map_err(to_build_error)?;
        let regions = mesh::build_regions(fit.layout(), &segments, mesh::Detail::of(ctx.quality), &material_id)
            .map_err(to_build_error)?;
        let mut meshes: Vec<PartMesh> = regions.into_iter().map(|(_, m)| m).collect();
        meshes.extend(hands::build(params, fit.layout(), &material_id).map_err(to_build_error)?);
        let material = skin_material(params, ctx)?;
        let mut publishes = Publishes::default().with::<FitFact>(fit);
        if let Some(stature) = stature {
            publishes.put::<StatureFact>(stature);
        }
        Ok(PartOutput::new()
            .with_meshes(meshes)
            .with_materials(vec![(material_id, material)])
            .with_publishes(publishes))
    }
}

register_part_kind!(Body);

/// The skin: the prototype's `skin-hand` toon (a low terminator shift, a soft edge, a thin warm rim) with the skin
/// treatment and a coloured outline, from the palette slots in `params.skin`.
fn skin_material(params: &BodyParams, ctx: &BuildCtx<'_>) -> Result<MaterialSpec, BuildError> {
    Ok(MaterialSpec {
        base: ctx.colour(&params.skin.base)?,
        base_texture: None,
        shading: Shading::Toon(Toon {
            shade: ctx.colour(&params.skin.shade)?,
            shift: -0.02,
            softness: 0.22,
            rim: Rim { colour: ctx.colour(&params.skin.rim)?, power: 3.0, strength: 0.12 },
            emissive: Colour::BLACK,
            skin: Some(Skin { floor: 0.66, hue_keep: 0.55 }),
        }),
        overlays: Vec::new(),
        alpha: omuzaki_render_api::AlphaMode::Opaque,
        double_sided: false,
        vertex_colours: false,
        outline: Some(Outline { colour: ctx.colour(&params.skin.line)?, width: OUTLINE_WIDTH }),
        receives_shadows: true,
        uv_transform: omuzaki_render_api::UvTransform::IDENTITY,
    })
}
