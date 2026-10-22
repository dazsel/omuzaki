//! The first-party automation nodes (BR-11, BR-26; design `docs/design/automation.md` §6).
//!
//! Each node kind registers itself next to its type (`register_node!`), so the engine (`omuzaki-live`), the editor and
//! agents find it through `omuzaki_live_api::node::node_kinds`; nothing here is called by name.
//!
//! | Folder | Node ids |
//! |---|---|
//! | [`event`] | `on.event`: the trigger on any event kind, with `where` and `min` filters |
//! | [`op`] | `op.run`: any op allowed for automations, as an action |
//!
//! [`fold`] is the one case-insensitive comparison every node uses (NFKC, then lowercase), so filters and chat
//! commands agree.

omuzaki_core::define_link! {
    omuzaki_live_api::link();
}

pub mod event;
pub mod fold;
mod l10n;
pub mod op;
mod schema;
