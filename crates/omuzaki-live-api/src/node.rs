//! The node contract (design D3, D4): ports, [`NodeKind`] and [`Node`], the [`Input`]s a node handles, what it may do
//! ([`NodeCx`], [`EvalCx`]), its errors, its registration ([`register_node!`](crate::register_node)) and its random
//! stream.
//!
//! A whole node, as a node task writes it (`flow.wait`, design D4):
//!
//! ```
//! use omuzaki_core::time::Seconds;
//! use omuzaki_live_api::node::{Category, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, Ports, WakeId};
//! use omuzaki_live_api::value::{PortType, Val};
//! use schemars::JsonSchema;
//! use serde::{Deserialize, Serialize};
//!
//! /// Parameters of `flow.wait`: none; the duration is an input so it can vary per event.
//! #[derive(Serialize, Deserialize, JsonSchema, Default)]
//! #[serde(deny_unknown_fields)]
//! pub struct WaitParams {}
//!
//! /// Delays each event on `in` by `seconds`; every event waits on its own, and `cancel` drops the waiting ones.
//! pub struct Wait {
//!     pending: Vec<WakeId>,
//! }
//!
//! impl NodeKind for Wait {
//!     type Params = WaitParams;
//!     fn meta() -> NodeMeta {
//!         NodeMeta::new("flow.wait", "Wait, then pass the event on.", NodeClass::Active, Category::Flow)
//!     }
//!     fn ports(_: &WaitParams) -> Ports {
//!         Ports::new()
//!             .event_in("in", "Each event to delay.")
//!             .event_in("cancel", "Drops every event still waiting.")
//!             .value_in("seconds", PortType::Number, Val::Number(1.0), "How long to wait.")
//!             .event_out("out", "The event, after the wait.")
//!     }
//!     fn new(_: WaitParams) -> Self {
//!         Self { pending: Vec::new() }
//!     }
//! }
//!
//! impl Node for Wait {
//!     fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
//!         match input {
//!             Input::Event { port: "in", .. } => {
//!                 let seconds = cx.input_number("seconds")?.max(0.0);
//!                 self.pending.push(cx.wake_after(Seconds(seconds))?);
//!                 Ok(())
//!             }
//!             Input::Event { port: "cancel", .. } => {
//!                 self.pending.drain(..).for_each(|id| cx.cancel_wake(id));
//!                 Ok(())
//!             }
//!             Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
//!             Input::Wake { id, .. } => {
//!                 self.pending.retain(|p| *p != id);
//!                 cx.fire("out")
//!             }
//!             Input::Start | Input::Stop | Input::Trigger { .. } | Input::Reply { .. } => Ok(()), // not used here
//!             other => Err(NodeError::unhandled(&other)), // an input added after this node (D21)
//!         }
//!     }
//! }
//! omuzaki_live_api::register_node!(Wait);
//!
//! let wait = omuzaki_live_api::node::find_node_kind("flow.wait").expect("registered");
//! assert_eq!(wait.kind_id().expect("a valid id").as_str(), "flow.wait");
//! ```

mod cx;
mod error;
mod kind;
mod ports;
mod registration;

use omuzaki_core::rng::Rng;

pub use cx::{EvalCx, EvalHost, NodeCx, NodeHost};
pub use error::{NodeError, NodeErrorCode, ParamError, ParamErrors};
pub use kind::{CallId, Category, Input, Node, NodeClass, NodeKind, NodeMeta, WakeId};
pub use ports::{PortDecl, Ports, validate_ports};
pub use registration::{DynNodeKind, NodeRegistration, find_node_kind, node_kinds};

/// The stream a node draws from (design D6): FNV-1a 64 of `"<graph>/<node>"`, so adding or removing a node never
/// shifts another node's numbers.
pub fn rng_stream(graph: &str, node: &str) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    graph
        .bytes()
        .chain(*b"/")
        .chain(node.bytes())
        .fold(OFFSET, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(PRIME))
}

/// A node's own generator (design D6): `Rng::new(seed).fork(stream)` on a fresh root, so the order nodes are created
/// in doesn't matter. A rebuilt instance restarts its stream.
pub fn node_rng(seed: u32, graph: &str, node: &str) -> Rng {
    Rng::new(u64::from(seed)).fork(rng_stream(graph, node))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stream_is_fnv1a_of_graph_slash_node() {
        // FNV-1a 64 of "bonk/cool", from an independent implementation checked against the published "foobar" vector.
        assert_eq!(rng_stream("bonk", "cool"), 0x868e_cbb5_ec96_23af);
        assert_ne!(rng_stream("bonk", "cool"), rng_stream("bonk", "count"));
        assert_ne!(rng_stream("a", "bc"), rng_stream("ab", "c"));
        assert_eq!(node_rng(7, "g", "n"), node_rng(7, "g", "n"));
    }
}
