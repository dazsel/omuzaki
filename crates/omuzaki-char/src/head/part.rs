//! The `head` part kind registration: the head mesh, the neck, the skin materials with the painted-face overlay, and what
//! the head publishes to everything built on it (port of the prototype's `head.js` `build`).
//!
//! **Placement.** The head is authored in the template's frame ([`HeadParams::cranium`]). `Neck::drop` lowers the whole
//! head part (mesh, surface, face window, anchors and the `head` joint) relative to the template's neck top, which the
//! neck itself does not follow: a chibi chin then rests near the collar. The neck is placed from the template's `head`
//! joint, found in the body's fit, so the head part requires the `body` kind and is built after it.
//!
//! **Publishes.** The head as built as a [`HeadSurface`] ([`mesh::build_head`]: the cranium with the lower face
//! reshaped, so hair roots and ears sit on the face as drawn), the
//! [`FaceProjection`](omuzaki_face_api::FaceProjection), the `top` anchor, and a joint-only [`JointFit`] for `head`,
//! `leftEye`, `rightEye` and `jaw`. A joint-only fit wins over the body
//! whatever the build order ([`Published::joint`](omuzaki_char_api::Published::joint)), so the dropped head joint is the one
//! the skeleton gets.

use std::sync::Arc;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::{
    Anchor, AnchorId, AnchorsFact, BuildCtx, BuildError, FaceFact, FitFact, HeadFact, HeadParams, HeadSurface,
    JointFit, JointRef, KindProvenance, PartCategory, PartKind, PartMesh, PartMeta, PartOutput, Publishes,
};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::narrow;

use super::ears::{self, Side};
use super::material;
use super::mesh::{self, Resolution};
use super::neck;

/// The head-local height below which the head mesh blends into the neck bone when the face has no stretched chin
/// (the prototype: world 0.60 against a head centre at 0.778).
const NO_CHIN_LINE: f64 = -0.178;
/// Just above a stretched chin's tip, the part of the head that blends into the neck bone.
const CHIN_MARGIN: f64 = 0.001;
/// Below the chin line a head vertex follows the head bone this much...
const HEAD_SHARE: f32 = 0.8;
/// ...and the neck bone this much.
const NECK_SHARE: f32 = 0.2;
/// The eye bones sit this far inside the painted face's surface, about an eyeball's radius.
const EYE_INSET: f32 = 0.03;

/// The `head` kind.
pub struct Head;

impl PartKind for Head {
    type Params = HeadParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "head",
            summary: "Build the head, neck and skin with the painted-face window.",
            category: PartCategory::Head,
            // The head joint is the body's, and the head is scaled by the stature the body publishes.
            requires: &["body"],
        }
    }

    fn provenance() -> KindProvenance {
        KindProvenance::ProjectOwned
    }

    fn build(params: &HeadParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        // The head is built at the template's size and scaled to the stature afterwards, so the body's joint (already in
        // metres) is brought back to the template's.
        let template_head = ctx
            .fit()?
            .joint(&JointRef::HEAD)
            .ok_or_else(|| ctx.fail("the body's fit has no head joint to place the head on"))?
            / ctx.published.scale();
        let placed = lowered(params);
        let centre = Vec3::from_array(placed.cranium.centre.map(narrow));
        let drop = Vec3::Y * narrow(params.neck.drop);

        let built = mesh::build_head(&placed, Resolution::of(ctx.quality), None)
            .map_err(|e| ctx.fail(format!("head mesh: {e}")))?;
        let (mut head_mesh, surface) = (built.mesh, built.surface);
        let projection = placed.face_projection();

        weigh_head(&mut head_mesh, centre.y, below_chin_line(params));
        let mut meshes = vec![part_mesh(ctx, "head", head_mesh, vec![JointRef::HEAD, JointRef::NECK], "skin-head")?];

        // The neck stays on the template's neck top, tucked behind the chin; the head moves, the neck does not.
        let neck_top = Vec3::new(centre.x, template_head.y + neck::TOP_ABOVE_JOINT, centre.z);
        let line = template_head.y + neck::WEIGHT_LINE_ABOVE_JOINT;
        if let Some(neck_mesh) = neck::mesh(&params.neck, neck_top, line).map_err(|e| ctx.fail(format!("neck: {e}")))? {
            let bones = vec![JointRef::HEAD, JointRef::NECK, JointRef::CHEST];
            meshes.push(part_mesh(ctx, "neck", neck_mesh, bones, "skin-neck")?);
        }

        let (ear_meshes, ear_anchors) = ears(ctx, &placed, surface.as_ref())?;
        meshes.extend(ear_meshes);

        let materials = vec![
            (material::name(ctx, "skin-head")?, material::skin_head(ctx, &placed, surface.centre())?),
            (material::name(ctx, "skin-ear")?, material::skin_ear(ctx, &placed)?),
            (material::name(ctx, "skin-neck")?, material::skin_neck(ctx, &placed)?),
        ];

        let top = Anchor {
            bone: JointRef::HEAD,
            position: surface.centre() + Vec3::Y * narrow(placed.cranium.half_height_up),
            rotation: Quat::IDENTITY,
        };
        let anchor = |name: &str| name.parse().map_err(|e| ctx.fail(format!("anchor '{name}': {e}")));
        let fit = JointFit::new(joints(ctx, &placed, surface.as_ref(), template_head - drop));
        let warnings = placed
            .traced
            .iter()
            .map(|_| "the traced radius map is not applied yet: the head is the analytic cranium".to_owned())
            .collect();
        let mut publishes = Publishes::default()
            .with::<HeadFact>(surface)
            .with::<FaceFact>(projection)
            .with::<FitFact>(Arc::new(fit))
            .with::<AnchorsFact>((anchor("top")?, top));
        for ear in ear_anchors {
            publishes.put::<AnchorsFact>(ear);
        }
        Ok(PartOutput { meshes, materials, publishes, warnings, ..PartOutput::default() })
    }
}

omuzaki_char_api::register_part_kind!(Head);

/// What the ears add to the part: their meshes and anchors.
type EarParts = (Vec<PartMesh>, Vec<(AnchorId, Anchor)>);

/// The ear meshes (rigid on the head, in the `skin-ear` material) and their `ear_l` and `ear_r` anchors. The left ear
/// roots on the head as built, the right mirrors it across the head's centre; a kind with no ear on the side builds
/// neither.
fn ears(ctx: &BuildCtx<'_>, placed: &HeadParams, surface: &dyn HeadSurface) -> Result<EarParts, BuildError> {
    let left_root = ears::root(&placed.ears, surface);
    let (mut meshes, mut anchors) = (vec![], vec![]);
    for side in Side::BOTH {
        let root = match side {
            Side::Left => left_root,
            Side::Right => Vec3::new(2.0 * surface.centre().x - left_root.x, left_root.y, left_root.z),
        };
        let Some(mut geometry) =
            ears::build(&placed.ears, side, root, surface).map_err(|e| ctx.fail(format!("ear: {e}")))?
        else {
            continue;
        };
        // Rigid on the head bone: an ear does not follow the neck.
        geometry.joints = vec![[0; 4]; geometry.positions.len()];
        geometry.weights = vec![[1.0, 0.0, 0.0, 0.0]; geometry.positions.len()];
        let name = ears::anchor_name(side);
        meshes.push(part_mesh(ctx, &name.replace('_', "-"), geometry, vec![JointRef::HEAD], "skin-ear")?);
        if let Some(bind) = ears::anchor(&placed.ears, side, root) {
            anchors.push((name.parse().map_err(|e| ctx.fail(format!("anchor '{name}': {e}")))?, bind));
        }
    }
    Ok((meshes, anchors))
}

/// `params` with the whole head lowered by the neck's drop.
fn lowered(params: &HeadParams) -> HeadParams {
    let mut placed = params.clone();
    placed.cranium.centre[1] -= params.neck.drop;
    placed
}

/// The head-local height below which the head mesh blends into the neck bone: the stretched chin's tip when there is
/// one, else a fixed line under the cheeks.
pub fn below_chin_line(params: &HeadParams) -> f32 {
    narrow(params.outline.chin.map_or(NO_CHIN_LINE, |chin| chin.y + CHIN_MARGIN))
}

/// The skin weights of a head vertex at head-local height `local_y`: all head above `line`
/// ([`below_chin_line`]), 0.8 head and 0.2 neck below it, so the chin does not shear off the neck when the head turns.
pub fn head_weights(local_y: f32, line: f32) -> ([u16; 4], [f32; 4]) {
    if local_y < line { ([0, 1, 0, 0], [HEAD_SHARE, NECK_SHARE, 0.0, 0.0]) } else { ([0; 4], [1.0, 0.0, 0.0, 0.0]) }
}

/// Skins the head mesh with [`head_weights`], heights taken from the head centre `centre_y`.
fn weigh_head(mesh: &mut MeshData, centre_y: f32, line: f32) {
    (mesh.joints, mesh.weights) = mesh.positions.iter().map(|p| head_weights(p.y - centre_y, line)).unzip();
}

/// A skinned mesh of this part, named and given the material `material_name` prefixed by the part id.
fn part_mesh(
    ctx: &BuildCtx<'_>,
    name: &str,
    geometry: MeshData,
    joint_bones: Vec<JointRef>,
    material_name: &str,
) -> Result<PartMesh, BuildError> {
    Ok(PartMesh {
        name: format!("{}-{name}", ctx.part),
        geometry,
        joint_bones,
        material: material::name(ctx, material_name)?,
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    })
}

/// The joints the head part places: `head` at `head_joint` (the template's, dropped), the eyes on the face layout and the
/// jaw by the ears, all in bind space. The character's left is +X.
fn joints(ctx: &BuildCtx<'_>, p: &HeadParams, surface: &dyn HeadSurface, head_joint: Vec3) -> [(JointRef, Vec3); 4] {
    let centre = surface.centre();
    let eye = |side: f32| {
        let (x, y) = (side * narrow(ctx.face.layout.eye_spacing), narrow(ctx.face.layout.eye_y));
        // Where the ray through the eye's spot on the face plane leaves the cranium, then back in by an eyeball.
        let on_face = surface.point(Vec3::new(x, y, narrow(p.cranium.half_depth_front)).normalize_or_zero(), 0.0);
        Vec3::new(centre.x + x, centre.y + y, on_face.z - EYE_INSET)
    };
    let [_, ear_y, ear_z] = p.ears.position.map(narrow);
    [
        (JointRef::HEAD, head_joint),
        (JointRef::Humanoid(HumanoidBone::LeftEye), eye(1.0)),
        (JointRef::Humanoid(HumanoidBone::RightEye), eye(-1.0)),
        // The jaw pivots by the ears: level with them, centred.
        (JointRef::Humanoid(HumanoidBone::Jaw), Vec3::new(centre.x, centre.y + ear_y, centre.z + ear_z)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_char_api::Chin;

    #[test]
    fn the_chin_line_follows_a_stretched_chin_else_sits_under_the_cheeks() {
        let mut p = HeadParams::default();
        assert!((below_chin_line(&p) - (-0.178)).abs() < 1e-6);
        p.outline.chin = Some(Chin { y: -0.3, ..Chin::default() });
        assert!((below_chin_line(&p) - (-0.299)).abs() < 1e-6);
    }

    #[test]
    fn weigh_head_splits_below_the_line_and_normalises() {
        let mut m =
            MeshData { positions: vec![Vec3::new(0.0, 0.1, 0.0), Vec3::new(0.0, -0.3, 0.0)], ..MeshData::default() };
        weigh_head(&mut m, 0.0, -0.178);
        assert_eq!(m.weights, vec![[1.0, 0.0, 0.0, 0.0], [0.8, 0.2, 0.0, 0.0]]);
        assert_eq!(m.joints[1], [0, 1, 0, 0]);
    }

    #[test]
    fn lowering_moves_only_the_vertical_centre() {
        let mut p = HeadParams::default();
        p.neck.drop = 0.02;
        let (a, b) = (HeadParams::default().cranium.centre, lowered(&p).cranium.centre);
        assert_eq!((a[0], a[2]), (b[0], b[2]));
        assert!((a[1] - b[1] - 0.02).abs() < 1e-12);
    }
}
