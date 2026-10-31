//! The `body` part kind: its layout from the character's template, its meshes, its skin material, and what it publishes
//! (the fit, the template's widened to hold the mesh, with the bind pose; and the character's real-world stature).

use std::sync::Arc;

use omuzaki_char_api::{
    BodyParams, BodyRegion, BuildCtx, BuildError, FitFact, KindProvenance, MaterialName, MaterialSpec, PartCategory,
    PartKind, PartMesh, PartMeta, PartOutput, Publishes, RecolourCtx, Recoloured, StatureFact, register_part_kind,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::ColourRef;
use omuzaki_render_api::{Outline, Rim, Shading, Skin, Toon};

use super::fit::BodyMeshFit;
use super::{hands, mesh};
use crate::kit::skin::SegmentSet;
use crate::template::registry;
use crate::template::skeleton::TemplateFit;

/// The name of the body's skin material.
const SKIN_MATERIAL: &str = "body-skin";
/// The name of the hands' material: the skin, whose vertex colours carry the warmer tint towards the fingertips.
const HAND_MATERIAL: &str = "body-hands";
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
        let fit = TemplateFit::new(layout);
        let material_id = MaterialName::new(SKIN_MATERIAL).map_err(|e| ctx.fail(format!("internal id: {e}")))?;
        let to_build_error = |e: crate::kit::KitError| ctx.fail(e.to_string());
        let segments = SegmentSet::from_fit(&fit, &mesh::all_bones()).map_err(to_build_error)?;
        let regions = mesh::build_regions(fit.layout(), &segments, mesh::Detail::of(ctx.quality), &material_id)
            .map_err(to_build_error)?;
        let hand_id = MaterialName::new(HAND_MATERIAL).map_err(|e| ctx.fail(format!("internal id: {e}")))?;
        let hand_meshes = hands::build(params, fit.layout(), &hand_id).map_err(to_build_error)?;
        // The published fit holds each region's own mesh: the bridged rings at the hip and ankle stand outside the
        // layout's sections. An arm's takes in its hand, which lies past its end: the arm's cap there is the hand, what
        // a cuff or a sleeve's end clears.
        let geometry: Vec<(BodyRegion, MeshData)> = regions
            .iter()
            .map(|(region, m)| {
                let hand = match region {
                    BodyRegion::ArmL => hand_meshes.iter().find(|h| h.name == hands::LEFT),
                    BodyRegion::ArmR => hand_meshes.iter().find(|h| h.name == hands::RIGHT),
                    _ => None,
                };
                (*region, hand.map_or_else(|| m.geometry.clone(), |hand| joined(&m.geometry, &hand.geometry)))
            })
            .collect();
        let body_fit = Arc::new(BodyMeshFit::new(fit, geometry.iter().map(|(region, m)| (*region, m))));
        let mut meshes: Vec<PartMesh> = regions.into_iter().map(|(_, m)| m).collect();
        meshes.extend(hand_meshes);
        let (skin, hand) = skin_materials(params, |c| ctx.colour(c))?;
        let mut publishes = Publishes::default().with::<FitFact>(body_fit);
        if let Some(stature) = stature {
            publishes.put::<StatureFact>(stature);
        }
        Ok(PartOutput::new()
            .with_meshes(meshes)
            .with_materials(vec![(material_id, skin), (hand_id, hand)])
            .with_publishes(publishes))
    }

    /// The body's colours are its two materials; the meshes (the hands' vertex colours included) don't depend on them.
    fn recolour(params: &BodyParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        let (skin, hand) = skin_materials(params, |c| ctx.colour(c))?;
        for (name, spec) in &mut output.materials {
            *spec = match name.as_str() {
                SKIN_MATERIAL => skin.clone(),
                HAND_MATERIAL => hand.clone(),
                _ => return Ok(Recoloured::NeedsRebuild),
            };
        }
        Ok(Recoloured::Done)
    }
}

register_part_kind!(Body);

/// The positions and triangles of `a` and `b` as one mesh, which is all the fit reads.
fn joined(a: &MeshData, b: &MeshData) -> MeshData {
    let base = u32::try_from(a.positions.len()).unwrap_or(u32::MAX);
    MeshData {
        positions: a.positions.iter().chain(&b.positions).copied().collect(),
        indices: a.indices.iter().copied().chain(b.indices.iter().map(|i| i.saturating_add(base))).collect(),
        ..MeshData::default()
    }
}

/// The skin: the prototype's `skin-hand` toon (a low terminator shift, a soft edge, a thin warm rim) with the skin
/// treatment and a coloured outline, from the palette slots in `params.skin`, with the vertex colours on: the legs' soft
/// ambient darkening ([`mesh`]'s module docs).
fn skin_material(
    params: &BodyParams,
    colour: impl Fn(&ColourRef) -> Result<Colour, BuildError>,
) -> Result<MaterialSpec, BuildError> {
    Ok(MaterialSpec {
        base: colour(&params.skin.base)?,
        base_texture: None,
        shading: Shading::Toon(Toon {
            shade: colour(&params.skin.shade)?,
            shift: -0.02,
            softness: 0.22,
            rim: Rim { colour: colour(&params.skin.rim)?, power: 3.0, strength: 0.12 },
            emissive: Colour::BLACK,
            skin: Some(Skin { floor: 0.66, hue_keep: 0.55 }),
        }),
        overlays: Vec::new(),
        painted: None,
        alpha: omuzaki_render_api::AlphaMode::Opaque,
        order_independent: false,
        double_sided: false,
        vertex_colours: true,
        outline: Some(Outline { colour: colour(&params.skin.line)?, width: OUTLINE_WIDTH }),
        receives_shadows: true,
        hair_shadow: omuzaki_render_api::HairShadow::NONE,
        uv_transform: omuzaki_render_api::UvTransform::IDENTITY,
    })
}

/// The body's two materials from its colours: the skin, and the hands' (the same skin, under its own name so a hand can
/// be told apart in the outliner and the export). The build and the recolour both make them here, so the two cannot drift apart.
fn skin_materials(
    params: &BodyParams,
    colour: impl Fn(&ColourRef) -> Result<Colour, BuildError>,
) -> Result<(MaterialSpec, MaterialSpec), BuildError> {
    let skin = skin_material(params, colour)?;
    let hand = skin.clone();
    Ok((skin, hand))
}
