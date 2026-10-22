//! Attachments on an imported avatar (`docs/design/byom.md` D1, condition 2; ADR 0033 decision 1).
//!
//! An imported character's `avatar.parts` are kit parts (a halo, a hairpin, a prop), each fixed by the kit's own
//! [`Attach`](omuzaki_char_api::Attach) path to a bone or to one of the avatar's anchors
//! ([`derive_anchors`](omuzaki_avatar_api::anchors::derive_anchors)). [`attach_parts`] builds them the way the kit builds
//! any part ([`plan`], [`build_part`], [`Assembler`]), against one part standing for the model itself,
//! [`AVATAR_PART`], which publishes what an attachment reads of a body: the avatar's joints in its bind pose and its
//! anchors. The meshes that come out are in the avatar's bind space, so each joins the avatar's meshes with the inverse
//! of its bone's bind matrix, and the stage, recording and export see one body.
//!
//! Nothing here places geometry: where a part sits is the kit's `Attach` resolution, and this module only feeds it.
//! Two limits, each refused with what to do: a part that adds bones of its own (a halo's bob, a chain) has nowhere to
//! put them on an imported skeleton yet, and kinds that read the kit head's surface (hats, glasses) have none to read.

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_avatar_api::anchors::{AVATAR_PART, HEAD_TOP, bind_matrices, bone_anchor};
use omuzaki_avatar_api::avatar::{AvatarMesh, ImportedAvatar};
use omuzaki_char_api::{
    Anchor, AnchorId, AnchorsFact, Assembler, BodyFit, BodyRegion, BodyTemplateId, BuildOptions, BuildServices, CharId,
    CharacterDef, FitFact, JointRef, PartDef, PartId, PartKindId, PartOutput, Publishes, Quality as BuildQuality,
    Section, build_part, plan,
};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_ops_api::OpError;

/// The kind of the part that stands for the model. No kind is registered under it: the part is never built, only fed.
const MODEL_KIND: &str = "imported-model";

/// The template the attachments' definition names. Kinds that attach to a body read its joints and anchors, never the
/// template, so it is only a valid id.
const TEMPLATE: &str = "imported";

/// The kit head's name for its top: a kind that floats over the head by default (the halo) looks it up, so the
/// avatar's [`HEAD_TOP`] is published under it too.
const KIT_TOP: &str = "top";

/// What an attachment reads of the avatar's body: its joints in its bind pose, and that pose. It wraps nothing
/// (garments need sections an imported mesh does not have).
#[derive(Debug)]
struct AvatarFit {
    joints: BTreeMap<JointRef, Vec3>,
    bind: HumanoidPose,
}

impl BodyFit for AvatarFit {
    fn section(&self, _: BodyRegion, _: f32) -> Option<Section> {
        None
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        self.joints.get(joint).copied()
    }

    fn envelope(&self) -> bool {
        false
    }

    fn bind(&self) -> HumanoidPose {
        self.bind
    }
}

/// Builds `parts` against `avatar` and adds their meshes, materials and textures to it, so it draws them as its own.
/// Nothing changes when `parts` is empty. Cold: builds and allocates.
///
/// # Errors
/// `Invalid` on `parts`, naming the part, when a part uses the id reserved for the model, does not build (with the
/// kit's reason), moves a bone the model does not have, or defines a material or texture the model already has; on an
/// error `avatar` is unchanged.
pub fn attach_parts(
    avatar: &mut ImportedAvatar,
    id: &CharId,
    parts: &BTreeMap<PartId, PartDef>,
    services: &BuildServices,
    quality: BuildQuality,
) -> Result<(), OpError> {
    if parts.is_empty() {
        return Ok(());
    }
    let invalid = |message: String| OpError::invalid("parts", message);
    let internal = |what: &str, e: &dyn std::fmt::Display| OpError::internal(format!("{what}: {e}"));
    let model = PartId::new(AVATAR_PART).map_err(|e| internal("the model's part id", &e))?;
    if parts.contains_key(&model) {
        return Err(
            invalid(format!("the part id `{AVATAR_PART}` is the model itself")).hint("give the attachment another id")
        );
    }
    let mut all = parts.clone();
    all.insert(
        model.clone(),
        PartDef {
            kind: PartKindId::new(MODEL_KIND).map_err(|e| internal("the model's kind", &e))?,
            parent: None,
            enabled: true,
            layer: i32::MIN,
            attach: None,
            params: serde_json::Value::Null,
        },
    );
    let template = BodyTemplateId::new(TEMPLATE).map_err(|e| internal("the template id", &e))?;
    let def = CharacterDef::new(id.as_str(), template).with_parts(all);
    let options = BuildOptions { quality, ..BuildOptions::default() };
    let failed = |e: &dyn std::fmt::Display| {
        invalid(format!("an attachment of '{id}' doesn't build: {e}")).hint(
            "attach it to a bone or anchor the model has (`avatar`'s `head-top`, `left-grip`, a bone such as `head`)",
        )
    };
    let mut assembler = Assembler::new(id, &def, &options);
    let mut seed = Some(model_output(avatar));
    for part in plan(&def).map_err(|e| failed(&e))? {
        let output = match seed.take_if(|_| part == model) {
            Some(output) => output,
            None => build_part(&def, &part, &options, services, assembler.published()).map_err(|e| failed(&e))?,
        };
        assembler.add(&part, output).map_err(|e| failed(&e))?;
    }
    let built = assembler.finish().map_err(|e| failed(&e))?;

    let bind = bind_matrices(&avatar.skeleton, &avatar.bind);
    let mut meshes = Vec::with_capacity(built.meshes.len());
    for built_mesh in built.meshes {
        let part = &built_mesh.part;
        let inverse_bind = built_mesh
            .mesh
            .joint_bones
            .iter()
            .map(|joint| {
                let bone = match joint {
                    JointRef::Humanoid(bone) => avatar.skeleton.humanoid_bone(*bone),
                    JointRef::Named(name) => avatar.skeleton.find(name.as_str()),
                };
                bone.and_then(|b| bind.get(b.index())).map(Mat4::inverse).ok_or_else(|| {
                    invalid(format!(
                        "part '{part}' moves the bone {joint:?}, which the model doesn't have: a part with bones of its \
                         own can't go on an imported model yet"
                    ))
                    .hint("turn off the part's own motion (a halo's `bob`), or attach a part without bones")
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        meshes.push(AvatarMesh::new(built_mesh.mesh).with_inverse_bind(inverse_bind).with_part(part.clone()));
    }
    let clash = |what: &str, name: &dyn std::fmt::Display| {
        invalid(format!("an attachment defines {what} '{name}', which the model already has"))
            .hint("rename the attachment: its materials and textures take its id")
    };
    if let Some(name) = built.materials.keys().find(|n| avatar.materials.contains_key(*n)) {
        return Err(clash("the material", name));
    }
    if let Some(name) = built.textures.keys().find(|n| avatar.textures.contains_key(*n)) {
        return Err(clash("the texture", name));
    }
    avatar.meshes.extend(meshes);
    avatar.materials.extend(built.materials);
    avatar.textures.extend(built.textures);
    Ok(())
}

/// What the model publishes for its attachments: its joints at its bind pose ([`FitFact`]) and its anchors
/// ([`AnchorsFact`]), the head's top also under the kit head's name.
fn model_output(avatar: &ImportedAvatar) -> PartOutput {
    let joints = avatar
        .anchors
        .iter()
        .filter(|(name, anchor)| is_joint(name, anchor))
        .map(|(_, anchor)| (anchor.bone.clone(), anchor.position))
        .collect();
    let mut publishes = Publishes::default();
    publishes.put::<FitFact>(Arc::new(AvatarFit { joints, bind: avatar.bind }));
    for (name, anchor) in &avatar.anchors {
        publishes.put::<AnchorsFact>((name.clone(), anchor.clone()));
    }
    if let (Some(top), Ok(kit)) = (avatar.anchors.iter().find(|(n, _)| n.as_str() == HEAD_TOP), AnchorId::new(KIT_TOP))
    {
        publishes.put::<AnchorsFact>((kit, top.1.clone()));
    }
    PartOutput::new().with_publishes(publishes)
}

/// Whether anchor `name` is the joint anchor of its bone (named for it), not a point placed off it (`head-top`, a grip).
fn is_joint(name: &AnchorId, anchor: &Anchor) -> bool {
    match &anchor.bone {
        JointRef::Humanoid(bone) => bone_anchor(*bone) == name.as_str(),
        JointRef::Named(_) => false,
    }
}
