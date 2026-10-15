//! The VRM avatar block: the humanoid map, `meta` from the licence record and provenance, first-person annotations,
//! look-at offsets and the expression placeholders the face baker fills.
//!
//! [`gather`](super::gather) builds the geometry; the two functions here finish the scene, and `io.export` and
//! `io.check_export` call them in this order:
//! 1. [`add_rights`] writes `scene.meta` and `scene.provenance` from the project's [`LicenceRecord`]. Every format
//!    needs it, because the rights rules of `preflight` apply to a plain GLB too.
//! 2. [`add_avatar`] writes `scene.avatar` for a VRM.
//!
//! **Rights are a legal surface, so nothing is made up.** The record is the only source of licence data: terms the
//! record does not hold stay at the VRM defaults (the most restrictive value of each flag), and fields the file
//! requires but the record lacks (the licence URL, the authors) stay empty, so `vrm_profile` reports them with a hint
//! instead of the file claiming something nobody decided. See [`add_rights`] for the few values that are carried over
//! from what the project does hold.

use omuzaki_anim_api::{BoneId, HumanoidBone, Skeleton};
use omuzaki_char_api::{BuiltCharacter, CharId, PartCategory, PartId, find_kind, paths};
use omuzaki_core::glam::Mat4;
use omuzaki_doc_api::DocPath;
use omuzaki_io_api::{
    Avatar, CreditNotation, Expression, ExpressionName, Finding, FirstPerson, FirstPersonFlag, IoScene, LookAt,
    LookAtKind, MeshAnnotation, Node, NodeIx, PresetExpression, RangeMap, VrmMeta,
};
use omuzaki_store_api::Provenance;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{GatherError, Gathered, skeleton, springs};

/// The record version this build reads and writes (`schema = 1`). A record carries it so a later layout can be told
/// from this one and migrated, never guessed.
pub const SCHEMA: u32 = 1;

/// What the project records about the rights to one character (spec §7): the only source of licence data for an export.
///
/// It lives in the document at [`record_path`]. A character with no record exports nothing but a failing check
/// (`rights.provenance_missing`): rights are never guessed from the parts. `licence.derive` computes the record the
/// parts and application records allow and `licence.set` stores one (`ops::licence`). Read one from its document node with
/// [`LicenceRecord::from_node`], which names an unknown `schema` before anything else.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LicenceRecord {
    /// The record's version, [`SCHEMA`]. Required: a record without one is not one of ours.
    pub schema: u32,
    /// The §7 provenance record the licence gate reads: author, SPDX licence, attribution, commercial status, consent
    /// and the private-test flag.
    pub provenance: Provenance,
    /// The licence facts of a VRM file that provenance has no field for: the licence URL, the permission flags,
    /// copyright, contact, references. Keys are those of `VRMC_vrm.meta` (`licenseUrl`, `commercialUsage`, ...). What
    /// the record leaves out takes the spec default, which is also the most restrictive value.
    #[serde(default)]
    pub meta: VrmMeta,
    /// Why the record grants more than the parts' recorded terms: the project owns every part, as the people named here
    /// asserted. `licence.set` writes it, and only when the record needed it; a value passed in is replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned: Option<OwnedBasis>,
}

/// The basis on which a record may grant more than the join of its parts' terms: every part is the project's own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OwnedBasis {
    /// Who asserted that the parts are the project's own (sorted, without repeats). Each name is recorded on a part's
    /// source row (`characters/<id>/sources/<part>`).
    pub asserted_by: Vec<String>,
}

/// Why a stored licence record can't be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// `schema` is missing, or a version this build doesn't read.
    #[error("`schema` is {found}, but this build reads only schema {SCHEMA}")]
    Schema {
        /// What the record has there, as written (`missing`, `2`, `"1"`).
        found: String,
    },
    /// The rest of the record doesn't fit its layout.
    #[error("{0}")]
    Invalid(String),
}

impl RecordError {
    /// The field at fault, for an op error: `schema`, or `licence` for the record as a whole.
    pub fn field(&self) -> &'static str {
        match self {
            Self::Schema { .. } => "schema",
            Self::Invalid(_) => "licence",
        }
    }
}

impl LicenceRecord {
    /// A record of the current [`SCHEMA`].
    pub fn new(provenance: Provenance, meta: VrmMeta) -> Self {
        Self { schema: SCHEMA, provenance, meta, owned: None }
    }

    /// Reads the record stored at [`record_path`]. The `schema` is checked first, so a record from a newer layout is
    /// reported as that and not as whichever of its fields stopped fitting.
    ///
    /// # Errors
    /// [`RecordError::Schema`] for a missing or unknown `schema`, [`RecordError::Invalid`] for anything else.
    pub fn from_node(node: &Value) -> Result<Self, RecordError> {
        check_schema(node)?;
        let record = Self::deserialize(node).map_err(|e| RecordError::Invalid(e.to_string()))?;
        match blank_rights_field(&record.provenance.licence, record.provenance.consent.as_ref()) {
            Some(fault) => Err(RecordError::Invalid(fault)),
            None => Ok(record),
        }
    }
}

/// The first required rights field that is blank, as a sentence naming it: the licence (give `LicenseRef-Unknown` when
/// nobody knows) or the person or scope of a consent. The one rule for every stored layout of rights, so a record, a
/// source row and an application record cannot disagree about what blank means.
pub(crate) fn blank_rights_field(licence: &str, consent: Option<&omuzaki_store_api::Consent>) -> Option<String> {
    if licence.trim().is_empty() {
        return Some(
            "`provenance.licence` is blank: give an SPDX id, or `LicenseRef-Unknown` if nobody knows".to_owned(),
        );
    }
    let consent = consent?;
    [("person", &consent.person), ("scope", &consent.scope)]
        .into_iter()
        .find(|(_, text)| text.trim().is_empty())
        .map(|(field, _)| format!("`provenance.consent.{field}` is blank: a consent names who agreed and to what"))
}

/// Checks that `node` says it is [`SCHEMA`], before anything else about it is read. Shared by every stored layout of
/// this crate's licence data (the record and the per-part sources), so a later version is told apart the same way.
///
/// # Errors
/// [`RecordError::Schema`] for a missing or unknown `schema`.
pub(crate) fn check_schema(node: &Value) -> Result<(), RecordError> {
    match node.get("schema") {
        Some(version) if version.as_u64() == Some(u64::from(SCHEMA)) => Ok(()),
        other => Err(RecordError::Schema { found: other.map_or_else(|| "missing".to_owned(), Value::to_string) }),
    }
}

/// Where the project keeps the [`LicenceRecord`] of character `id`: `characters/<id>/licence`. It is a sibling sub-tree
/// of the character node, like `variants`, `colourways`, `rig` and `look`, so it moves with the character when it is
/// renamed, duplicated or deleted by design. `CharacterDef` does not list it, so reading the character skips it.
pub fn record_path(id: &CharId) -> DocPath {
    paths::character(id).join("licence")
}

/// Writes `scene.meta` and `scene.provenance` from `record`; `None` leaves the provenance unset, which `preflight`
/// reports as `rights.provenance_missing`.
///
/// The meta is the record's own, with four carry-overs from what the project holds, none of which widens a right:
/// - `name` is the character's display name when the record has none;
/// - `authors` is the provenance author when the record lists none;
/// - an `attribution` in the provenance means credit is required, and the text travels as the copyright information
///   unless the record already gives some, so a CC BY credit is never dropped on export;
/// - `thumbnailImage` is cleared: it indexes the images of the file the record came from, not of this scene.
pub fn add_rights(scene: &mut IoScene, built: &BuiltCharacter, record: Option<&LicenceRecord>) {
    let mut meta = record.map(|r| r.meta.clone()).unwrap_or_default();
    meta.thumbnail_image = None;
    if is_blank(&meta.name) {
        meta.name.clone_from(&built.def.name);
    }
    let provenance = record.map(|r| &r.provenance);
    if meta.authors.iter().all(|a| is_blank(a))
        && let Some(author) = provenance.and_then(|p| p.author.as_deref()).filter(|a| !is_blank(a))
    {
        meta.authors = vec![author.to_owned()];
    }
    if let Some(credit) = provenance.and_then(|p| p.attribution.as_deref()).filter(|a| !is_blank(a)) {
        meta.terms.credit_notation = CreditNotation::Required;
        if meta.copyright_information.as_deref().is_none_or(is_blank) {
            meta.copyright_information = Some(credit.to_owned());
        }
    }
    scene.meta = meta;
    scene.provenance = provenance.cloned();
}

fn is_blank(text: &str) -> bool {
    text.trim().is_empty()
}

/// Writes `gathered.scene.avatar`: the VRM block for `built`, which `gathered` came from.
///
/// - **Humanoid:** every humanoid bone of the skeleton, with its node.
/// - **Expressions:** the 18 presets as empty placeholders. An expression that drives nothing is legal and does
///   nothing; the face baker replaces the entries it realises, by name.
/// - **Spring bones:** the rig's chains and colliders ([`springs::to_io`]), lengths scaled like the rest of the scene.
/// - **First person:** head and hair meshes are `thirdPersonOnly` (the wearer would see the inside of their own head),
///   every other mesh of a known part kind is `both`. A mesh of a kind nobody registered is left at `auto`.
/// - **Look-at:** bone gaze from the point between the eye bones, relative to the head bone. Without both eye bones
///   there is no look-at block, and the report says so.
///
/// # Errors
/// [`GatherError::NotHumanoid`] for a generic skeleton; [`GatherError::UnknownBone`] as in
/// [`skeleton::humanoid_nodes`].
pub fn add_avatar(gathered: &mut Gathered, built: &BuiltCharacter) -> Result<(), GatherError> {
    let rig = &built.rig;
    let mut avatar = Avatar::new();
    avatar.humanoid.bones.extend(skeleton::humanoid_nodes(&rig.skeleton)?);
    avatar.expressions =
        PresetExpression::ALL.into_iter().map(|preset| (ExpressionName::Preset(preset), Expression::new())).collect();
    avatar.spring_bones = springs::to_io(&rig.springs, gathered.scale);
    avatar.first_person = Some(first_person(built, &gathered.scene));
    avatar.look_at = look_at(&rig.skeleton, &gathered.scene.nodes);
    if avatar.look_at.is_none() {
        let msg = "the skeleton lacks leftEye or rightEye, so the file has no look-at block";
        let hint = "use a head part or body template with eye bones if viewers should follow the camera with the eyes";
        gathered.report.push(Finding::info("gather.look_at_missing", msg).at("avatar/lookAt").hint(hint));
    }
    gathered.scene.avatar = Some(avatar);
    Ok(())
}

/// One annotation per mesh node whose part kind is registered. Mesh nodes are named `<part>.<mesh>` and a part id is
/// a slug, so the text before the first dot is exactly the part.
fn first_person(built: &BuiltCharacter, scene: &IoScene) -> FirstPerson {
    let annotations = (0..)
        .map(NodeIx)
        .zip(&scene.nodes)
        .filter(|(_, node)| node.mesh.is_some())
        .filter_map(|(ix, node)| {
            let (part, _) = node.name.split_once('.')?;
            Some(MeshAnnotation { node: ix, flag: first_person_flag(built, part)? })
        })
        .collect();
    FirstPerson { annotations }
}

fn first_person_flag(built: &BuiltCharacter, part: &str) -> Option<FirstPersonFlag> {
    let kind = &built.def.parts.get(&PartId::new(part).ok()?)?.kind;
    let category = (find_kind(kind.as_str())?.meta)().category;
    Some(match category {
        PartCategory::Head | PartCategory::Hair => FirstPersonFlag::ThirdPersonOnly,
        PartCategory::Body | PartCategory::Garment | PartCategory::Footwear | PartCategory::Accessory => {
            FirstPersonFlag::Both
        }
    })
}

/// The range of a gaze movement the spec's own example uses (`lookAt.md`: 90 degrees of gaze move the eye bone 10
/// degrees); the spec states no default, and this is tuning, not rights.
const GAZE_RANGE: RangeMap = RangeMap { input_max_value: 90.0, output_scale: 10.0 };

/// Bone-driven gaze from the point between the eyes, in the head bone's frame.
///
/// The offset is measured in the **rest pose**, which is what the file's nodes hold (ADR 0014: rest is the VRM T-pose,
/// and the pose a mesh was sculpted in is carried by the inverse bind matrices alone). So it is built from the scene's
/// node transforms, already scaled by the gather, and never from inverse bind matrices; the skin's matrices have one
/// source, `skeleton::inverse_bind` (the rig's bind pose), used only by the scene half of the gather.
fn look_at(skeleton: &Skeleton, nodes: &[Node]) -> Option<LookAt> {
    let bone = |role| skeleton.humanoid_bone(role).map(BoneId::index);
    let (head, left, right) = (bone(HumanoidBone::Head)?, bone(HumanoidBone::LeftEye)?, bone(HumanoidBone::RightEye)?);
    let worlds = rest_worlds(skeleton, nodes);
    let position = |ix: usize| worlds.get(ix).map(|m| m.w_axis.truncate());
    let between_eyes = (position(left)? + position(right)?) * 0.5;
    Some(LookAt {
        kind: LookAtKind::Bone,
        offset_from_head_bone: worlds.get(head)?.inverse().transform_point3(between_eyes),
        horizontal_inner: GAZE_RANGE,
        horizontal_outer: GAZE_RANGE,
        vertical_down: GAZE_RANGE,
        vertical_up: GAZE_RANGE,
    })
}

/// The world matrix of every bone at rest, from the scene's node transforms. Bone `i` is node `i` and parents come
/// first, so one pass suffices.
fn rest_worlds(skeleton: &Skeleton, nodes: &[Node]) -> Vec<Mat4> {
    let mut worlds: Vec<Mat4> = Vec::with_capacity(skeleton.len());
    for (bone, node) in skeleton.bones().iter().zip(nodes) {
        let t = node.transform;
        let local = Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation);
        let parent = bone.parent.and_then(|p| worlds.get(p.index())).copied().unwrap_or(Mat4::IDENTITY);
        worlds.push(parent * local);
    }
    worlds
}
