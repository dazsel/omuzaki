//! Workspaces, panels and gallery stories register themselves (`shell.md` section 3, spec v3 §39 rule 2).
//!
//! ```ignore
//! register_workspace! { id: "build", title: "Build", icon: icons::CUBE, order: 20, group: Group::Make,
//!                       default_layout: layouts::build }
//! register_panel! {
//!     id: "outliner", title: "Outliner", icon: icons::TREE, region: Region::Left, min_size: [240.0, 160.0],
//!     order: 10, empty: empty_states::NO_CHARACTER, build: |ctx| Box::new(OutlinerPanel::new(ctx)),
//! }
//! ```
//!
//! Titles are the English words of the registration; the shell translates them through its message table.
//! [`Registry::collect`] orders each list by group (a workspace's [`Group`], a panel's [`Region`], a story's
//! [`StoryGroup`]), then `order`, then id, so the order never depends on link order. Two registrations with one id
//! keep the first in that order (then by their `file:line` text) and report the other in [`Registry::duplicates`],
//! never dropping it silently.

use std::any::Any;
use std::collections::{BTreeMap, btree_map};
use std::fmt;

use egui::Ui;

use crate::layout::Node;
use crate::panel::{EmptyState, Panel, PanelCtx};

/// Where a panel lives by default (`shell.md` section 1).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum PanelKind {
    /// The one panel of a workspace's centre: the viewport or a canvas. It cannot be closed or hidden.
    Primary,
    /// Everything else: docked, moved and closed by the person.
    #[default]
    Dockable,
}

/// The groups the workspace tabs fall into, in pipeline order (`shell.md` section 2).
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
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

/// The section of the UI gallery a story appears in.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum StoryGroup {
    /// Colour, type, spacing: the tokens themselves.
    Foundations,
    /// Things you press, type into or drag.
    Controls,
    /// Chips, rows and cards that show state.
    Display,
    /// Things that appear over or beside the work: toasts, approvals.
    Feedback,
    /// Whole panels assembled from the components.
    Composites,
}

/// Builds a panel when the shell first shows it.
pub type BuildPanel = fn(&PanelCtx<'_>) -> Box<dyn Panel>;

/// A registered panel. Made by [`register_panel!`](crate::register_panel).
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct PanelDef {
    /// Stable id, saved in layouts.
    pub id: &'static str,
    /// The English title.
    pub title: &'static str,
    /// An icon glyph.
    pub icon: &'static str,
    /// Where it goes by default.
    pub region: Region,
    /// Smallest size in points (`shell.md` section 4, rule 3).
    pub min_size: [f32; 2],
    /// Order within its region in the View menu and its tab group, lowest first.
    pub order: u32,
    /// Primary or dockable.
    pub kind: PanelKind,
    /// What its body shows while [`Panel::available`] is false; `None` shows a generic notice.
    pub empty: Option<EmptyState>,
    /// Builds an instance.
    pub build: BuildPanel,
    /// Where it was registered (`file:line`), named in duplicate reports.
    pub origin: &'static str,
}

impl PanelDef {
    /// A dockable panel with no empty state. The macro's constructor, `const` so it runs at link time.
    #[allow(clippy::too_many_arguments)] // one per registration field; the macro is the only caller
    pub const fn new(
        id: &'static str,
        title: &'static str,
        icon: &'static str,
        region: Region,
        min_size: [f32; 2],
        order: u32,
        build: BuildPanel,
        origin: &'static str,
    ) -> Self {
        Self { id, title, icon, region, min_size, order, kind: PanelKind::Dockable, empty: None, build, origin }
    }

    /// The panel as `kind`.
    #[must_use]
    pub const fn with_kind(mut self, kind: PanelKind) -> Self {
        self.kind = kind;
        self
    }

    /// The panel with `empty` as its empty state.
    #[must_use]
    pub const fn with_empty(mut self, empty: EmptyState) -> Self {
        self.empty = Some(empty);
        self
    }
}

/// A registered workspace: a saved layout plus a default context, not a mode. Made by
/// [`register_workspace!`](crate::register_workspace).
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct WorkspaceDef {
    /// Stable id, the name of its layout file.
    pub id: &'static str,
    /// The English title.
    pub title: &'static str,
    /// An icon glyph.
    pub icon: &'static str,
    /// Position among the tabs of its group, lowest first.
    pub order: u32,
    /// The tab group.
    pub group: Group,
    /// Builds the layout a new person (or Reset layout) gets.
    pub default_layout: fn() -> Node,
    /// Where it was registered (`file:line`).
    pub origin: &'static str,
}

impl WorkspaceDef {
    /// The macro's constructor, `const` so it runs at link time.
    pub const fn new(
        id: &'static str,
        title: &'static str,
        icon: &'static str,
        order: u32,
        group: Group,
        default_layout: fn() -> Node,
        origin: &'static str,
    ) -> Self {
        Self { id, title, icon, order, group, default_layout, origin }
    }
}

/// A registered gallery story: one component shown in all its states. Made by
/// [`register_story!`](crate::register_story).
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct StoryDef {
    /// Stable id: the key of its state and the name in screenshot file names.
    pub id: &'static str,
    /// The card heading.
    pub title: &'static str,
    /// The gallery section.
    pub group: StoryGroup,
    /// Order within its section, lowest first.
    pub order: u32,
    /// Makes the story's state (what its widgets edit) once.
    pub new_state: fn() -> Box<dyn Any>,
    /// Draws the story into a card, given the state `new_state` made.
    pub show: fn(&mut Ui, &mut dyn Any),
    /// Where it was registered (`file:line`).
    pub origin: &'static str,
}

impl StoryDef {
    /// The macro's constructor, `const` so it runs at link time.
    pub const fn new(
        id: &'static str,
        title: &'static str,
        group: StoryGroup,
        order: u32,
        new_state: fn() -> Box<dyn Any>,
        show: fn(&mut Ui, &mut dyn Any),
        origin: &'static str,
    ) -> Self {
        Self { id, title, group, order, new_state, show, origin }
    }
}

inventory::collect!(PanelDef);
inventory::collect!(WorkspaceDef);
inventory::collect!(StoryDef);

/// Which registry a [`Duplicate`] is in.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum DefKind {
    /// [`WorkspaceDef`].
    Workspace,
    /// [`PanelDef`].
    Panel,
    /// [`StoryDef`].
    Story,
}

impl DefKind {
    /// The registry's name, as the link gate records it: `workspace`, `panel` or `story`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Panel => "panel",
            Self::Story => "story",
        }
    }
}

/// An id registered more than once: which registration was kept and which was left out.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub struct Duplicate {
    /// The registry.
    pub kind: DefKind,
    /// The id.
    pub id: &'static str,
    /// Where the kept registration is (`file:line`).
    pub kept: &'static str,
    /// Where the one left out is.
    pub dropped: &'static str,
}

impl Duplicate {
    /// The report that `kind` `id` is registered at `kept` and again at `dropped`.
    pub const fn new(kind: DefKind, id: &'static str, kept: &'static str, dropped: &'static str) -> Self {
        Self { kind, id, kept, dropped }
    }
}

impl fmt::Display for Duplicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {:?} is registered more than once: kept at {}, left out at {}",
            self.kind.as_str(),
            self.id,
            self.kept,
            self.dropped
        )
    }
}

/// Everything registered in this binary, ordered by group, then `order`, then id, with one entry per id.
#[derive(Clone, Debug)]
pub struct Registry {
    workspaces: Vec<&'static WorkspaceDef>,
    panels: Vec<&'static PanelDef>,
    stories: Vec<&'static StoryDef>,
    duplicates: Vec<Duplicate>,
}

impl Registry {
    /// Collects every registration linked into this binary. Call it once at startup: it allocates and sorts.
    pub fn collect() -> Self {
        let mut duplicates = Vec::new();
        let workspaces = ordered::<WorkspaceDef>(&mut duplicates);
        let panels = ordered::<PanelDef>(&mut duplicates);
        let stories = ordered::<StoryDef>(&mut duplicates);
        duplicates.sort_unstable();
        Self { workspaces, panels, stories, duplicates }
    }

    /// The workspaces, in tab order: by group, then order, then id.
    pub fn workspaces(&self) -> &[&'static WorkspaceDef] {
        &self.workspaces
    }

    /// The panels, in View-menu order: by region, then order, then id.
    pub fn panels(&self) -> &[&'static PanelDef] {
        &self.panels
    }

    /// The gallery stories, in gallery order: by group, then order, then id.
    pub fn stories(&self) -> &[&'static StoryDef] {
        &self.stories
    }

    /// The workspace registered as `id`.
    pub fn workspace(&self, id: &str) -> Option<&'static WorkspaceDef> {
        self.workspaces.iter().copied().find(|w| w.id == id)
    }

    /// The panel registered as `id`.
    pub fn panel(&self, id: &str) -> Option<&'static PanelDef> {
        self.panels.iter().copied().find(|p| p.id == id)
    }

    /// Every id registered more than once, sorted by registry, id and place. The shell reports them at startup; a
    /// test of an App asserts there are none.
    pub fn duplicates(&self) -> &[Duplicate] {
        &self.duplicates
    }
}

/// What [`ordered`] needs of a registration.
trait Entry: inventory::Collect {
    const KIND: DefKind;
    fn id(&self) -> &'static str;
    /// The group's position (its variant's index), then `order`.
    fn rank(&self) -> (u8, u32);
    fn origin(&self) -> &'static str;
}

macro_rules! entry {
    ($ty:ty, $kind:expr, $group:ident) => {
        impl Entry for $ty {
            const KIND: DefKind = $kind;
            fn id(&self) -> &'static str {
                self.id
            }
            fn rank(&self) -> (u8, u32) {
                (self.$group as u8, self.order)
            }
            fn origin(&self) -> &'static str {
                self.origin
            }
        }
    };
}
entry!(WorkspaceDef, DefKind::Workspace, group);
entry!(PanelDef, DefKind::Panel, region);
entry!(StoryDef, DefKind::Story, group);

/// The registrations of `T` by group, order, id and place; a repeated id keeps the first and is reported in `duplicates`.
fn ordered<T: Entry>(duplicates: &mut Vec<Duplicate>) -> Vec<&'static T> {
    let mut all: Vec<&'static T> = inventory::iter::<T>.into_iter().collect();
    all.sort_by_key(|e| (e.rank(), e.id(), e.origin()));
    let mut kept: BTreeMap<&'static str, &'static str> = BTreeMap::new();
    all.retain(|e| match kept.entry(e.id()) {
        btree_map::Entry::Vacant(slot) => {
            slot.insert(e.origin());
            true
        }
        btree_map::Entry::Occupied(first) => {
            duplicates.push(Duplicate::new(T::KIND, e.id(), first.get(), e.origin()));
            false
        }
    });
    all
}

/// Registers a panel next to its code. `kind` (default [`PanelKind::Dockable`]) and `empty` are optional, in this
/// order; `build` is any non-capturing closure or function from `&PanelCtx` to `Box<dyn Panel>`.
///
/// ```ignore
/// omuzaki_ui_api::register_panel! {
///     id: "outliner", title: "Outliner", icon: icons::TREE, region: Region::Left, min_size: [240.0, 160.0],
///     order: 10, empty: NO_CHARACTER, build: |ctx| Box::new(OutlinerPanel::new(ctx)),
/// }
/// ```
#[macro_export]
macro_rules! register_panel {
    (id: $id:literal, title: $title:literal, icon: $icon:expr, region: $region:expr, min_size: $min:expr,
     order: $order:literal, $(kind: $kind:expr,)? $(empty: $empty:expr,)? build: $build:expr $(,)?) => {
        $crate::__inventory::submit! {
            $crate::registry::PanelDef::new(
                $id, $title, $icon, $region, $min, $order, $build,
                ::core::concat!(::core::file!(), ":", ::core::line!()),
            )
            $(.with_kind($kind))? $(.with_empty($empty))?
        }
        $crate::__record_registration!("panel", $id);
    };
}

/// Registers a workspace next to its default layout.
///
/// ```ignore
/// omuzaki_ui_api::register_workspace! {
///     id: "build", title: "Build", icon: icons::CUBE, order: 20, group: Group::Make, default_layout: layouts::build,
/// }
/// ```
#[macro_export]
macro_rules! register_workspace {
    (id: $id:literal, title: $title:literal, icon: $icon:expr, order: $order:literal, group: $group:expr,
     default_layout: $layout:expr $(,)?) => {
        $crate::__inventory::submit! {
            $crate::registry::WorkspaceDef::new(
                $id, $title, $icon, $order, $group, $layout,
                ::core::concat!(::core::file!(), ":", ::core::line!()),
            )
        }
        $crate::__record_registration!("workspace", $id);
    };
}

/// Registers a gallery story. `state` is a `Default` type holding what the story's widgets edit.
///
/// ```ignore
/// omuzaki_ui_api::register_story! {
///     id: "button", title: "Button", group: StoryGroup::Controls, order: 10, state: ButtonState,
///     show: |ui, state| { /* draw */ },
/// }
/// ```
#[macro_export]
macro_rules! register_story {
    (id: $id:literal, title: $title:literal, group: $group:expr, order: $order:literal, state: $state:ty,
     show: $show:expr $(,)?) => {
        $crate::__inventory::submit! {
            $crate::registry::StoryDef::new(
                $id, $title, $group, $order,
                || ::std::boxed::Box::new(<$state as ::core::default::Default>::default()),
                |ui, state| {
                    let show: fn(&mut $crate::__egui::Ui, &mut $state) = $show;
                    match state.downcast_mut::<$state>() {
                        ::core::option::Option::Some(state) => show(ui, state),
                        // The gallery passes only the state this story's `new_state` made.
                        ::core::option::Option::None => ::core::debug_assert!(false, "story state of another type"),
                    }
                },
                ::core::concat!(::core::file!(), ":", ::core::line!()),
            )
        }
        $crate::__record_registration!("story", $id);
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::Node;

    struct Blank;

    impl Panel for Blank {
        fn ui(&mut self, _ui: &mut Ui, _ctx: &PanelCtx<'_>) {}
    }

    // Two panels and two stories under one id each, registered in the opposite of their sort order.
    crate::register_panel! { id: "dup", title: "Second", icon: "", region: Region::Left, min_size: [240.0, 160.0],
    order: 5, build: |_| Box::new(Blank) }
    crate::register_panel! { id: "dup", title: "First", icon: "", region: Region::Left, min_size: [240.0, 160.0],
    order: 5, kind: PanelKind::Primary, build: |_| Box::new(Blank) }
    crate::register_story! { id: "twice", title: "Later", group: StoryGroup::Controls, order: 9, state: u32,
    show: |_, _| {} }
    crate::register_story! { id: "twice", title: "Earlier", group: StoryGroup::Controls, order: 1, state: u32,
    show: |_, n| *n += 1 }
    crate::register_workspace! { id: "solo", title: "Solo", icon: "", order: 1, group: Group::Make,
    default_layout: || Node::pane("dup") }
    // A later group sorts after an earlier one whatever its `order`.
    crate::register_workspace! { id: "aaa", title: "Watch", icon: "", order: 0, group: Group::Watch,
    default_layout: || Node::pane("dup") }
    crate::register_panel! { id: "aaa", title: "Bottom", icon: "", region: Region::Bottom, min_size: [240.0, 160.0],
    order: 0, build: |_| Box::new(Blank) }

    #[test]
    fn lists_are_ordered_by_group_then_order_then_id() {
        let registry = Registry::collect();
        assert_eq!(registry.workspaces().iter().map(|w| w.id).collect::<Vec<_>>(), ["solo", "aaa"]);
        assert_eq!(registry.panels().iter().map(|p| p.id).collect::<Vec<_>>(), ["dup", "aaa"]);
    }

    #[test]
    fn a_repeated_id_keeps_the_first_by_order_then_place_and_reports_the_other() {
        let registry = Registry::collect();
        let dup = registry.panel("dup").map(|p| (p.title, p.kind));
        assert_eq!(dup, Some(("Second", PanelKind::Dockable)), "same order: the first by place wins");
        assert_eq!(registry.stories().iter().map(|s| s.title).collect::<Vec<_>>(), ["Earlier"], "lower order wins");
        let found: Vec<(DefKind, &str)> = registry.duplicates().iter().map(|d| (d.kind, d.id)).collect();
        assert_eq!(found, [(DefKind::Panel, "dup"), (DefKind::Story, "twice")]);
        let panel = registry.duplicates()[0];
        assert!(panel.kept.contains("registry.rs:") && panel.dropped.contains("registry.rs:"), "{panel}");
        assert_ne!(panel.kept, panel.dropped);
        assert!(panel.to_string().starts_with("panel \"dup\" is registered more than once: kept at "), "{panel}");
        let recorded = omuzaki_core::link::registered().into_iter().filter(|r| r.package == "omuzaki-ui-api").count();
        assert_eq!(recorded, 7, "every registration is recorded for the link gate, duplicates included");
    }

    #[test]
    fn a_story_draws_with_the_state_it_made_and_a_workspace_builds_its_layout() {
        let registry = Registry::collect();
        let story = registry.stories()[0];
        let mut state = (story.new_state)();
        let ctx = egui::Context::default();
        ctx.run_ui(egui::RawInput::default(), |ui| (story.show)(ui, state.as_mut())).drop_without_applying_deltas();
        assert_eq!(state.downcast_ref::<u32>(), Some(&1), "the selected story's callback ran on its state");
        let layout = registry.workspace("solo").map(|w| (w.default_layout)());
        assert_eq!(layout, Some(Node::pane("dup")));
    }
}
