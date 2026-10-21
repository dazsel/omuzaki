//! Contract for connectors (spec v3 §14, CN-01 to CN-05; `docs/design/connectors.md`, ADR 0030): artists bring their
//! own generators, services and programs, and Omuzaki calls them, shows what it will send and spend before it does,
//! keeps the results out of the project until a person accepts them, and records where every byte came from.
//!
//! ```text
//!  caller op (connect.generate, concept.sheet.generate, ...)      plan: tray::input_rights, then Connectors::preview
//!      │                                                          job:  Connectors::run, then tray::land
//!      ▼
//!  omuzaki-connect (the host: dyn Connectors) ── every register_connector! in the binary
//!      │  target once → preview → cap → seed → RunCx → Connector::run → provenance → Store::put → Candidate
//!      ▼
//!  adapter (omuzaki-connect-<family>): sees RunCx only (no store, no key, no document); HTTP is omuzaki-http-api's
//! ```
//!
//! | Module | What it fixes |
//! |---|---|
//! | [`id`] | [`ConnectorId`], [`CandidateId`], [`RunId`], the closed [`Capability`] set, [`MediaKind`] |
//! | [`manifest`] | [`ConnectorManifest`] (TOML) and its rules: transport, auth, cost, privacy, licence, MCP tool map |
//! | [`params`] | [`ParamDecl`]: adapter knobs a manifest declares and the host checks |
//! | [`request`] | one typed [`Request`] per capability, its checks and limits |
//! | [`output`] | [`Artifact`], [`Output`], [`Usage`], [`RunOutput`], [`ConnectError`] and its HTTP mapping |
//! | [`estimate`] | [`Estimate`] and [`estimate_from_manifest`], the [`Preview`], [`Preview::widens`], [`Status`] |
//! | [`config`] | the person's [`ConnectorConfig`] and the [`Target`] a run resolves once |
//! | [`connector`] | the [`Connector`] trait, [`RunCx`], [`RunEnv`], [`Meter`], [`Inputs`], [`Pace`] |
//! | [`registry`] | [`register_connector!`] and [`registered`] |
//! | [`candidate`], [`tray`] | the tray's records and document functions, [`InputRights`] |
//! | [`rights`] | the strictest-wins joins of commercial status and provenance (D6) |
//! | [`service`] | the [`Connectors`] service, [`Call`], [`RunResult`], [`PreviewError`], [`RunError`] |
//! | [`net`] | the endpoint and loopback rules |
//!
//! ## Conventions
//!
//! - **Everything is cold.** Connectors run on job workers; nothing here is on a real-time path.
//! - **Money is integer micro-dollars**, so sums are exact.
//! - **Struct-like variants are `#[non_exhaustive]` with a constructor**, so a field can be added without breaking
//!   implementers; match them with `..`.
//! - **Keys never appear in a value of this crate.** [`SecretLabel`] names a key; no type holds one.
//!
//! `omuzaki-connect-mock` has a fake connector per capability, test doubles for the run context and the conformance
//! suite every connector passes.
#![deny(missing_docs)]

pub mod candidate;
pub mod config;
pub mod connector;
pub mod estimate;
pub mod id;
pub mod manifest;
pub mod net;
pub mod output;
pub mod params;
pub mod registry;
pub mod request;
pub mod rights;
pub mod service;
pub mod tray;

pub use candidate::{Candidate, CandidateState, Usable};
pub use config::{ConfigSource, ConnectorConfig, DeclaredModel, Target};
pub use connector::{Connector, InputAsset, Inputs, Meter, Pace, RunCx, RunEnv};
pub use estimate::{Estimate, Outbound, Preview, Status, Widening, estimate_from_manifest};
pub use id::{CandidateId, Capability, ConnectorId, MediaKind, RunId};
pub use manifest::{
    ArgSource, AuthSpec, CapabilityDecl, ConnectorManifest, CostModel, Deliver, LicencePolicy, ManifestError, McpSpec,
    McpTool, OutputsFrom, PrivacyNotes, Sends, Transport, Tri, Unit,
};
pub use output::{Artifact, ConnectError, Output, ReportedRights, RunOutput, Usage, usd};
pub use params::{ParamDecl, ParamKind, check_params};
pub use registry::{ConnectorRegistration, find, registered};
pub use request::{
    ChatMessage, DccRequest, ImageRequest, LlmRequest, LoraUse, MAX_COUNT, MAX_LORA_WEIGHT, MAX_MESSAGES,
    MAX_MODEL3D_IMAGES, MAX_PROMPT_BYTES, MAX_REFERENCES, MAX_SECONDS, MAX_SIDE, MAX_TOKENS, MAX_VISION_IMAGES,
    Model3dRequest, MotionRequest, MotionSource, MusicKind, MusicRequest, Quality, Request, RequestProblem, Role,
    TextureRequest, VisionRequest, VoiceChoice, VoiceRequest,
};
pub use rights::{join_commercial, join_provenance};
pub use service::{Call, Connectors, PreviewError, RunError, RunResult};
pub use tray::{InputRights, TrayError};

/// The label of a key in the OS keychain (`connector/<id>`), never the key: the settings contract's validated
/// label, so connectors, settings and the keychain name keys one way.
pub type SecretLabel = omuzaki_settings_api::secret::SecretRef;

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
