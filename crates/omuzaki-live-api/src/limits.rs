//! The engine's and the document's limits (design D8), plus the few numbers of D5, D7, D9 and D10 that the engine,
//! the inbox and the ops read. Each is named here once; error text derives from these constants, never from a
//! literal.
//!
//! All run on the engine's clock, so they are exact under a fake clock.

// ── Ids ─────────────────────────────────────────────────────────────────────────────────────────────────────────

/// The longest [`NodeKindId`](crate::ids::NodeKindId), in bytes.
pub const MAX_NODE_KIND_ID_LEN: usize = 96;
/// The longest [`EventKind`](crate::ids::EventKind), in bytes.
pub const MAX_EVENT_KIND_LEN: usize = 96;
/// The longest [`PortName`](crate::ids::PortName), in bytes.
pub const MAX_PORT_NAME_LEN: usize = 32;

// ── One step (D8, the loop guard) ───────────────────────────────────────────────────────────────────────────────

/// Activations in one step. Past it the step stops and the graph that ran most is suspended.
pub const MAX_ACTIVATIONS_PER_STEP: usize = 2_000;
/// Stack depth in one step. Past it, as for [`MAX_ACTIVATIONS_PER_STEP`].
pub const MAX_STACK_DEPTH: usize = 128;
/// `event.emit` hops: an emitted event's `hops` is its emitting token's root event's `hops` plus one. One past this
/// is dropped and the emitting graph suspended.
pub const MAX_EMIT_HOPS: u8 = 8;
/// How many of the last nodes on the stack a loop-guard finding names.
pub const LOOP_GUARD_LAST_NODES: usize = 8;

// ── Rates and in-flight work (D8) ───────────────────────────────────────────────────────────────────────────────

/// New tokens per graph per second (a token bucket on the engine clock); past it new tokens are dropped and counted.
pub const TOKENS_PER_SECOND_PER_GRAPH: u32 = 500;
/// The token bucket's burst, per graph.
pub const TOKEN_BURST_PER_GRAPH: u32 = 2_000;
/// Op calls in flight per show; past it `run_op` refuses with a limit error and the node fires `failed`.
pub const MAX_CALLS_IN_FLIGHT: usize = 64;
/// `net.*` requests in flight per show.
pub const MAX_NET_IN_FLIGHT: usize = 8;
/// `exec.run` programs running at once per show.
pub const MAX_EXEC_RUNNING: usize = 4;
/// `exec.run` starts per minute per show.
pub const MAX_EXEC_PER_MINUTE: u32 = 30;
/// Pending wakes per node; past it `wake_*` refuses with a limit error.
pub const MAX_WAKES_PER_NODE: usize = 1_000;
/// Keys of per-key state a node keeps (per-user cooldowns, repeat windows), oldest expired first.
pub const MAX_KEYS_PER_NODE: usize = 10_000;

// ── The inbox (D8, D9) ──────────────────────────────────────────────────────────────────────────────────────────

/// The queue of droppable kinds (`chat.message`): a droppable event is dropped and counted when it is full.
pub const INBOX_DROPPABLE_CAPACITY: usize = 10_000;
/// The hard cap of pending events of other kinds, ten times the droppable queue; past it `publish` returns `Full`
/// and the producer retains and retries.
pub const INBOX_HARD_CAP: usize = 100_000;
/// Events of one platform the inbox remembers for de-duplication.
pub const DEDUP_EVENTS: usize = 1_024;
/// How long the inbox remembers an event for de-duplication, in seconds.
pub const DEDUP_SECONDS: f64 = 600.0;
/// Undelivered non-droppable events a producer keeps, oldest dropped first.
pub const PRODUCER_BUFFER: usize = 1_000;
/// A producer's first retry delay after `Full`, in seconds; it doubles up to [`PRODUCER_RETRY_MAX_SECONDS`].
pub const PRODUCER_RETRY_SECONDS: f64 = 0.1;
/// A producer's longest retry delay, in seconds.
pub const PRODUCER_RETRY_MAX_SECONDS: f64 = 1.0;

// ── Values (D8) ─────────────────────────────────────────────────────────────────────────────────────────────────

/// The longest text value, in bytes.
pub const MAX_TEXT_BYTES: usize = 64 * 1024;
/// The most items in a list value.
pub const MAX_LIST_ITEMS: usize = 1_000;
/// The largest event payload, as JSON, in bytes.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

// ── Show size (D8; the document ops refuse past these) ─────────────────────────────────────────────────────────

/// Graphs in one show.
pub const MAX_GRAPHS: usize = 256;
/// Nodes in one graph.
pub const MAX_NODES_PER_GRAPH: usize = 512;
/// Ports of one node, inputs and outputs together.
pub const MAX_PORTS_PER_NODE: usize = 64;
/// Variables declared in one show.
pub const MAX_VARIABLES: usize = 1_024;
/// One node's parameters, as JSON, in bytes.
pub const MAX_PARAMS_BYTES: usize = 64 * 1024;

// ── The automation session (D7) ─────────────────────────────────────────────────────────────────────────────────

/// Edits per minute an automation session may request.
pub const SESSION_EDITS_PER_MINUTE: u32 = 1_200;
/// The burst of edits an automation session may request.
pub const SESSION_EDITS_BURST: u32 = 200;
/// Op runs per minute an automation session may request.
pub const SESSION_RUNS_PER_MINUTE: u32 = 6_000;
/// The burst of op runs an automation session may request.
pub const SESSION_RUNS_BURST: u32 = 600;
/// Approvals an automation session may have pending.
pub const SESSION_MAX_PENDING_APPROVALS: usize = 16;

// ── Running and stopping (D5, D13) ──────────────────────────────────────────────────────────────────────────────

/// How long the engine lets edits under its show settle before it recompiles, in seconds.
pub const RELOAD_SETTLE_SECONDS: f64 = 0.1;
/// How long `automation.stop` waits for calls in flight, in seconds.
pub const STOP_GRACE_SECONDS: f64 = 2.0;

// ── Variables and the values monitor (D10) ─────────────────────────────────────────────────────────────────────

/// The shortest time between two writes of one values file, in seconds.
pub const VALUES_WRITE_INTERVAL_SECONDS: f64 = 1.0;
/// Firings the values monitor shows, newest last.
pub const MONITOR_FIRINGS: usize = 200;
/// Events the values monitor shows, and the engine keeps in memory, newest last.
pub const MONITOR_EVENTS: usize = 50;
/// The longest value the values monitor shows for a firing, in characters.
pub const MONITOR_VALUE_CHARS: usize = 200;
/// The most often the engine refreshes the values snapshot, per second.
pub const MONITOR_REFRESH_PER_SECOND: u32 = 4;
/// How recently someone must have read the values for the engine to keep refreshing them, in seconds.
pub const MONITOR_WATCHED_SECONDS: f64 = 2.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_inbox_cap_is_ten_times_the_droppable_queue() {
        assert_eq!(INBOX_HARD_CAP, 10 * INBOX_DROPPABLE_CAPACITY);
    }

    #[test]
    fn the_numbers_are_the_designs() {
        assert_eq!((MAX_ACTIVATIONS_PER_STEP, MAX_STACK_DEPTH, MAX_EMIT_HOPS), (2_000, 128, 8));
        assert_eq!((TOKENS_PER_SECOND_PER_GRAPH, TOKEN_BURST_PER_GRAPH, MAX_CALLS_IN_FLIGHT), (500, 2_000, 64));
        assert_eq!((MAX_GRAPHS, MAX_NODES_PER_GRAPH, MAX_PORTS_PER_NODE, MAX_VARIABLES), (256, 512, 64, 1_024));
        assert_eq!(
            (MAX_TEXT_BYTES, MAX_LIST_ITEMS, MAX_PAYLOAD_BYTES, MAX_PARAMS_BYTES),
            (65_536, 1_000, 65_536, 65_536)
        );
    }
}
