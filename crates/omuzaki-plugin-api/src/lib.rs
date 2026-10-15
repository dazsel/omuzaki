//! Contract for WASM plugins (PL-01, ADR 0058, `docs/design/plugins.md`).
//!
//! This crate holds the contract only: no wasmtime and no I/O. The pieces:
//!
//! - **[`WIT_PACKAGE`] and [`WIT`]**: the WIT package `omuzaki:plugin`, the base world every plugin targets.
//! - **[`Manifest`]**: `plugin.toml`, its parser and its checks ([`Report`], [`LicenceClass`]).
//! - **[`summarise`] and [`diff`]**: the permission card a person reads before turning a plugin on, and what an update
//!   asks for beyond what was approved.
//! - **[`Approval`] and [`ceiling`]**: what the person approved, and the grants the plugin's session runs under.
//! - **[`limits`]**: the sandbox's numbers and [`CallKind`]'s fuel and deadline [`Budget`]s, shared by the host and
//!   the mock.
//! - **[`PluginId`], [`PluginInfo`], [`PluginState`], [`FaultKind`], [`LogLine`]**: what a plugin is, what state it
//!   is in and what it said.
//! - **[`PluginError`]**: why a request about a plugin failed; it converts to [`OpError`](omuzaki_ops_api::OpError).
//! - **[`Plugins`]**: the service the `plugin.*` ops and the plugin manager call. The host implements it, the mock
//!   fakes it.
#![deny(missing_docs)]

mod error;
mod grants;
pub mod limits;
pub mod manifest;
mod service;
mod state;
mod summary;

pub use error::PluginError;
pub use grants::{Approval, approved_hosts, ceiling, needs_person_each_use};
pub use limits::{Budget, CallKind};
pub use manifest::{LicenceClass, Manifest, ManifestError, Publisher, Report, Requests, Severity};
pub use service::Plugins;
pub use state::{
    DisabledReason, FaultKind, FaultRecord, LogLevel, LogLine, PluginId, PluginIdError, PluginInfo, PluginState,
};
pub use summary::{DataLine, PER_USE, PermLine, PermissionDiff, PermissionSummary, Risk, diff, risk, summarise};

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
        use crate::{
            Approval, Budget, CallKind, DataLine, DisabledReason, FaultKind, FaultRecord, LicenceClass, LogLevel,
            LogLine, Manifest, ManifestError, PER_USE, PermLine, PermissionDiff, PermissionSummary, PluginError,
            PluginId, PluginIdError, PluginInfo, PluginState, Plugins, Publisher, Report, Requests, Risk, Severity,
            WIT, WIT_PACKAGE, approved_hosts, ceiling, diff, needs_person_each_use, risk, summarise,
        };
        assert!(crate::WIT.contains(crate::WIT_PACKAGE.trim_start_matches("omuzaki:plugin@")));
    }
}
