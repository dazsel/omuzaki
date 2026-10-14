//! The avatar layer of a VRM: expressions, gaze, first-person flags, spring bones and node constraints.
//!
//! These types follow VRM 1.0 (`VRMC_vrm`, `VRMC_springBone`, `VRMC_node_constraint`); the 0.x importer converts into
//! them and the 0.x exporter converts back, warning where 0.x cannot say the same thing (per-joint spring
//! parameters, node constraints, `surprised`). The humanoid is in [`super::humanoid`], the meta in [`crate::meta`].

use std::collections::BTreeMap;
use std::fmt;

use omuzaki_core::glam::{Vec2, Vec3, Vec4};
use serde::{Deserialize, Serialize};

use super::{Humanoid, MaterialIx, NodeIx};

/// The 18 preset expressions of VRM 1.0 (`VRMC_vrm-1.0/expressions.md`, "Preset Expressions").
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PresetExpression {
    /// Emotion; 0.x `Joy`.
    Happy,
    /// Emotion.
    Angry,
    /// Emotion; 0.x `Sorrow`.
    Sad,
    /// Emotion; 0.x `Fun`.
    Relaxed,
    /// Emotion; new in 1.0, so 0.x has no preset for it.
    Surprised,
    /// Lip sync "aa"; 0.x `A`.
    Aa,
    /// Lip sync "ih"; 0.x `I`.
    Ih,
    /// Lip sync "ou"; 0.x `U`.
    Ou,
    /// Lip sync "ee"; 0.x `E`.
    Ee,
    /// Lip sync "oh"; 0.x `O`.
    Oh,
    /// Both eyelids closed.
    Blink,
    /// Left eyelid closed; 0.x `Blink_L`.
    BlinkLeft,
    /// Right eyelid closed; 0.x `Blink_R`.
    BlinkRight,
    /// Gaze up, for models whose eyes move by expression.
    LookUp,
    /// Gaze down.
    LookDown,
    /// Gaze left.
    LookLeft,
    /// Gaze right.
    LookRight,
    /// Kept "for backwards compatibility" (the spec's words).
    Neutral,
}

impl PresetExpression {
    /// All presets, in the spec's listing order.
    pub const ALL: [Self; 18] = [
        Self::Happy,
        Self::Angry,
        Self::Sad,
        Self::Relaxed,
        Self::Surprised,
        Self::Aa,
        Self::Ih,
        Self::Ou,
        Self::Ee,
        Self::Oh,
        Self::Blink,
        Self::BlinkLeft,
        Self::BlinkRight,
        Self::LookUp,
        Self::LookDown,
        Self::LookLeft,
        Self::LookRight,
        Self::Neutral,
    ];

    /// The VRM 1.0 name, the key under `expressions.preset`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Happy => "happy",
            Self::Angry => "angry",
            Self::Sad => "sad",
            Self::Relaxed => "relaxed",
            Self::Surprised => "surprised",
            Self::Aa => "aa",
            Self::Ih => "ih",
            Self::Ou => "ou",
            Self::Ee => "ee",
            Self::Oh => "oh",
            Self::Blink => "blink",
            Self::BlinkLeft => "blinkLeft",
            Self::BlinkRight => "blinkRight",
            Self::LookUp => "lookUp",
            Self::LookDown => "lookDown",
            Self::LookLeft => "lookLeft",
            Self::LookRight => "lookRight",
            Self::Neutral => "neutral",
        }
    }

    /// Parses a VRM 1.0 preset name (case-sensitive).
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.as_str() == name)
    }

    /// The VRM 0.x `presetName` (`blendShapeMaster.blendShapeGroups[*].presetName`), or `None` where 0.x has no
    /// preset (`surprised`). The mapping is spec text: "happy: changed from `joy`", "sad: from `sorrow`",
    /// "relaxed: from `fun`", and the vowels `A I U E O`.
    pub const fn vrm0_preset(self) -> Option<&'static str> {
        Some(match self {
            Self::Happy => "Joy",
            Self::Angry => "Angry",
            Self::Sad => "Sorrow",
            Self::Relaxed => "Fun",
            Self::Surprised => return None,
            Self::Aa => "A",
            Self::Ih => "I",
            Self::Ou => "U",
            Self::Ee => "E",
            Self::Oh => "O",
            Self::Blink => "Blink",
            Self::BlinkLeft => "Blink_L",
            Self::BlinkRight => "Blink_R",
            Self::LookUp => "LookUp",
            Self::LookDown => "LookDown",
            Self::LookLeft => "LookLeft",
            Self::LookRight => "LookRight",
            Self::Neutral => "Neutral",
        })
    }

    /// Parses a 0.x `presetName`. 0.x ids are compared upper-cased ("`GetID`" in the 0.x spec), so this is
    /// case-insensitive. `Unknown` (a custom clip) returns `None`.
    pub fn from_vrm0_preset(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.vrm0_preset().is_some_and(|v| v.eq_ignore_ascii_case(name)))
    }

    /// Lip sync, blink and gaze presets are *procedural*: an app drives them automatically, and other expressions
    /// can suppress them through [`Override`]. Emotions and `neutral` are not.
    pub const fn is_procedural(self) -> bool {
        !matches!(self, Self::Happy | Self::Angry | Self::Sad | Self::Relaxed | Self::Surprised | Self::Neutral)
    }
}

/// Why a string can't be a [`CustomName`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CustomNameError {
    /// Empty names can't be map keys in a file.
    #[error("an expression name can't be empty")]
    Empty,
    /// "Custom Expressions cannot have names that are the same as any Preset Expressions" (`expressions.md`).
    #[error("{0:?} is a preset expression, not a custom one")]
    Preset(String),
}

/// The name of a custom expression: any non-empty string that is not a preset name. Perfect-sync channels are custom
/// expressions named exactly like `ARKit`'s blendshapes (`jawOpen`, `eyeBlinkLeft`), which never collide with presets.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CustomName(String);

impl CustomName {
    /// Validates `name`.
    ///
    /// # Errors
    /// [`CustomNameError`] if it is empty or equals a preset name.
    pub fn new(name: impl Into<String>) -> Result<Self, CustomNameError> {
        let name = name.into();
        if name.is_empty() {
            return Err(CustomNameError::Empty);
        }
        if PresetExpression::parse(&name).is_some() {
            return Err(CustomNameError::Preset(name));
        }
        Ok(Self(name))
    }

    /// The name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for CustomName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CustomName({:?})", self.0)
    }
}

/// An expression's name: one of the presets, or a custom name. Ordered presets first (in spec order), then custom
/// names alphabetically, so a scene's expression map iterates, and exports, deterministically.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum ExpressionName {
    /// A preset.
    Preset(PresetExpression),
    /// A custom expression.
    Custom(CustomName),
}

impl ExpressionName {
    /// A preset if `name` is exactly a preset name, otherwise a custom name.
    ///
    /// # Errors
    /// [`CustomNameError::Empty`] for an empty string.
    pub fn parse(name: &str) -> Result<Self, CustomNameError> {
        match PresetExpression::parse(name) {
            Some(p) => Ok(Self::Preset(p)),
            None => CustomName::new(name).map(Self::Custom),
        }
    }

    /// The name as written in the file.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Preset(p) => p.as_str(),
            Self::Custom(c) => c.as_str(),
        }
    }
}

impl TryFrom<String> for ExpressionName {
    type Error = CustomNameError;
    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::parse(&s)
    }
}

impl From<ExpressionName> for String {
    fn from(n: ExpressionName) -> Self {
        n.as_str().to_owned()
    }
}

impl fmt::Display for ExpressionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How an active expression affects the procedural expressions of one kind (`overrideBlink` and friends).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum Override {
    /// No effect.
    #[default]
    None,
    /// Set the target weights to 0 while this expression's weight is above 0.
    Block,
    /// Attenuate the target weights linearly by this expression's weight.
    Blend,
}

/// Drives a morph target of a mesh node. In 1.0 the bind names the *node* that holds the mesh; 0.x names the mesh
/// itself, and the 0.x importer resolves it to the node that uses that mesh.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MorphBind {
    /// The node with the mesh.
    pub node: NodeIx,
    /// The morph target index, the same on every primitive of that mesh.
    pub index: u32,
    /// The target's weight at expression value 1, in 0..=1 (0.x stores 0..=100; importers divide by 100).
    pub weight: f32,
}

/// Which material colour a [`ColorBind`] moves (`expressions.md`, "`MaterialColorBind`").
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ColorBindKind {
    /// `pbrMetallicRoughness.baseColorFactor`.
    Color,
    /// `emissiveFactor`.
    EmissionColor,
    /// `MToon` `shadeColorFactor`.
    ShadeColor,
    /// `MToon` `matcapFactor`.
    MatcapColor,
    /// `MToon` `parametricRimColorFactor`.
    RimColor,
    /// `MToon` `outlineColorFactor`.
    OutlineColor,
}

/// Blends a material colour towards `target` as the expression rises: `base + (target - base) * weight`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ColorBind {
    /// The material.
    pub material: MaterialIx,
    /// Which colour.
    pub kind: ColorBindKind,
    /// The colour at weight 1 (linear RGBA; the alpha is ignored for colours that have none).
    pub target: Vec4,
}

/// Moves the UV scale and offset of every UV-addressed texture of a material: how painted eyes and mouths switch
/// frames in an atlas. The value interpolates linearly with the expression weight, so a flipbook of discrete frames
/// pairs this with [`Expression::is_binary`].
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct UvBind {
    /// The material.
    pub material: MaterialIx,
    /// Scale at weight 1.
    pub scale: Vec2,
    /// Offset at weight 1.
    pub offset: Vec2,
}

/// An expression: a named bundle of morph, colour and UV binds.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Expression {
    /// Values above 0.5 count as 1 and the rest as 0.
    pub is_binary: bool,
    /// Morph targets driven. The file format requires at least one entry when the array is present, so exporters
    /// omit an empty list rather than writing it.
    pub morph_binds: Vec<MorphBind>,
    /// Material colours driven.
    pub color_binds: Vec<ColorBind>,
    /// Texture transforms driven.
    pub uv_binds: Vec<UvBind>,
    /// Effect on the blink presets.
    pub override_blink: Override,
    /// Effect on the gaze presets.
    pub override_look_at: Override,
    /// Effect on the lip-sync presets.
    pub override_mouth: Override,
}

impl Expression {
    /// An expression that drives nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// An expression that drives one morph target at full weight: the shape of every perfect-sync channel.
    pub fn morph(node: NodeIx, index: u32) -> Self {
        Self { morph_binds: vec![MorphBind { node, index, weight: 1.0 }], ..Self::default() }
    }

    /// True if it has no binds at all (a legal but inert expression).
    pub fn is_inert(&self) -> bool {
        self.morph_binds.is_empty() && self.color_binds.is_empty() && self.uv_binds.is_empty()
    }
}

/// How gaze is applied (`lookAt.md`, "`LookAtType`").
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum LookAtKind {
    /// Rotate the `leftEye` and `rightEye` bones.
    #[default]
    Bone,
    /// Weight the `lookUp`/`lookDown`/`lookLeft`/`lookRight` expressions.
    Expression,
}

/// Maps a gaze angle to a bone rotation or expression weight (`lookAt.md`, "`RangeMap`").
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RangeMap {
    /// The input angle, in degrees (0..=180), at which the output saturates.
    pub input_max_value: f32,
    /// The output at that angle: degrees for bones, weight for expressions.
    pub output_scale: f32,
}

/// Gaze control (`VRMC_vrm.lookAt`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct LookAt {
    /// Bone or expression.
    pub kind: LookAtKind,
    /// The gaze origin relative to the head bone (between the eyes), in the head's rest frame. 0.x
    /// `firstPersonBoneOffset` maps here.
    pub offset_from_head_bone: Vec3,
    /// Inward horizontal range.
    pub horizontal_inner: RangeMap,
    /// Outward horizontal range.
    pub horizontal_outer: RangeMap,
    /// Downward range.
    pub vertical_down: RangeMap,
    /// Upward range.
    pub vertical_up: RangeMap,
}

/// Per-mesh first-person rendering (`firstPerson.md`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum FirstPersonFlag {
    /// Split by head-bone weights: the head part is third-person only, the rest both. The default when a mesh has
    /// no annotation, and when the whole `firstPerson` block is absent.
    #[default]
    Auto,
    /// Visible in both views.
    Both,
    /// Hidden from the wearer's own view (head, hair, hats).
    ThirdPersonOnly,
    /// Visible only to the wearer.
    FirstPersonOnly,
}

/// A first-person flag for the mesh on `node`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct MeshAnnotation {
    /// The node with the mesh.
    pub node: NodeIx,
    /// The flag.
    pub flag: FirstPersonFlag,
}

/// First-person settings (`VRMC_vrm.firstPerson`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FirstPerson {
    /// Per-mesh flags. Unlisted meshes are [`FirstPersonFlag::Auto`].
    pub annotations: Vec<MeshAnnotation>,
}

/// A spring-bone collision shape in the local space of its node (`VRMC_springBone`, "colliders"). The inside-out and
/// plane shapes of `VRMC_springBone_extended_collider` are not modelled; importers report them as dropped.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ColliderShape {
    /// A sphere.
    Sphere {
        /// Centre.
        offset: Vec3,
        /// Radius, metres.
        radius: f32,
    },
    /// A capsule from `offset` to `tail`.
    Capsule {
        /// Centre of the start cap.
        offset: Vec3,
        /// Radius, metres.
        radius: f32,
        /// Centre of the end cap.
        tail: Vec3,
    },
}

/// A collider attached to a node.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Collider {
    /// The node it moves with.
    pub node: NodeIx,
    /// Its shape.
    pub shape: ColliderShape,
}

/// A named set of colliders that chains can collide with. `colliders` index [`SpringBones::colliders`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ColliderGroup {
    /// Display name.
    pub name: String,
    /// Indices into [`SpringBones::colliders`].
    pub colliders: Vec<u32>,
}

/// One joint of a spring chain, with the 1.0 parameter defaults (`VRMC_springBone.joint.schema.json`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct SpringJoint {
    /// The node that swings.
    pub node: NodeIx,
    /// Collision radius of the joint's sphere, metres.
    pub hit_radius: f32,
    /// Force pulling back to the rest pose.
    pub stiffness: f32,
    /// Gravitational acceleration.
    pub gravity_power: f32,
    /// Gravity direction.
    pub gravity_dir: Vec3,
    /// Air resistance, 0..=1.
    pub drag_force: f32,
}

impl SpringJoint {
    /// A joint with the spec defaults: radius 0, stiffness 1, no gravity (pointing down), drag 0.5.
    pub fn new(node: NodeIx) -> Self {
        Self { node, hit_radius: 0.0, stiffness: 1.0, gravity_power: 0.0, gravity_dir: Vec3::NEG_Y, drag_force: 0.5 }
    }
}

/// A chain of joints, each a descendant of the one before. The last joint is only a tail: its own parameters are not
/// used, so a chain that should swing its last bone needs an extra (possibly empty) node after it. (0.x adds an
/// implicit tail 7 cm past the end, which the 0.x importer makes explicit.)
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpringChain {
    /// Display name.
    pub name: String,
    /// The joints, root first.
    pub joints: Vec<SpringJoint>,
    /// Indices into [`SpringBones::collider_groups`] this chain collides with.
    pub collider_groups: Vec<u32>,
    /// Evaluate in the space of this node instead of world space, so a walking model's hair doesn't whip. It must be
    /// the first joint or one of its ancestors.
    pub center: Option<NodeIx>,
}

/// All spring-bone data (`VRMC_springBone`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpringBones {
    /// Every collider.
    pub colliders: Vec<Collider>,
    /// Groups of colliders.
    pub collider_groups: Vec<ColliderGroup>,
    /// The chains. No joint belongs to two chains.
    pub chains: Vec<SpringChain>,
}

/// Which local axis a roll constraint copies.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RollAxis {
    /// X.
    X,
    /// Y.
    Y,
    /// Z.
    Z,
}

/// Which local axis an aim constraint points along the source.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AimAxis {
    /// +X.
    PositiveX,
    /// −X.
    NegativeX,
    /// +Y.
    PositiveY,
    /// −Y.
    NegativeY,
    /// +Z.
    PositiveZ,
    /// −Z.
    NegativeZ,
}

/// What a constraint does (`VRMC_node_constraint`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ConstraintKind {
    /// Copies the source's twist about one axis (forearm twist bones).
    Roll {
        /// The driving node.
        source: NodeIx,
        /// The axis.
        axis: RollAxis,
    },
    /// Points one axis at the source.
    Aim {
        /// The driving node.
        source: NodeIx,
        /// The axis.
        axis: AimAxis,
    },
    /// Copies the source's full rotation.
    Rotation {
        /// The driving node.
        source: NodeIx,
    },
}

/// A constraint on a node. 0.x has no equivalent, so a 0.x export drops these with a warning.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct NodeConstraint {
    /// The constrained node.
    pub node: NodeIx,
    /// The constraint.
    pub kind: ConstraintKind,
    /// Blend from the rest rotation (0) to the constrained rotation (1).
    pub weight: f32,
}

/// Everything VRM adds to a glTF to make it an avatar.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct Avatar {
    /// Which node plays which bone.
    pub humanoid: Humanoid,
    /// Expressions by name, in deterministic order.
    pub expressions: BTreeMap<ExpressionName, Expression>,
    /// Gaze control.
    pub look_at: Option<LookAt>,
    /// First-person flags.
    pub first_person: Option<FirstPerson>,
    /// Spring bones and colliders.
    pub spring_bones: SpringBones,
    /// Node constraints.
    pub constraints: Vec<NodeConstraint>,
}

impl Avatar {
    /// An avatar with nothing assigned yet.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_names_round_trip_and_map_to_vrm0() {
        for p in PresetExpression::ALL {
            assert_eq!(PresetExpression::parse(p.as_str()), Some(p));
            if let Some(v0) = p.vrm0_preset() {
                assert_eq!(PresetExpression::from_vrm0_preset(v0), Some(p), "{v0}");
            }
        }
        // The spec's renames: happy <- Joy, sad <- Sorrow, relaxed <- Fun, vowels A I U E O.
        assert_eq!(PresetExpression::from_vrm0_preset("joy"), Some(PresetExpression::Happy));
        assert_eq!(PresetExpression::from_vrm0_preset("Sorrow"), Some(PresetExpression::Sad));
        assert_eq!(PresetExpression::from_vrm0_preset("FUN"), Some(PresetExpression::Relaxed));
        assert_eq!(PresetExpression::from_vrm0_preset("Blink_L"), Some(PresetExpression::BlinkLeft));
        assert_eq!(PresetExpression::from_vrm0_preset("Unknown"), None);
        assert_eq!(PresetExpression::Surprised.vrm0_preset(), None);
        assert_eq!(PresetExpression::ALL.len(), 18);
    }

    #[test]
    fn only_lip_sync_blink_and_gaze_are_procedural() {
        let procedural: Vec<_> = PresetExpression::ALL.into_iter().filter(|p| p.is_procedural()).collect();
        assert_eq!(procedural.len(), 12, "5 vowels + 3 blink + 4 look");
        assert!(!PresetExpression::Neutral.is_procedural() && !PresetExpression::Happy.is_procedural());
    }

    #[test]
    fn custom_names_cannot_shadow_presets_or_be_empty() {
        assert_eq!(CustomName::new(""), Err(CustomNameError::Empty));
        assert_eq!(CustomName::new("happy"), Err(CustomNameError::Preset("happy".into())));
        assert!(CustomName::new("jawOpen").is_ok());
        assert!(CustomName::new("Happy").is_ok(), "names are compared exactly, as in the spec");
    }

    #[test]
    fn expression_names_parse_order_and_serialise() {
        let names = ["zzz", "blink", "aa", "jawOpen"].map(|n| ExpressionName::parse(n).unwrap());
        let mut sorted = names.to_vec();
        sorted.sort();
        let order: Vec<_> = sorted.iter().map(ExpressionName::as_str).collect();
        assert_eq!(order, ["aa", "blink", "jawOpen", "zzz"], "presets in spec order, then custom names");
        assert_eq!(serde_json::to_string(&names[1]).unwrap(), "\"blink\"");
        assert!(serde_json::from_str::<ExpressionName>("\"\"").is_err());
    }

    #[test]
    fn spring_joint_defaults_match_the_schema() {
        let j = SpringJoint::new(NodeIx(0));
        assert_eq!((j.hit_radius, j.stiffness, j.gravity_power, j.drag_force), (0.0, 1.0, 0.0, 0.5));
        assert_eq!(j.gravity_dir, Vec3::NEG_Y);
    }

    #[test]
    fn expression_helpers() {
        assert!(Expression::new().is_inert());
        let e = Expression::morph(NodeIx(2), 5);
        assert!(!e.is_inert());
        assert_eq!(e.morph_binds[0], MorphBind { node: NodeIx(2), index: 5, weight: 1.0 });
    }
}
