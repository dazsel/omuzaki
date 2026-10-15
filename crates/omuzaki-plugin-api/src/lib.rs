//! Contract for WASM plugins (PL-01, ADR 0058, `docs/design/plugins.md`).
//!
//! This crate holds the contract only: no wasmtime and no I/O. The pieces:
//!
//! Callers import by module path (ADR 0054 §2): `omuzaki_plugin_api::manifest::Manifest`. Only the id, the error
//! and the service trait are at the root.
//!
//! - **[`WIT_PACKAGE`] and [`WIT`]**: the WIT package `omuzaki:plugin`, the base world every plugin targets.
//! - **[`manifest`]**: `plugin.toml`, its parser and its checks.
//! - **[`summary`]**: the permission card a person reads before turning a plugin on, and what an update asks for
//!   beyond what was approved.
//! - **[`grants`]**: what the person approved, and the grants the plugin's session runs under.
//! - **[`limits`]**: the sandbox's numbers and each call kind's fuel and deadline budgets, shared by the host and the
//!   mock.
//! - **[`state`]**: what a plugin is, what state it is in and what it said; [`PluginId`] names one.
//! - **[`PluginError`]**: why a request about a plugin failed; it converts to [`OpError`](omuzaki_ops_api::OpError).
//! - **[`Plugins`]**: the service the `plugin.*` ops and the plugin manager call. The host implements it, the mock
//!   fakes it.
#![deny(missing_docs)]

pub mod error;
pub mod grants;
pub mod limits;
pub mod manifest;
pub mod service;
pub mod state;
pub mod summary;

pub use error::PluginError;
pub use service::Plugins;
pub use state::PluginId;

omuzaki_core::define_link!();

/// The WIT package name and version. A component's imports name the version it was built against.
pub const WIT_PACKAGE: &str = "omuzaki:plugin@0.1.0";

/// The text of `wit/omuzaki-plugin.wit`, the base world `plugin`, for the host to parse.
pub const WIT: &str = include_str!("../wit/omuzaki-plugin.wit");

#[cfg(test)]
mod tests {
    /// A compile-time list of the public names other crates build on. Removing or renaming any of them breaks it.
    #[test]
    fn the_public_api_snapshot() {
        #[allow(unused_imports)]
        use crate::grants::{Approval, approved_hosts, ceiling, needs_person_each_use};
        #[allow(unused_imports)]
        use crate::limits::{Budget, CallKind};
        #[allow(unused_imports)]
        use crate::manifest::{LicenceClass, Manifest, ManifestError, Publisher, Report, Requests, Severity};
        #[allow(unused_imports)]
        use crate::state::{
            DisabledReason, FaultKind, FaultRecord, LogLevel, LogLine, PluginIdError, PluginInfo, PluginState,
        };
        #[allow(unused_imports)]
        use crate::summary::{
            DataLine, PER_USE, PermLine, PermissionDiff, PermissionSummary, Risk, diff, risk, summarise,
        };
        #[allow(unused_imports)]
        use crate::{PluginError, PluginId, Plugins, WIT, WIT_PACKAGE};
        assert!(crate::WIT.contains(crate::WIT_PACKAGE.trim_start_matches("omuzaki:plugin@")));
    }
}
