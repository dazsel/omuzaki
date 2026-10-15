//! Contract for WASM plugins (PL-01, ADR 0058, `docs/design/plugins.md`).
//!
//! This crate holds the contract only: no wasmtime, no manifest parsing and no behaviour. The pieces:
//!
//! - **[`WIT_PACKAGE`] and [`WIT`]**: the WIT package `omuzaki:plugin`, the base world every plugin targets.
//! - **[`PluginId`], [`PluginInfo`], [`PluginState`], [`FaultKind`]**: what a plugin is and what state it is in.
//! - **[`PluginError`]**: why a request about a plugin failed; it converts to [`OpError`](omuzaki_ops_api::OpError).
//! - **[`Plugins`]**: the service the `plugin.*` ops and the plugin manager call. The host implements it, the mock
//!   fakes it.
#![deny(missing_docs)]

mod error;
mod service;
mod state;

pub use error::PluginError;
pub use service::Plugins;
pub use state::{FaultKind, PluginId, PluginIdError, PluginInfo, PluginState};

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
            FaultKind, PluginError, PluginId, PluginIdError, PluginInfo, PluginState, Plugins, WIT, WIT_PACKAGE,
        };
        assert!(crate::WIT.contains(crate::WIT_PACKAGE.trim_start_matches("omuzaki:plugin@")));
    }
}
