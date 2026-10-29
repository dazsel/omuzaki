//! Contract for the scene (spec v3 §7, §24; ADR 0024; design: `docs/design/scenes.md`).
//!
//! `omuzaki-stage-api` is an API crate (§39 rule 1): data types and pure functions, small, stable, documented. The ops,
//! the rig library, landmarks, framing, resolve and apply live in `omuzaki-stage`. It holds:
//! - [`ids`]: [`SceneId`], [`MemberId`], [`LightId`], [`CameraId`], [`PropId`]
//! - [`scene`]: [`SceneDef`] (read and written like `CharacterDef`), its document [`paths`], its limits
//! - [`settings`]: [`SceneSettings`], [`Backdrop`], [`Ground`], [`PostDef`]
//! - [`lighting`]: [`Lighting`], [`LightDef`], [`Ambient`], [`FogDef`], [`EnvironmentDef`]
//! - [`camera`], [`cast`]: [`CameraDef`], [`Framing`], [`CastMember`]
//! - [`drive`]: which drivers move a member's channels ([`DriveMap`], `docs/design/drivers.md`)
//! - [`anchor`]: the [`Anchor`] a light or camera is measured against
//! - [`check`]: [`SceneDef::check`], the one validator (design D13), with [`SceneError`] and [`Finding`]
//! - [`effects`]: effect presets, the scene's [`WeatherDef`], live bursts and weather, and the [`EffectService`]
//!   (particles D9 to D14)
//!
//! Phase 2 fields are additive (design D14): spot lights, fog, the environment and its backdrop are here; typed props
//! and the other backdrop kinds arrive with their own tasks.
#![deny(missing_docs)]

omuzaki_core::define_link! {
    omuzaki_char_api::link();
}

pub mod anchor;
pub mod camera;
pub mod cast;
pub mod check;
pub mod drive;
pub mod effects;
pub mod error;
pub mod ids;
pub mod lighting;
pub mod resolve;
pub mod scene;
pub mod settings;

pub use anchor::Anchor;
pub use camera::{CameraDef, Framing};
pub use cast::{CastMember, Interact, InteractKind};
pub use drive::{DriveGroup, DriveMap, DriverKind};
pub use effects::{
    BurstAccepted, BurstRequest, ChatEmoteRef, EffectAt, EffectError, EffectEvent, EffectEventKind, EffectKind,
    EffectOrigin, EffectPreset, EffectService, EffectStatus, LiveWeather, PresetInfo, SpriteDef, WeatherArea,
    WeatherDef,
};
pub use error::{Finding, FindingKind, Findings, SceneError};
pub use ids::{CameraId, LightId, MemberId, PropId, SceneId};
pub use lighting::{
    Ambient, BloomDef, EnvironmentDef, EnvironmentSource, FogDef, LightDef, LightRole, LightShape, Lighting,
};
pub use scene::{MAX_CAMERAS, MAX_CAST, MAX_LIGHTS, MAX_PROPS, REFERENCE_HEIGHT_M, SceneDef, paths};
pub use settings::{
    Backdrop, BackdropFit, Bars, DepthOfFieldDef, Focus, FocusSubject, GradeDef, Ground, GroundKind, PostDef,
    SceneSettings,
};
