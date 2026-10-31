//! The `head` part kind registration: the head mesh, the neck, the skin materials with the painted-face overlay, and what
//! the head publishes to everything built on it (port of the prototype's `head.js` `build`).
//!
//! **Placement.** The head is authored in the template's frame ([`HeadParams::cranium`]); a cranium with no centre of its
//! own is seated on the template's head joint ([`seated`]). `Neck::drop` lowers the whole
//! head part (mesh, surface, face window, anchors and the `head` joint) relative to the template's neck top, which the
//! neck itself does not follow: a chibi chin then rests near the collar. The neck is placed from the template's `head`
//! joint, found in the body's fit, so the head part requires the `body` kind and is built after it.
//!
//! **Publishes.** The head as built as a [`HeadSurface`] ([`mesh::build_head`]: the cranium with the lower face
//! reshaped, so hair roots and ears sit on the face as drawn), the
//! [`FaceProjection`](omuzaki_face_api::FaceProjection), the `top` anchor, the [`EarCoverFact`] of each shell ear, and a
//! joint-only [`JointFit`] for `head`, `leftEye`, `rightEye` and `jaw`. A joint-only fit wins over the body whatever the
//! build order ([`Published::joint`](omuzaki_char_api::Published::joint)), so the dropped head joint is the one the
//! skeleton gets.

use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone, SpringChain, SpringParams, Transform};
use omuzaki_char_api::{
    Anchor, AnchorId, AnchorsFact, BoneName, BuildCtx, BuildError, Cranium, EarKind, ExtraBone, FaceFact, FitFact,
    HeadFact, HeadParams, HeadSurface, JointFit, JointRef, KindProvenance, MaterialSpec, PartCategory, PartKind,
    PartMesh, PartMeta, PartOutput, Publishes, RecolourCtx, Recoloured, RigContribution,
};
use omuzaki_core::glam::{Mat3, Quat, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::narrow;

use super::ears::fact::EarCoverFact;
use super::ears::{self, Side};
use super::material;
use super::mesh::{self, Resolution};
use super::neck;
use super::outline;
use crate::template::soft_sd;

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
        let placed = lowered(&seated(params, f64::from(template_head.y)));
        let centre = Vec3::from_array(placed.cranium.centre().map(narrow));
        let drop = Vec3::Y * narrow(params.neck.drop);

        let built = mesh::build_head(&placed, Resolution::of(ctx.quality), None)
            .map_err(|e| ctx.fail(format!("head mesh: {e}")))?;
        let (mut head_mesh, surface) = (built.mesh, built.surface);
        // The face window follows the outline the mesh is built from, so they agree outside the plausible space too (#693).
        let framed = HeadParams { outline: outline::plausible(&placed.outline, &placed.cranium), ..placed.clone() };
        let projection = framed.face_projection();

        weigh_head(&mut head_mesh, centre.y, below_chin_line(params));
        let mut meshes = vec![part_mesh(ctx, "head", head_mesh, vec![JointRef::HEAD, JointRef::NECK], "skin-head")?];

        // The neck stays on the template's neck top, tucked behind the chin; the head moves, the neck does not.
        let neck_top = Vec3::new(centre.x, template_head.y + neck::TOP_ABOVE_JOINT, centre.z);
        let line = template_head.y + neck::WEIGHT_LINE_ABOVE_JOINT;
        if let Some(neck_mesh) = neck::mesh(&params.neck, neck_top, line).map_err(|e| ctx.fail(format!("neck: {e}")))? {
            let bones = vec![JointRef::HEAD, JointRef::NECK, JointRef::CHEST];
            meshes.push(part_mesh(ctx, "neck", neck_mesh, bones, "skin-neck")?);
        }

        let (ear_meshes, mut ear_anchors, ear_covers) = ears(ctx, &placed, surface.as_ref(), template_head - drop)?;
        meshes.extend(ear_meshes);
        let mut rig = RigContribution::default();
        if placed.ears.kind == EarKind::Animal {
            let (animal_meshes, tips) = animal_ears(ctx, &placed, surface.as_ref(), &mut rig)?;
            meshes.extend(animal_meshes);
            ear_anchors.extend(tips);
        }
        let overlays = material::face_overlays(&framed, surface.centre());
        let materials = materials(&|c| ctx.colour(c), &placed, overlays)?
            .into_iter()
            .map(|(name, spec)| Ok((material::name(ctx, name)?, spec)))
            .collect::<Result<Vec<_>, BuildError>>()?;

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
        for cover in ear_covers {
            publishes.put::<EarCoverFact>(cover);
        }
        let mut output = PartOutput::new()
            .with_meshes(meshes)
            .with_materials(materials)
            .with_publishes(publishes)
            .with_rig(rig)
            .with_warnings(warnings);
        sized(ctx, &mut output, params.size(), template_head - drop)?;
        Ok(output)
    }

    /// The head's colours are its materials, made by the same function as the build's; the face window's overlays are
    /// geometry and are kept. The meshes don't depend on colour.
    fn recolour(params: &HeadParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        let placed = lowered(params);
        let face = format!("{}-skin-head", ctx.part);
        let Some(overlays) =
            output.materials.iter().find(|(name, _)| name.as_str() == face).map(|(_, s)| s.overlays.clone())
        else {
            return Ok(Recoloured::NeedsRebuild);
        };
        let fresh = materials(&|c| ctx.colour(c), &placed, overlays)?;
        if fresh.len() != output.materials.len() {
            return Ok(Recoloured::NeedsRebuild);
        }
        for ((name, spec), (suffix, made)) in output.materials.iter_mut().zip(fresh) {
            if name.as_str() != format!("{}-{suffix}", ctx.part) {
                return Ok(Recoloured::NeedsRebuild);
            }
            *spec = made;
        }
        Ok(Recoloured::Done)
    }
}

omuzaki_char_api::register_part_kind!(Head);

/// The head's materials, by name after the part id, in the order the build lists them: the face (with `overlays`), the
/// ears and the neck, then the fur and inner panel of animal ears. The build and the recolour both make them here.
fn materials(
    colour: material::Resolve<'_>,
    placed: &HeadParams,
    overlays: Vec<omuzaki_char_api::FaceOverlay>,
) -> Result<Vec<(&'static str, MaterialSpec)>, BuildError> {
    let mut all = vec![
        ("skin-head", material::skin_head(colour, placed, overlays)?),
        ("skin-ear", material::skin_ear(colour, placed)?),
        ("skin-neck", material::skin_neck(colour, placed)?),
    ];
    if placed.ears.kind == EarKind::Animal {
        all.push(("fur-ear", material::fur_ear(colour, placed)?));
        all.push(("ear-inner", material::ear_inner(colour, placed)?));
    }
    Ok(all)
}

/// What the ears add to the part: their meshes, their `ear_l` and `ear_r` anchors and the covers they publish.
type EarParts = (Vec<PartMesh>, Vec<(AnchorId, Anchor)>, Vec<(Side, ears::EarCover)>);

/// The ear meshes (rigid on the head, in the `skin-ear` material), their `ear_l` and `ear_r` anchors and the
/// [`EarCoverFact`] of each. The left ear roots on the head as built, the right mirrors it across the head's centre; a
/// kind with no ear on the side builds none of them.
fn ears(
    ctx: &BuildCtx<'_>,
    placed: &HeadParams,
    surface: &dyn HeadSurface,
    joint: Vec3,
) -> Result<EarParts, BuildError> {
    let left_root = ears::root(&placed.ears, surface);
    let (mut meshes, mut anchors, mut covers) = (vec![], vec![], vec![]);
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
        // The head is built at the template's size, sized about its joint and scaled to the stature afterwards, which
        // moves the anchors but not this fact, so the disc is moved here, the way `scale_output_about` moves an anchor.
        if let Some(cover) = ears::cover(&placed.ears, side, root) {
            #[allow(clippy::float_cmp)] // exactly 1 is the template's head, bit for bit
            let cover = if placed.size() == 1.0 { cover } else { cover.about(narrow(placed.size()), joint) };
            covers.push((side, cover.scaled(ctx.published.scale())));
        }
    }
    Ok((meshes, anchors, covers))
}

/// The soft springs of a rabbit's ear: it sways and settles, and gravity tips it back a little.
const EAR_ROOT_SPRING: SpringParams =
    SpringParams { hit_radius: 0.0, stiffness: 0.7, gravity_power: 0.08, gravity_dir: Vec3::NEG_Y, drag_force: 0.45 };
const EAR_TIP_SPRING: SpringParams = SpringParams { stiffness: 0.4, ..EAR_ROOT_SPRING };

/// What the animal ears add to the part: their meshes and their `ear_top_l` and `ear_top_r` anchors.
type AnimalParts = (Vec<PartMesh>, Vec<(AnchorId, Anchor)>);

/// The animal ears (`kind = "animal"`): each side's fur shell and inner panel, the `ear_top_l` and `ear_top_r` anchors at
/// their tips, and, for a species that sways, a three-bone spring chain from the head bone up the ear (added to `rig`),
/// the ear's vertices blended along it. Rigid ears ride the head bone.
fn animal_ears(
    ctx: &BuildCtx<'_>,
    placed: &HeadParams,
    surface: &dyn HeadSurface,
    rig: &mut RigContribution,
) -> Result<AnimalParts, BuildError> {
    let left_root = ears::root(&placed.ears, surface);
    let (mut meshes, mut anchors) = (vec![], vec![]);
    for side in Side::BOTH {
        let ear = ears::animal::build(&placed.ears, side, left_root, surface)
            .map_err(|e| ctx.fail(format!("animal ear: {e}")))?;
        let tag = match side {
            Side::Left => "l",
            Side::Right => "r",
        };
        let mut bones = vec![JointRef::HEAD];
        if !ear.chain.is_empty() {
            let mut ids = Vec::with_capacity(ear.chain.len());
            for (i, point) in ear.chain.iter().enumerate() {
                let name = BoneName::new(format!("ear_{}_{tag}_{i}", ctx.part))
                    .map_err(|e| ctx.fail(format!("ear bone name: {e}")))?;
                let parent = bones.last().cloned().unwrap_or(JointRef::HEAD);
                ids.push(rig.add_bone(ExtraBone {
                    name: name.clone(),
                    parent,
                    rest: Transform::from_translation(*point),
                }));
                bones.push(JointRef::Named(name));
            }
            let mut chain =
                SpringChain::tapered(format!("ear_{}_{tag}", ctx.part), &ids, EAR_ROOT_SPRING, EAR_TIP_SPRING);
            chain.collider_groups = ctx.colliders().iter().map(|group| group.name.clone()).collect();
            rig.chains.push(chain);
        }
        let (mut shell, mut panel) = (ear.shell, ear.panel);
        (shell.joints, shell.weights) = ear.shell_along.iter().map(|v| along_chain(*v, bones.len())).unzip();
        (panel.joints, panel.weights) = ear.panel_along.iter().map(|v| along_chain(*v, bones.len())).unzip();
        meshes.push(part_mesh(ctx, &format!("ear-{tag}"), shell, bones.clone(), "fur-ear")?);
        meshes.push(part_mesh(ctx, &format!("ear-{tag}-inner"), panel, bones, "ear-inner")?);
        let rotation = Quat::from_mat3(&Mat3::from_cols(
            ear.frame.across.as_vec3(),
            ear.frame.up.as_vec3(),
            ear.frame.facing.as_vec3(),
        ));
        let name = format!("ear_top_{tag}");
        let id = name.parse().map_err(|e| ctx.fail(format!("anchor '{name}': {e}")))?;
        anchors.push((id, Anchor { bone: JointRef::HEAD, position: ear.tip, rotation }));
    }
    Ok((meshes, anchors))
}

/// The skin of a vertex `v` of the way up an ear over `bones` joints (the head, then the chain's bones, if any): all
/// head for a rigid ear, else a blend of the two chain bones either side of it ([`ears::animal::CHAIN_AT`]).
#[allow(clippy::cast_possible_truncation)] // joint indices into a list of four
fn along_chain(v: f32, bones: usize) -> ([u16; 4], [f32; 4]) {
    let knots = ears::animal::CHAIN_AT;
    if bones <= 1 {
        return ([0; 4], [1.0, 0.0, 0.0, 0.0]);
    }
    let v = f64::from(v);
    let above = knots.iter().position(|&k| k >= v).unwrap_or(knots.len() - 1);
    if above == 0 || v >= knots[knots.len() - 1] {
        let at = if above == 0 { 1 } else { knots.len() as u16 };
        return ([at, 0, 0, 0], [1.0, 0.0, 0.0, 0.0]);
    }
    let t = narrow((v - knots[above - 1]) / (knots[above] - knots[above - 1]));
    let (below, above) = (above as u16, above as u16 + 1);
    ([below, above, 0, 0], [1.0 - t, t, 0.0, 0.0])
}

/// Scales the head's own output by `size` about its joint `joint` (the template's, dropped), leaving the neck mesh as
/// the body has it: a bigger or smaller head on the same neck, its painted face, eyes, jaw, ears and anchors with it.
fn sized(ctx: &BuildCtx<'_>, output: &mut PartOutput, size: f64, joint: Vec3) -> Result<(), BuildError> {
    #[allow(clippy::float_cmp)] // exactly 1 is the template's head, bit for bit
    if size == 1.0 {
        return Ok(());
    }
    let neck: Vec<PartMesh> = output.meshes.extract_if(.., |m| is_neck(&m.name)).collect();
    omuzaki_char_api::scale_output_about(ctx.part, output, narrow(size), joint)?;
    output.meshes.extend(neck);
    Ok(())
}

/// Whether a mesh of the head part is its neck (`part_mesh` names it `<part>-neck`).
fn is_neck(name: &str) -> bool {
    name == "neck" || name.ends_with("-neck")
}

/// `params` with the cranium's centre written: as the author wrote it, or, when unset, seated on the template whose head
/// joint is at `template_head_y` (#825). [`Cranium::DEFAULT_CENTRE`] is sculpted on soft-sd's neck, so an unset centre
/// rises with the template's head joint above soft-sd's, and the head meets the neck on every template and rides it
/// when the body's sliders move it. The rise is taken to the micrometre: the joint comes back from the stature's scale
/// in `f32`, and that noise must not move a soft-sd head off its own seat.
pub fn seated(params: &HeadParams, template_head_y: f64) -> HeadParams {
    let mut seated = params.clone();
    seated.cranium.centre = Some(seat(&params.cranium, template_head_y));
    seated
}

/// Where `cranium`'s centre is on a template whose head joint is at `template_head_y` ([`seated`]).
#[must_use]
pub fn seat(cranium: &Cranium, template_head_y: f64) -> [f64; 3] {
    let rise = ((template_head_y - soft_sd::TABLE.joints.head_base.y) * 1e6).round() / 1e6;
    cranium.raised_centre(rise)
}

/// `params` with the whole head lowered by the neck's drop.
fn lowered(params: &HeadParams) -> HeadParams {
    let mut placed = params.clone();
    let [x, y, z] = params.cranium.centre();
    placed.cranium.centre = Some([x, y - params.neck.drop, z]);
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
        let (a, b) = (HeadParams::default().cranium.centre(), lowered(&p).cranium.centre());
        assert_eq!((a[0], a[2]), (b[0], b[2]));
        assert!((a[1] - b[1] - 0.02).abs() < 1e-12);
    }
}
