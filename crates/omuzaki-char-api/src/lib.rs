//! Contract for the procedural character (spec v3 §18, §32; design: `docs/design/char-face-api.md`).
//!
//! `omuzaki-char-api` is an API crate (§39 rule 1): small, stable, documented. The generators live in `omuzaki-char`; this
//! crate defines what they plug into and what they produce. It holds:
//! - [`ids`]: typed ids ([`CharId`], [`PartId`]...) and [`JointRef`], how meshes name the bones they follow
//! - [`character`]: [`CharacterDef`], the part tree ([`PartDef`]) and the document paths ops edit
//! - [`kind`]: [`PartKind`], self-registration ([`register_part_kind!`]), what parts [`Publishes`] to each other, and
//!   [`PartKind::recolour`], how a palette change reaches a built part without rebuilding its geometry
//! - [`geometry`], [`mesh`]: meshes, joint lists, toon materials, textures: what a part gives the renderer
//! - [`runtime`]: [`PartRuntime`], the contract for hair and cloth simulations in the frame loop
//! - [`build`]: [`BuiltCharacter`], [`CharacterBuilder`], the reference [`RegistryBuilder`] and the pipeline it is made of
//!   ([`plan`], [`build_part`], [`recolour_part`], [`part_rng`], [`Assembler`]), which any other builder reuses
//! - [`services`]: [`BuildServices`], the asset reader and rasteriser the app injects into a build
//! - [`rig`], [`skin`]: the extra bones, spring chains and colliders parts contribute, the [`CharacterRig`] the builder
//!   assembles from them, and weight brushes
//! - [`look`]: the look node ([`LookDef`]: a preset plus overrides), self-registering presets
//!   ([`register_look_presets!`]) and the build-time material transform ([`BuiltCharacter::apply_look`])
//! - [`drivers`]: [`BoneDriver`]s, a custom channel or another bone posing an extra bone, and how they export
//! - [`stature`]: [`Stature`], real-size scaling of the body and head before the parts that simulate
//! - [`variant`]: per-use variants as path overrides, and automatic selection by shot
//! - [`head`], [`hair`], [`body`], [`shape`]: parameters of the core part kinds and face-shape profiles
//!
//! The face itself (channels, profile, painter) is in `omuzaki-face-api`, which this crate builds on.
#![deny(missing_docs)]

pub mod body;
pub mod build;
pub mod character;
pub mod doc_key;
pub mod drivers;
pub mod error;
pub mod fact;
pub mod geometry;
pub mod hair;
pub mod head;
pub mod ids;
pub mod keying;
pub mod kind;
pub mod look;
pub mod rig;
pub mod runtime;
pub mod services;
pub mod shape;
pub mod skin;
pub mod stature;
pub mod variant;

omuzaki_core::define_link!();

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;

pub use body::{BodyParams, BodyShape, HandParams, HandStyle, Proportions};
pub use build::{
    Assembler, BuildOptions, BuildReport, BuiltCharacter, BuiltMesh, CharacterBuilder, FaceBinding, PartReport,
    RegistryBuilder, RuntimeSlot, build_part, part_rng, plan, recolour_part,
};
pub use character::{Attach, AttachOffset, CharacterDef, PartDef, TreeError, paths};
pub use doc_key::{DocKeyError, DocKeyRegistration};
pub use drivers::{
    BakedDriver, BoneDriver, Drive, DriverError, DriverExport, DriverPlan, Mapping, NodeConstraint, RollAxis,
    export_plan,
};
pub use error::BuildError;
pub use fact::{Agree, Append, Fact, FactRegistration, Keyed, Last, Rule, facts};
pub use geometry::{FaceOverlay, MaterialError, MaterialSpec, PartMesh, TextureData, face_texture_desc};
pub use hair::{
    BangLayer, BangsGen, BraidGen, Bulge, BunGen, CapGen, CurtainGen, Dynamics, Falloff, Follow, FramingGen, Generator,
    GuideClump, GuidePoint, GuidesGen, HAIR_INFO, HAIR_PROGRAM, HAIR_UV, HairClump, HairGroup, HairParams, HairPhysics,
    HairRng, HairShading, HairShadingError, HairSide, LegacyLcg, PartSweepGen, SphereCollider, Streak, StreakBlend,
    TailGen, Taper, TaperShape, Wave, Wisp,
};
pub use head::{Chin, Cranium, EarKind, Ears, FaceWindow, HeadOutline, HeadParams, Neck, SkinColours, TracedHead};
pub use ids::{
    AnchorId, BodyShapeId, BodyTemplateId, BoneName, CharId, ColourwayId, GroupId, JointRef, LookPresetId,
    MaterialName, PartId, PartKindId, ProfileId, SkinToneId, TextureName, VariantId,
};
pub use kind::{
    Anchor, AnchorsFact, BodyFit, BodyRegion, BuildCtx, CollidersFact, FaceFact, FitFact, Fits, HeadFact, HeadSurface,
    KindProvenance, PartCategory, PartKind, PartKindRegistration, PartMeta, PartOutput, Published, Publishes, Quality,
    RecolourCtx, Recoloured, RegionEnd, Section, StatureFact, find_kind, kinds, requires_of,
};
pub use look::{
    ClassLook, DEFAULT_PRESET, DERIVED_LINE, GradeOverride, LookDef, LookError, LookParams, LookPreset,
    LookPresetSource, MaterialClass, OutlineColour, ProgramLook, SceneLook, find_look_preset, look_presets,
};
pub use omuzaki_core::mesh::{ExtraAttribute, MeshData, MeshError};
pub use omuzaki_store_api::ContentHash;
pub use rig::{
    BindError, CharacterRig, ExtraBone, JointFit, RigContribution, extra_id, humanoid_id, humanoid_of, rest_skeleton,
};
pub use runtime::{DynamicMesh, PartRuntime, PoseSource, RuntimeCost, StepCtx};
pub use services::{AssetError, AssetSource, BuildServices, RasterizerFactory};
pub use shape::{PRESET_IDS, ShapeProfile};
pub use skin::{BrushFalloff, WeightBrush, apply_brushes, influence_cut};
pub use stature::{Stature, scale_output};
pub use variant::{
    HeadCoverage, SHOT_HYSTERESIS, ShotRange, VariantDef, VariantError, VariantSet, resolve_variant, select_variant,
};
