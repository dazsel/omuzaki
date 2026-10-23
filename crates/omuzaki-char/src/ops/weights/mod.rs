//! The `rig.weights` ops: brushes that move skin weight toward a joint around a point (RG-02).
//!
//! A brush is data. It is stored in the character as `characters/<id>/weight_brushes/<part>`, a list of
//! [`WeightBrush`]es, and the builder applies a part's brushes right after the part builds. Brushes are positional
//! (bind-space centre and radius) and the joints they name are the bind-pose joints (ADR 0014), so they survive
//! parameter edits that move the vertices, and no pose is special-cased here.
//!
//! | Module | Holds |
//! |---|---|
//! | [`brush`] | `rig.weights.brush`: appends one brush, after checking its joint against the skeleton |
//! | [`mirror`] | `rig.weights.mirror`: appends the mirror image of each brush, with the sides swapped |
//! | [`clear`] | `rig.weights.clear`: removes one brush, or all of a part's |
//! | [`report`] | `rig.weights.report`: a query on the built mesh, and the brushes that reach no vertex or cross the plane |
//!
//! Each op edits one part's list and nothing else, so undo restores it exactly.

use omuzaki_char_api::{
    BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, JointRef, PartId,
    RegistryBuilder, WeightBrush, paths,
};
use omuzaki_core::text::nearest;
use omuzaki_doc_api::Change;
use omuzaki_face_api::{DrawList, FaceError, FaceLayerSpec, FaceRasterizer, Raster, Validate};
use omuzaki_ops_api::{OpCtx, OpError};

use crate::kit::skin::brush_mirrored;
use crate::ops::character::access;

pub mod brush;
pub mod clear;
pub mod mirror;
pub mod report;

/// The brushes `part` holds, in the order they were added (none when it holds none).
pub(crate) fn brushes_of(def: &CharacterDef, part: &PartId) -> Vec<WeightBrush> {
    def.weight_brushes.get(part).cloned().unwrap_or_default()
}

/// Checks one brush against its own rules, naming the first field at fault (`centre/1`, `radius`).
///
/// # Errors
/// `invalid`, with the field path of the first rule the brush breaks.
pub(crate) fn check_brush(brush: &WeightBrush) -> Result<(), OpError> {
    brush.validate().map_err(|errors| {
        let first = errors.0.first().map_or_else(String::new, |e| e.path.clone());
        OpError::invalid(first, errors.to_string())
            .hint("centre is three numbers in metres from -10 to 10, radius is above 0 metres, strength is from 0 to 1")
    })
}

/// Builds the character as `def` stands, for the ops that need its skeleton, its skin or its simulation: the geometry
/// only, its painted textures (a face, a plaid, a decal) left blank, so a part that paints builds without the app's
/// rasteriser.
///
/// # Errors
/// `invalid` (field `character`) naming the build error when the character does not build.
pub(crate) fn build_character(id: &CharId, def: &CharacterDef) -> Result<BuiltCharacter, OpError> {
    RegistryBuilder.build(id, def, &BuildOptions::default(), &geometry_services()).map_err(|e: BuildError| {
        OpError::invalid("character", format!("character `{id}` doesn't build: {e}"))
            .hint("fix the part the message names (a brush's joint or radius can cause it), then try again")
    })
}

/// The services of a build that reads only geometry: a rasteriser that leaves every painted texture blank, so a part
/// that paints (a face, a plaid, a decal) builds without the app's.
pub(crate) fn geometry_services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(Blank))
}

/// A rasteriser that paints nothing: every texture comes out transparent at its size, for builds that only read
/// geometry.
#[derive(Debug)]
struct Blank;

impl FaceRasterizer for Blank {
    fn rasterize(&mut self, spec: &FaceLayerSpec, _: &DrawList, px_per_m: f32, out: &mut Raster) -> Result<(), FaceError> {
        let (w, h) = spec.rect.size_px(px_per_m);
        *out = Raster::new(w, h);
        Ok(())
    }
}

/// Refuses a brush joint the character's skeleton does not have, with the field `joint` and the nearest bone name.
///
/// The skeleton does not depend on the brushes, so the built character's rig is the one the brush will meet.
///
/// # Errors
/// `invalid` (field `joint`) naming the joint, the character and the nearest bone the skeleton has.
pub(crate) fn check_joint(built: &BuiltCharacter, character: &CharId, joint: &JointRef) -> Result<(), OpError> {
    if built.rig.bone(joint).is_some() {
        return Ok(());
    }
    let given = joint_name(joint);
    let hint = match nearest(&given, built.rig.skeleton.bones().iter().map(|b| b.name.as_str())) {
        Some(near) => {
            format!("the nearest bone is `{near}`: name it as {{\"named\": \"{near}\"}}, or pick another bone")
        }
        None => "the character's skeleton has no bones to name".to_owned(),
    };
    Err(OpError::invalid("joint", format!("`{given}` is not a bone of `{character}`")).hint(hint))
}

/// A joint as a person names it: the VRM bone name, or the extra bone's name.
pub(crate) fn joint_name(joint: &JointRef) -> String {
    match joint {
        JointRef::Humanoid(bone) => serde_json::to_value(bone).ok().and_then(|v| v.as_str().map(str::to_owned)),
        JointRef::Named(name) => Some(name.as_str().to_owned()),
    }
    .unwrap_or_default()
}

/// The brushes that cross the centre plane (`|x| < radius`) and whose mirror image is a different brush: their two sides
/// meet on the plane. Information only: `apply_brushes` applies such a pair together, so the skin stays symmetric.
/// A brush with a centre-line joint on the plane is its own mirror, so it is not listed.
pub(crate) fn straddling(brushes: &[WeightBrush]) -> Vec<usize> {
    brushes
        .iter()
        .enumerate()
        .filter(|(_, brush)| brush.centre[0].abs() < brush.radius && brush_mirrored(brush) != **brush)
        .map(|(index, _)| index)
        .collect()
}

/// The changes that store `brushes` as the part's whole list. An empty list removes the part's entry, so no empty
/// list or `null` is left in the file. A part with no entry that stays without one gets no change.
///
/// # Errors
/// `internal` if the brushes cannot be serialised, which their plain fields rule out.
pub(crate) fn store(
    ctx: &OpCtx<'_>,
    character: &CharId,
    part: &PartId,
    brushes: &[WeightBrush],
) -> Result<Vec<Change>, OpError> {
    let path = paths::weight_brushes(character).join(part.as_str());
    let exists = ctx.doc.get(&path).is_some();
    if brushes.is_empty() {
        return Ok(if exists { vec![access::change(ctx, path, None)] } else { Vec::new() });
    }
    let value = serde_json::to_value(brushes)
        .map_err(|e| OpError::internal(format!("serialising the weight brushes of `{part}` failed: {e}")))?;
    Ok(if exists { vec![access::change(ctx, path, Some(value))] } else { access::create(ctx, path, value) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_brush_that_crosses_the_plane_with_a_different_mirror_is_listed() {
        let at = |x: f64, radius: f64, joint: JointRef| WeightBrush {
            centre: [x, 0.0, 0.0],
            radius,
            joint,
            strength: 0.5,
            falloff: omuzaki_char_api::BrushFalloff::Smooth,
        };
        let left = JointRef::Named("hair_l_2".parse().unwrap());
        let brushes = [
            at(0.0, 0.1, JointRef::CHEST), // on the plane, its own mirror: not listed
            at(0.02, 0.05, left.clone()),  // crosses the plane: listed
            at(0.2, 0.05, left),           // well off the plane: not listed
            at(0.0, 0.1, JointRef::Named("hair_l_2".parse().unwrap())), // on the plane with a side joint: listed
        ];
        assert_eq!(straddling(&brushes), [1, 3]);
    }
}
