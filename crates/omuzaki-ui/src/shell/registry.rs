//! Workspaces and panels register themselves (`shell.md` section 3, spec §39 rule 2).
//!
//! ```ignore
//! register_workspace! { id: "build", title: "Build", icon: icons::CUBE, order: 20, group: Group::Make,
//!                       layout: layouts::build }
//! register_panel! { id: "outliner", title: "Outliner", icon: icons::TREE_STRUCTURE, region: Region::Left,
//!                   min_size: [240.0, 160.0], order: 10, kind: PanelKind::Dockable }
//! ```
//!
//! Nothing edits a central list: the tab bar and the View menu show whatever is compiled in, ordered by `order`.
//! Titles are the English words of the registration; the shell passes them through its message table
//! ([`super::text`]) so they translate like everything else.

use super::layout::Layout;

/// Where a panel lives by default.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Region {
    /// The left dock.
    Left,
    /// The centre: the primary panel.
    Center,
    /// The right dock.
    Right,
    /// The bottom dock.
    Bottom,
}

/// What a panel may do (`shell.md` section 4, rule 2).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PanelKind {
    /// The one panel of a workspace's centre: the viewport or a canvas. It cannot be closed or hidden.
    Primary,
    /// Everything else: docked, moved and closed by the person.
    Dockable,
}

/// A registered panel.
pub struct PanelDef {
    /// Stable id, saved in layouts.
    pub id: &'static str,
    /// The English title.
    pub title: &'static str,
    /// An icon glyph from [`crate::ds::icons`].
    pub icon: &'static str,
    /// Where it goes by default.
    pub region: Region,
    /// Smallest size, in points: left panels 240 by 160, right ones 288 by 160, bottom ones 160 high.
    pub min_size: [f32; 2],
    /// Order within its region's tab group in the View menu.
    pub order: u32,
    /// Primary or dockable.
    pub kind: PanelKind,
}

inventory::collect!(PanelDef);

/// The four groups the workspace tabs fall into, in pipeline order (`shell.md` section 2).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Group {
    /// Concept, Build, Face, Rig, Look.
    Make,
    /// Perform, Stage, Broadcast.
    Run,
    /// Shots.
    Produce,
    /// Theatre.
    Watch,
}

/// A registered workspace: a saved layout plus a default context, not a mode.
pub struct WorkspaceDef {
    /// Stable id, the name of its layout file.
    pub id: &'static str,
    /// The English title.
    pub title: &'static str,
    /// An icon glyph.
    pub icon: &'static str,
    /// Position among the tabs.
    pub order: u32,
    /// The tab group.
    pub group: Group,
    /// Builds the layout a new person (or Reset layout) gets.
    pub layout: fn() -> Layout,
}

inventory::collect!(WorkspaceDef);

/// Registers a panel.
#[macro_export]
macro_rules! register_panel {
    (id: $id:literal, title: $title:literal, icon: $icon:expr, region: $region:expr, min_size: $min:expr, order: $order:literal, kind: $kind:expr $(,)?) => {
        ::inventory::submit! {
            $crate::shell::registry::PanelDef {
                id: $id, title: $title, icon: $icon, region: $region, min_size: $min, order: $order, kind: $kind,
            }
        }
        $crate::__record_registration!("panel", $id);
    };
}

/// Registers a workspace.
#[macro_export]
macro_rules! register_workspace {
    (id: $id:literal, title: $title:literal, icon: $icon:expr, order: $order:literal, group: $group:expr, layout: $layout:expr $(,)?) => {
        ::inventory::submit! {
            $crate::shell::registry::WorkspaceDef {
                id: $id, title: $title, icon: $icon, order: $order, group: $group, layout: $layout,
            }
        }
        $crate::__record_registration!("workspace", $id);
    };
}

/// Every registered workspace, in tab order.
pub fn workspaces() -> Vec<&'static WorkspaceDef> {
    let mut all: Vec<_> = inventory::iter::<WorkspaceDef>.into_iter().collect();
    all.sort_by_key(|w| (w.group, w.order, w.id));
    all
}

/// Every registered panel, ordered by region and `order`.
pub fn panels() -> Vec<&'static PanelDef> {
    let mut all: Vec<_> = inventory::iter::<PanelDef>.into_iter().collect();
    all.sort_by_key(|p| (p.region as u8, p.order, p.id));
    all
}

/// The panel registered as `id`.
pub fn panel(id: &str) -> Option<&'static PanelDef> {
    inventory::iter::<PanelDef>.into_iter().find(|p| p.id == id)
}

/// The workspace registered as `id`.
pub fn workspace(id: &str) -> Option<&'static WorkspaceDef> {
    inventory::iter::<WorkspaceDef>.into_iter().find(|w| w.id == id)
}
