//! What the shell reads from the registry (`shell.md` section 3, spec §39 rule 2).
//!
//! Workspaces and panels register themselves through `omuzaki_ui_api` ([`register_workspace!`] and [`register_panel!`]),
//! each next to its own code in whichever crate owns it, so nothing here lists them: the tab bar, the View menu and every
//! docked panel come from [`Registry::collect`], ordered by group (a workspace's group, a panel's region), then `order`,
//! then id. The shell collects once, at startup, through [`collect`], which also says so in the log when an id is
//! registered twice.
//!
//! [`register_workspace!`]: omuzaki_ui_api::register_workspace
//! [`register_panel!`]: omuzaki_ui_api::register_panel

use omuzaki_ui_api::registry::{DefKind, Registry};

/// Every workspace, panel and story linked into this binary. An id registered more than once keeps its first
/// registration (the order the registry documents) and is reported with a warning that names both places, so a clash is
/// never settled silently; [`Registry::duplicates`] has the same list for a test.
///
/// Stories belong to the gallery, which reports its own.
pub fn collect() -> Registry {
    let registry = Registry::collect();
    for duplicate in registry.duplicates().iter().filter(|d| d.kind != DefKind::Story) {
        tracing::warn!(
            kind = duplicate.kind.as_str(),
            id = duplicate.id,
            kept = duplicate.kept,
            left_out = duplicate.dropped,
            "{duplicate}"
        );
    }
    registry
}
