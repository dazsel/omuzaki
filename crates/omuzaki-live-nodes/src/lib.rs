//! The first-party automation nodes (BR-11, BR-26; design `docs/design/automation.md` §6).
//!
//! Each node kind registers itself next to its type (`register_node!`), so the engine (`omuzaki-live`), the editor and
//! agents find it through `omuzaki_live_api::node::node_kinds`; nothing here is called by name.
//!
//! | Folder | Node ids |
//! |---|---|
//! | [`event`] | `on.event`: the trigger on any event kind, with `where` and `min` filters |
//! | [`emit`] | `event.emit`: sends a `custom.<name>` event other automations hear |
//! | [`flow`] | `flow.wait`, `flow.random`, `flow.sequence`, `flow.switch`, `flow.branch`, `flow.then`, `flow.cooldown`, `flow.gate` |
//! | [`logic`] | `logic.and`, `logic.or`, `logic.not` |
//! | [`math`] | `math.op`, `math.map_range`, `math.ease`, `math.compare`, `math.random_range` |
//! | [`op`] | `op.run`: any op allowed for automations, as an action |
//! | [`text`] | `text.format`, `text.to_number`, `text.contains` |
//!
//! [`fold`] is the one case-insensitive comparison every node uses (NFKC, then lowercase), so filters and chat
//! commands agree.

omuzaki_core::define_link! {
    omuzaki_live_api::link();
}

pub mod emit;
pub mod event;
pub mod flow;
pub mod fold;
mod l10n;
pub mod logic;
pub mod math;
pub mod op;
mod schema;
pub mod text;
