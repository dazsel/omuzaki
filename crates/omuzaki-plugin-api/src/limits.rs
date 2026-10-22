//! The sandbox's numbers (design `plugins.md` §4.2 to §5.2, §7), defined once so the wasmtime host and the mock enforce
//! the same limits and the conformance suite checks them against both.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One MiB, in bytes.
pub const MIB: u64 = 1024 * 1024;

/// The largest component file, checked before compiling.
pub const COMPONENT_MAX_BYTES: u64 = 16 * MIB;

/// Linear memory of a worker instance (one memory).
pub const WORKER_MEMORY_BYTES: u64 = 64 * MIB;

/// Linear memory of a node instance on the automation engine thread.
pub const NODE_MEMORY_BYTES: u64 = 16 * MIB;

/// What one running plugin costs the host beyond its linear memory: its worker thread's stack, compiled code and store.
pub const HOST_SIDE_BYTES: u64 = 8 * MIB;

/// Linear memory and host side of every plugin together, in the editor (of `ram.editor`'s 1.5 GB).
pub const ALL_PLUGINS_EDITOR_BYTES: u64 = 256 * MIB;

/// The same in performance mode (of `ram.runtime`'s 400 MB); provisional (ADR 0058 decision 13).
pub const ALL_PLUGINS_PERFORMANCE_BYTES: u64 = 128 * MIB;

/// Tables per instance.
pub const MAX_TABLES: u32 = 1;

/// Elements per table.
pub const MAX_TABLE_ELEMENTS: usize = 10_000;

/// Core instances per component.
pub const MAX_INSTANCES: usize = 32;

/// The wasm stack.
pub const MAX_WASM_STACK_BYTES: usize = 512 * 1024;

/// Host calls a second, across every import (a token bucket on the runner's clock).
pub const HOST_CALLS_PER_SECOND: u32 = 10_000;

/// The host-call bucket's burst.
pub const HOST_CALL_BURST: u32 = 1_000;

/// The fixed fuel every import charges, about 2 µs on `R` (§4.3).
pub const IMPORT_FUEL: u64 = 2_000;

/// The fuel every import charges per byte passed either way.
pub const IMPORT_FUEL_PER_BYTE: u64 = 1;

/// Fuel per millisecond of work on `R`: 10⁶ fuel is about 1 ms (automation D15). The plugin bench calibrates it.
pub const FUEL_PER_MS: u64 = 1_000_000;

/// The CPU share every plugin gets: this many milliseconds of work a second (10 % of a core), host calls included.
pub const CPU_SHARE_MS_PER_SECOND: u64 = 100;

/// The CPU share in fuel a second.
pub const CPU_SHARE_FUEL_PER_SECOND: u64 = CPU_SHARE_MS_PER_SECOND * FUEL_PER_MS;

/// `ops.call` arguments and `plan` input, before decoding, and their results, before returning.
pub const OP_BYTES_MAX: usize = 1024 * 1024;

/// `list()` results and `describe` strings.
pub const EXPORT_LIST_BYTES_MAX: usize = 1024 * 1024;

/// The JSON settings passed to `init`.
pub const INIT_CONFIG_BYTES_MAX: usize = 64 * 1024;

/// The most bytes one `get-random-bytes` or `get-insecure-random-bytes` may ask for; over it traps before the host
/// allocates.
pub const RANDOM_REQUEST_MAX: u64 = 4_096;

/// Entry calls waiting for a plugin's worker; beyond it, new ones are refused `limited`.
pub const WORKER_QUEUE: usize = 64;

/// The waits before each restart after a fault, in seconds.
pub const RESTART_BACKOFF_SECONDS: [f64; 3] = [1.0, 10.0, 60.0];

/// Faults within [`FAULT_WINDOW_SECONDS`] that disable a plugin.
pub const FAULTS_TO_DISABLE: usize = 3;

/// The window [`FAULTS_TO_DISABLE`] counts in.
pub const FAULT_WINDOW_SECONDS: f64 = 600.0;

/// The longest log line, in bytes; longer lines are cut at a character boundary.
pub const LOG_LINE_MAX_BYTES: usize = 1024;

/// Log lines a second, after the burst (§7).
pub const LOG_LINES_PER_SECOND: u32 = 20;

/// The log's burst.
pub const LOG_BURST: u32 = 100;

/// Lines the in-memory log keeps, oldest dropped first.
pub const LOG_RING_LINES: usize = 1_000;

/// The kind of call a host makes into a plugin. Each has its own fuel cap and epoch deadline (§4.3).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CallKind {
    /// `lifecycle.init`.
    Init,
    /// `lifecycle.approval-resolved`.
    ApprovalResolved,
    /// A worker entry of an extension world, or a `plugin.run` command.
    Worker,
    /// `lifecycle.shutdown`, with every effectful import denied.
    Shutdown,
    /// A node activation on the automation engine thread (PL-03).
    Node,
}

impl CallKind {
    /// Every call kind.
    pub const ALL: [CallKind; 5] = [Self::Init, Self::ApprovalResolved, Self::Worker, Self::Shutdown, Self::Node];

    /// The entry's name as the guest exports it, for fault reasons ("out of fuel in init").
    pub fn entry(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::ApprovalResolved => "approval-resolved",
            Self::Worker => "a worker entry",
            Self::Shutdown => "shutdown",
            Self::Node => "a node activation",
        }
    }

    /// The fuel cap and deadline of this kind.
    pub fn budget(self) -> Budget {
        match self {
            Self::Init => Budget { fuel: 1_000_000_000, deadline_ms: 5_000 },
            Self::ApprovalResolved | Self::Worker => Budget { fuel: 1_000_000_000, deadline_ms: 30_000 },
            Self::Shutdown => Budget { fuel: 100_000_000, deadline_ms: 1_000 },
            Self::Node => Budget { fuel: 1_000_000, deadline_ms: 2 },
        }
    }

    /// Whether running out of fuel or time faults the plugin. A breached `shutdown` only drops the instance.
    pub fn breach_faults(self) -> bool {
        self != Self::Shutdown
    }
}

/// What one call may use.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Budget {
    /// Fuel, close to one unit per wasm instruction; imports charge it too.
    pub fuel: u64,
    /// The wall-clock backstop, in milliseconds.
    pub deadline_ms: u64,
}

impl Budget {
    /// The fuel a call actually gets: the smaller of this cap and what is left of the plugin's CPU share this second.
    pub fn fuel_within(self, share_left: u64) -> u64 {
        self.fuel.min(share_left)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_budget_table_is_the_designs() {
        let table: Vec<(CallKind, u64, u64)> =
            CallKind::ALL.iter().map(|k| (*k, k.budget().fuel, k.budget().deadline_ms)).collect();
        assert_eq!(
            table,
            [
                (CallKind::Init, 1_000_000_000, 5_000),
                (CallKind::ApprovalResolved, 1_000_000_000, 30_000),
                (CallKind::Worker, 1_000_000_000, 30_000),
                (CallKind::Shutdown, 100_000_000, 1_000),
                (CallKind::Node, 1_000_000, 2),
            ]
        );
        assert!(!CallKind::Shutdown.breach_faults() && CallKind::Init.breach_faults());
        assert_eq!(CallKind::Init.budget().fuel_within(CPU_SHARE_FUEL_PER_SECOND), 100_000_000);
        assert_eq!(CallKind::Node.budget().fuel_within(CPU_SHARE_FUEL_PER_SECOND), 1_000_000);
    }

    #[test]
    fn call_kinds_round_trip() {
        for kind in CallKind::ALL {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(serde_json::from_str::<CallKind>(&json).unwrap(), kind);
        }
        assert_eq!(serde_json::to_string(&CallKind::ApprovalResolved).unwrap(), "\"approval_resolved\"");
    }
}
