//! The WASM plugin host (PL-01, ADR 0058, `docs/design/plugins.md`). Native only.
//!
//! - [`PluginEngine`]: wasmtime with fuel and epoch interruption, its epoch ticker and the all-plugins memory budget.
//! - [`load`]: the component checks: size, component not core module, imports inside the stub subset, the WIT
//!   version window and the manifest's `api`.
//! - [`LoadedPlugin`]: one instance under its limits; entry calls run under fuel and an epoch deadline per
//!   [`CallKind`](omuzaki_plugin_api::CallKind), within the CPU share, and every way they stop becomes a [`Fault`].
//! - [`WasmPlugins`]: the [`Plugins`](omuzaki_plugin_api::Plugins) service: a worker thread per plugin, its session,
//!   faults, restart backoff and auto-disable.
//! - [`log`]: the plugin's capped log and its rotated file.
//! - [`bridge`]: the seam the op bridge plugs into.
#![deny(missing_docs)]

pub mod bridge;
mod engine;
mod imports;
mod instance;
pub mod limits;
pub mod load;
pub mod log;
mod wasi;

pub use engine::{EPOCH_TICK, EPOCH_TICK_MS, HostConfig, PluginEngine};
pub use instance::{Fault, InstanceKind, InstanceSetup, LoadedPlugin, StartError, insecure_seed};
