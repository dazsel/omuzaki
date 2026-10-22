//! A story: one component shown in all its states. Stories register themselves with `register_story!` from
//! `omuzaki_ui_api`, next to the component they show, so adding a component never edits a shared list (spec v3 section 39
//! rule 2); the gallery lists whatever is compiled in, in the registry's order (section, then `order`, then id).
//!
//! ```ignore
//! omuzaki_ui_api::register_story! {
//!     id: "button", title: "Button", group: Group::Controls, order: 10,
//!     state: ButtonState,                       // `Default`; what the widgets edit
//!     show: |ui, state| { /* draw */ },
//! }
//! ```

use omuzaki_ui_api::registry::{DefKind, Duplicate, Registry, StoryDef};

/// Which part of the library a story belongs to; the gallery groups cards by it.
pub use omuzaki_ui_api::registry::StoryGroup as Group;

/// One registered story: its id, heading, section, order, the state it edits and how it draws.
pub type Story = StoryDef;

/// The heading shown above a group.
pub const fn group_title(group: Group) -> &'static str {
    match group {
        Group::Foundations => "Foundations",
        Group::Controls => "Controls",
        Group::Display => "Display",
        Group::Feedback => "Feedback",
        Group::Composites => "Composites",
        // A section added to the contract before the gallery names it.
        _ => "More",
    }
}

/// Every registered story, grouped and ordered, with one entry per id.
pub fn all() -> Vec<&'static Story> {
    Registry::collect().stories().to_vec()
}

/// The story ids registered more than once: the first (in registry order) is shown, and each other is named here.
pub fn duplicates() -> Vec<Duplicate> {
    story_duplicates(&Registry::collect())
}

/// Every registered story, as [`all`], after logging each id registered twice with a warning that names both places, so
/// a clash is never settled silently. The gallery calls it once, when it starts.
pub(super) fn collect() -> Vec<&'static Story> {
    let registry = Registry::collect();
    for duplicate in story_duplicates(&registry) {
        tracing::warn!(id = duplicate.id, kept = duplicate.kept, left_out = duplicate.dropped, "{duplicate}");
    }
    registry.stories().to_vec()
}

fn story_duplicates(registry: &Registry) -> Vec<Duplicate> {
    registry.duplicates().iter().filter(|d| d.kind == DefKind::Story).copied().collect()
}
