//! The connector host (spec v3 §14, CN-01 to CN-05; `docs/design/connectors.md`, ADR 0030): Omuzaki calls the
//! generators, services and programs an artist brings, shows what a call will send and spend before it does, keeps the
//! results out of the project until a person accepts them, and records where every byte came from.
//!
//! ```text
//!  connect.* ops (and any crate's op, through dyn Connectors)
//!      │ plan: tray::input_rights, then Host::preview (pure: no connector, no transport)
//!      │ job:  Host::run, then tray::land in finish
//!      ▼
//!  Host ── ConnectorSet (every register_connector! in the binary, validated once)
//!      │  preview: choose → check request and params → enabled, target, key → inputs (private_test, consent)
//!      │           → outbound → estimate → rights → needs_person
//!      │  run:     preview again → widens? → expect → cap → seed → key → AuthedHttp + StoreInputs + RunCx
//!      │           → Connector::run → provenance per output → Store::put → pending Candidate; text and JSON → answers
//!      ▼
//!  adapter: sees RunCx only (no store, no keychain, no document); every string it returns is scrubbed of the key
//! ```
//!
//! | Module | What it holds |
//! |---|---|
//! | [`set`] | [`ConnectorSet`]: the registered connectors, each manifest parsed and validated once |
//! | [`status`] | [`status`](status::status): disabled, unconfigured, needs a key, needs a plan, or ready |
//! | [`rights`] | the provenance the host writes for each output, and the commercial join (D6) |
//! | [`auth`] | [`AuthedHttp`]: the key attached by the host, only to the target's hosts, and [`scrub_str`] |
//! | [`host`] | [`Host`] and [`HostParts`], what an App wires; [`ThreadPace`] |
//! | `preview`, `run`, `service` | the preview, the run and `impl Connectors for Host` |
//! | [`settings`] | the `connectors.entries` setting and [`SettingsConfig`] |
//! | [`ops`] | `connect.list`, `describe`, `key_status`, `preview`, `generate`, `test`, `tray_list`, `accept`, `reject`, `regenerate` |
//!
//! Where the issues' proposed signatures and the accepted design differ, the design wins: the setting key is
//! `connectors.entries`, a plan tier outside `requires_plan` narrows the outputs to non-commercial with a warning
//! rather than refusing the call, and a plain `http://` endpoint on another computer is allowed (flagged, never
//! carrying a key).
#![deny(missing_docs)]

pub mod auth;
pub mod host;
pub mod ops;
mod preview;
pub mod rights;
mod run;
mod service;
pub mod set;
pub mod settings;
pub mod status;

pub use auth::{AuthedHttp, scrub_str};
pub use host::{Host, HostParts, KeyStatus, ThreadPace};
pub use set::{ConnectorSet, Entry, SetError};
pub use settings::{ConnectorSettings, SettingsConfig};

omuzaki_core::define_link!();
