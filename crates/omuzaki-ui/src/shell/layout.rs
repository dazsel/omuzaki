//! A workspace's layout (`shell.md` section 4): a small tree of panel ids, saved per person.
//!
//! The tree is ours, not the docking widget's: it is what is written to disk, so a change inside `egui_tiles` can never
//! make a saved file unreadable. [`Layout::to_tree`] and [`Layout::from_tree`] convert to and from the widget's tree,
//! and [`Layout::repaired`] makes a loaded layout safe (a panel that no longer exists is dropped, the primary panel is
//! always there) before it is shown.

use std::cell::RefCell;
use std::collections::HashSet;

use egui_tiles::{Container, Linear, LinearDir, Tabs, Tile, TileId, Tiles, Tree};
use omuzaki_ui_api::layout::{Dir as ApiDir, Node as ApiNode};
use serde::{Deserialize, Serialize};

/// The file format version. A file with another version is replaced by the default layout.
pub const VERSION: u32 = 1;

/// The pane type of the docking tree: a panel's registered id.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PanelId(pub String);

/// Which way a split runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    /// Children side by side.
    Horizontal,
    /// Children stacked.
    Vertical,
}

/// One node of a layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Node {
    /// A panel.
    Pane {
        /// Its registered id.
        panel: String,
    },
    /// A tab group; `active` indexes `children`.
    Tabs {
        /// The tabs, in order.
        children: Vec<Node>,
        /// The shown one.
        #[serde(default)]
        active: usize,
    },
    /// A split with a relative share per child.
    Split {
        /// Which way it runs.
        dir: Dir,
        /// The parts, in order.
        children: Vec<Node>,
        /// How much of the space each takes, relative to the others; missing entries count as 1.
        #[serde(default)]
        shares: Vec<f32>,
    },
}

impl Node {
    /// A panel.
    pub fn pane(panel: &str) -> Self {
        Self::Pane { panel: panel.to_owned() }
    }

    /// A tab group showing the first of `panels`.
    pub fn tabs(panels: &[&str]) -> Self {
        Self::Tabs { children: panels.iter().map(|p| Self::pane(p)).collect(), active: 0 }
    }

    /// A split of `children` with `shares`.
    pub fn split(dir: Dir, children: Vec<Self>, shares: &[f32]) -> Self {
        Self::Split { dir, children, shares: shares.to_vec() }
    }

    /// The node for a workspace's registered default layout, written in the contract's tree (`omuzaki_ui_api::layout`).
    /// `None` when the tree holds a kind of node this shell does not know, so a newer contract never half-converts.
    pub fn from_api(node: &ApiNode) -> Option<Self> {
        match node {
            ApiNode::Pane { panel, .. } => Some(Self::Pane { panel: panel.clone() }),
            ApiNode::Tabs { children, active, .. } => {
                Some(Self::Tabs { children: Self::from_api_all(children)?, active: *active })
            }
            ApiNode::Split { dir, children, shares, .. } => Some(Self::Split {
                dir: if *dir == ApiDir::Vertical { Dir::Vertical } else { Dir::Horizontal },
                children: Self::from_api_all(children)?,
                shares: shares.clone(),
            }),
            _ => None,
        }
    }

    fn from_api_all(children: &[ApiNode]) -> Option<Vec<Self>> {
        children.iter().map(Self::from_api).collect()
    }

    /// Whether `panel` is in this node.
    fn contains(&self, panel: &str) -> bool {
        let mut all = Vec::new();
        self.collect(&mut all);
        all.contains(&panel)
    }

    /// The first panel in reading order.
    fn first_panel(&self) -> Option<&str> {
        match self {
            Self::Pane { panel } => Some(panel),
            Self::Tabs { children, .. } | Self::Split { children, .. } => children.iter().find_map(Self::first_panel),
        }
    }

    fn collect<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Self::Pane { panel } => out.push(panel),
            Self::Tabs { children, .. } | Self::Split { children, .. } => children.iter().for_each(|c| c.collect(out)),
        }
    }

    /// Makes every split's shares usable by the docking widget, which panics on a NaN: a share that is missing, not
    /// finite or not positive becomes the mean of the good ones (1 when none is), and extras are dropped.
    fn sanitise_shares(&mut self) {
        match self {
            Self::Pane { .. } => {}
            Self::Tabs { children, .. } => children.iter_mut().for_each(Self::sanitise_shares),
            Self::Split { children, shares, .. } => {
                children.iter_mut().for_each(Self::sanitise_shares);
                shares.resize(children.len(), f32::NAN);
                let good: Vec<f32> = shares.iter().copied().filter(|s| s.is_finite() && *s > 0.0).collect();
                let mean = if good.is_empty() { 1.0 } else { good.iter().sum::<f32>() / good.len() as f32 };
                for share in shares.iter_mut().filter(|s| !(s.is_finite() && **s > 0.0)) {
                    *share = mean;
                }
            }
        }
    }

    /// The node with `extra` panels joined to the tab group of `anchor` (a lone `anchor` becomes a group).
    fn joined(self, anchor: &str, extra: &[&str]) -> Self {
        let is_anchor = |n: &Self| matches!(n, Self::Pane { panel } if panel == anchor);
        let with_extra = |mut children: Vec<Self>| {
            children.extend(extra.iter().map(|p| Self::pane(p)));
            children
        };
        match self {
            Self::Pane { .. } if is_anchor(&self) => Self::Tabs { children: with_extra(vec![self]), active: 0 },
            Self::Pane { .. } => self,
            Self::Tabs { children, active } if children.iter().any(is_anchor) => {
                Self::Tabs { children: with_extra(children), active }
            }
            Self::Tabs { children, active } => {
                Self::Tabs { children: children.into_iter().map(|c| c.joined(anchor, extra)).collect(), active }
            }
            Self::Split { dir, children, shares } => {
                Self::Split { dir, children: children.into_iter().map(|c| c.joined(anchor, extra)).collect(), shares }
            }
        }
    }

    /// The node without the panels `keep` refuses, and without containers that became empty. `None` when nothing is left.
    fn pruned(&self, keep: &dyn Fn(&str) -> bool) -> Option<Self> {
        match self {
            Self::Pane { panel } => keep(panel).then(|| self.clone()),
            Self::Tabs { children, active } => {
                let kept: Vec<(usize, Self)> =
                    children.iter().enumerate().filter_map(|(i, c)| c.pruned(keep).map(|c| (i, c))).collect();
                let shown = kept.iter().position(|(i, _)| i == active).unwrap_or(0);
                let children: Vec<Self> = kept.into_iter().map(|(_, c)| c).collect();
                match children.len() {
                    0 => None,
                    1 => children.into_iter().next(),
                    _ => Some(Self::Tabs { children, active: shown }),
                }
            }
            Self::Split { dir, children, shares } => {
                let kept: Vec<(usize, Self)> =
                    children.iter().enumerate().filter_map(|(i, c)| c.pruned(keep).map(|c| (i, c))).collect();
                let shares: Vec<f32> = kept.iter().map(|(i, _)| shares.get(*i).copied().unwrap_or(1.0)).collect();
                let children: Vec<Self> = kept.into_iter().map(|(_, c)| c).collect();
                match children.len() {
                    0 => None,
                    1 => children.into_iter().next(),
                    _ => Some(Self::Split { dir: *dir, children, shares }),
                }
            }
        }
    }
}

/// A workspace's layout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    /// The file format version.
    pub version: u32,
    /// The whole layout.
    pub root: Node,
}

/// What a loaded layout needs checking against: which panels exist and which one must always be there.
pub struct Known<'a> {
    /// Whether a panel id is registered.
    pub exists: &'a dyn Fn(&str) -> bool,
    /// The panel that must be in the layout: the workspace's primary panel. A workspace that registered none has no
    /// such requirement.
    pub primary: Option<&'a str>,
}

impl Layout {
    /// A layout with `root`.
    pub const fn new(root: Node) -> Self {
        Self { version: VERSION, root }
    }

    /// Every panel id in the layout, in reading order.
    pub fn panels(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.root.collect(&mut out);
        out
    }

    /// Whether the layout holds `panel`.
    pub fn contains(&self, panel: &str) -> bool {
        self.panels().contains(&panel)
    }

    /// This layout made safe to show: panels that are no longer registered are dropped (and containers left with one
    /// child are flattened), and a layout without the primary panel, or with another version, or with nothing left, is
    /// replaced by `default`.
    #[must_use]
    pub fn repaired(self, known: &Known<'_>, default: &Self) -> Self {
        if self.version != VERSION {
            return default.clone();
        }
        let Some(root) = vetted(&self.root, known.exists) else { return default.clone() };
        let repaired = Self { version: VERSION, root };
        if known.primary.is_none_or(|primary| repaired.contains(primary)) { repaired } else { default.clone() }
    }

    /// The layout a workspace registered as its default, made safe the same way a loaded one is: panels that are not
    /// registered are dropped (the caller reports them), a panel listed twice is drawn once, and shares the docking
    /// widget would panic on are made usable. `None` when the tree is empty after that or holds a node this shell does
    /// not know.
    pub fn from_registered(root: &ApiNode, exists: &dyn Fn(&str) -> bool) -> Option<Self> {
        vetted(&Node::from_api(root)?, exists).map(Self::new)
    }

    /// This layout with `panel` added, for "Show panel" on a panel the layout lacks: as a tab beside `anchor` when that
    /// is in the layout, else as a new column at the left edge of the root (`front`) or the right. The new column
    /// starts as a quarter of the root's shares; the shell raises it to the panel's minimum before it is drawn.
    #[must_use]
    pub fn with_panel(&self, panel: &str, anchor: Option<&str>, front: bool) -> Self {
        if self.contains(panel) {
            return self.clone();
        }
        if let Some(anchor) = anchor.filter(|a| self.contains(a)) {
            return Self::new(self.root.clone().joined(anchor, &[panel]));
        }
        let pane = Node::pane(panel);
        let (mut children, mut shares) = match &self.root {
            Node::Split { dir: Dir::Horizontal, children, shares } => (children.clone(), shares.clone()),
            other => (vec![other.clone()], Vec::new()),
        };
        shares.resize(children.len(), 1.0);
        let share = shares.iter().sum::<f32>() / 4.0;
        if front {
            children.insert(0, pane);
            shares.insert(0, share);
        } else {
            children.push(pane);
            shares.push(share);
        }
        Self::new(Node::Split { dir: Dir::Horizontal, children, shares })
    }

    /// This layout with `panel` moved to the left or right of `primary` (a panel's menu, Move to the left or right, the
    /// keyboard's way to do what a drag does): it joins the outermost column on that side, or becomes a new one there.
    /// Unchanged when the panel is the primary one or is not in the layout.
    #[must_use]
    pub fn moved(&self, panel: &str, left: bool, primary: &str) -> Self {
        if panel == primary || !self.contains(panel) {
            return self.clone();
        }
        let rest = self.without(&[panel]);
        let anchor = match &rest.root {
            Node::Split { dir: Dir::Horizontal, children, .. } => {
                let at = children.iter().position(|c| c.contains(primary));
                let edge = if left { children.first() } else { children.last() };
                let beside = |i: usize| at.is_some_and(|p| if left { i < p } else { i > p });
                let index = if left { 0 } else { children.len().saturating_sub(1) };
                edge.filter(|_| beside(index)).and_then(|c| c.first_panel().map(str::to_owned))
            }
            _ => None,
        };
        rest.with_panel(panel, anchor.as_deref(), left)
    }

    /// Which side of `primary` `panel` is on: `Some(true)` left of it, `Some(false)` right, `None` in its column or not
    /// in a side-by-side split with it.
    pub fn side_of(&self, panel: &str, primary: &str) -> Option<bool> {
        let Node::Split { dir: Dir::Horizontal, children, .. } = &self.root else { return None };
        let at = |p: &str| children.iter().position(|c| c.contains(p));
        let (here, centre) = (at(panel)?, at(primary)?);
        (here != centre).then_some(here < centre)
    }

    /// This layout without `panels`. A layout that would be left empty is returned unchanged.
    #[must_use]
    pub fn without(&self, panels: &[&str]) -> Self {
        self.root.pruned(&|p| !panels.contains(&p)).map_or_else(|| self.clone(), Self::new)
    }

    /// This layout with the root split's children sized in points: a child whose first panel `wanted` gives a width for
    /// takes it, and the others share what is left of `total` equally. A layout whose root is not a split is unchanged.
    #[must_use]
    pub fn with_root_widths(mut self, wanted: &dyn Fn(&str) -> Option<f32>, total: f32) -> Self {
        if let Node::Split { children, shares, .. } = &mut self.root {
            let fixed: Vec<Option<f32>> = children
                .iter()
                .map(|child| {
                    let mut panels = Vec::new();
                    child.collect(&mut panels);
                    panels.first().and_then(|first| wanted(first))
                })
                .collect();
            let flexible = fixed.iter().filter(|w| w.is_none()).count().max(1);
            let rest = (total - fixed.iter().flatten().sum::<f32>()).max(1.0) / flexible as f32;
            *shares = fixed.iter().map(|w| w.unwrap_or(rest)).collect();
        }
        self
    }

    /// This layout with the `moved` panels folded into the tab group of `anchor`, for a window too small to show them
    /// apart (Library beside the Outliner, Agent and History beside the Inspector). Panels the layout lacks are
    /// skipped, and so is the whole fold when `anchor` is missing.
    #[must_use]
    pub fn folded(&self, anchor: &str, moved: &[&str]) -> Self {
        let present: Vec<&str> = moved.iter().copied().filter(|p| *p != anchor && self.contains(p)).collect();
        if present.is_empty() || !self.contains(anchor) {
            return self.clone();
        }
        self.root
            .pruned(&|p| !present.contains(&p))
            .map_or_else(|| self.clone(), |root| Self::new(root.joined(anchor, &present)))
    }

    /// The docking widget's tree for this layout, keyed by `id` (egui keeps its scroll and drag state under it).
    pub fn to_tree(&self, id: impl Into<egui::Id>) -> Tree<PanelId> {
        let mut tiles = Tiles::default();
        let root = insert(&mut tiles, &self.root);
        Tree::new(id, root, tiles)
    }

    /// The layout of a docking tree, as the person left it.
    pub fn from_tree(tree: &Tree<PanelId>) -> Option<Self> {
        let root = tree.root?;
        Some(Self::new(extract(&tree.tiles, root)?))
    }
}

/// `root` with the panels `exists` refuses dropped (and containers left with one child flattened), a panel listed twice
/// kept once (the first in reading order, which `pruned` visits in), and every split's shares made usable.
fn vetted(root: &Node, exists: &dyn Fn(&str) -> bool) -> Option<Node> {
    let seen = RefCell::new(HashSet::new());
    let mut root = root.pruned(&|p| exists(p) && seen.borrow_mut().insert(p.to_owned()))?;
    root.sanitise_shares();
    Some(root)
}

fn insert(tiles: &mut Tiles<PanelId>, node: &Node) -> TileId {
    match node {
        Node::Pane { panel } => tiles.insert_pane(PanelId(panel.clone())),
        Node::Tabs { children, active } => {
            let ids: Vec<TileId> = children.iter().map(|c| insert(tiles, c)).collect();
            let mut tabs = Tabs::new(ids.clone());
            if let Some(shown) = ids.get(*active).or(ids.first()) {
                tabs.set_active(*shown);
            }
            tiles.insert_container(tabs)
        }
        Node::Split { dir, children, shares } => {
            let ids: Vec<TileId> = children.iter().map(|c| insert(tiles, c)).collect();
            let dir = match dir {
                Dir::Horizontal => LinearDir::Horizontal,
                Dir::Vertical => LinearDir::Vertical,
            };
            let mut linear = Linear::new(dir, ids.clone());
            for (i, id) in ids.iter().enumerate() {
                linear.shares.set_share(*id, shares.get(i).copied().unwrap_or(1.0));
            }
            tiles.insert_container(linear)
        }
    }
}

fn extract(tiles: &Tiles<PanelId>, id: TileId) -> Option<Node> {
    match tiles.get(id)? {
        Tile::Pane(PanelId(panel)) => Some(Node::Pane { panel: panel.clone() }),
        Tile::Container(Container::Tabs(tabs)) => {
            let shown = tabs.active;
            let mut children = Vec::new();
            let mut active = 0;
            for child in &tabs.children {
                if let Some(node) = extract(tiles, *child) {
                    if Some(*child) == shown {
                        active = children.len();
                    }
                    children.push(node);
                }
            }
            (!children.is_empty()).then_some(Node::Tabs { children, active })
        }
        Tile::Container(Container::Linear(linear)) => {
            let mut children = Vec::new();
            let mut shares = Vec::new();
            for child in &linear.children {
                if let Some(node) = extract(tiles, *child) {
                    children.push(node);
                    shares.push(linear.shares[*child]);
                }
            }
            let dir = if linear.dir == LinearDir::Horizontal { Dir::Horizontal } else { Dir::Vertical };
            (!children.is_empty()).then_some(Node::Split { dir, children, shares })
        }
        Tile::Container(Container::Grid(grid)) => {
            let children: Vec<Node> = grid.children().filter_map(|c| extract(tiles, *c)).collect();
            let shares = vec![1.0; children.len()];
            (!children.is_empty()).then_some(Node::Split { dir: Dir::Horizontal, children, shares })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Build workspace's shape: Outliner and Library on the left, the viewport over the bottom dock, the Inspector
    /// over Agent and History on the right.
    fn build() -> Layout {
        Layout::new(Node::split(
            Dir::Horizontal,
            vec![
                Node::tabs(&["outliner", "library"]),
                Node::split(Dir::Vertical, vec![Node::pane("viewport"), Node::pane("expressions")], &[3.0, 1.0]),
                Node::split(
                    Dir::Vertical,
                    vec![Node::pane("inspector"), Node::tabs(&["agent", "history"])],
                    &[2.0, 1.0],
                ),
            ],
            &[300.0, 1260.0, 360.0],
        ))
    }

    fn all_exist(_: &str) -> bool {
        true
    }

    #[test]
    fn a_layout_survives_the_trip_through_the_docking_tree_with_its_shares_and_active_tabs() {
        let mut layout = build();
        if let Node::Split { children, .. } = &mut layout.root
            && let Node::Tabs { active, .. } = &mut children[2]
        {
            *active = 0;
        }
        if let Node::Split { children, .. } = &mut layout.root
            && let Node::Split { children: right, .. } = &mut children[2]
            && let Node::Tabs { active, .. } = &mut right[1]
        {
            *active = 1;
        }
        let back = Layout::from_tree(&layout.to_tree("t")).expect("a tree with a root");
        assert_eq!(back, layout);
    }

    #[test]
    fn the_panels_are_listed_in_reading_order() {
        assert_eq!(
            build().panels(),
            ["outliner", "library", "viewport", "expressions", "inspector", "agent", "history"]
        );
    }

    #[test]
    fn a_panel_that_no_longer_exists_is_dropped_and_a_container_left_with_one_child_is_flattened() {
        let known = Known { exists: &|p| p != "library" && p != "expressions", primary: Some("viewport") };
        let repaired = build().repaired(&known, &build());
        assert_eq!(repaired.panels(), ["outliner", "viewport", "inspector", "agent", "history"]);
        let Node::Split { children, shares, .. } = &repaired.root else { panic!("root is a split") };
        assert_eq!(children[0], Node::pane("outliner"), "a tab group of one is just the panel");
        assert_eq!(shares, &[300.0, 1260.0, 360.0], "the other shares are kept");
    }

    #[test]
    fn a_layout_without_the_primary_panel_or_with_another_version_is_replaced_by_the_default() {
        let known = Known { exists: &all_exist, primary: Some("viewport") };
        let default = build();
        let no_viewport = Layout::new(Node::tabs(&["outliner", "inspector"]));
        assert_eq!(no_viewport.repaired(&known, &default), default);
        let mut old = build();
        old.version = 0;
        assert_eq!(old.repaired(&known, &default), default);
        let nothing = Layout::new(Node::tabs(&["gone"]));
        let known_none = Known { exists: &|_| false, primary: Some("viewport") };
        assert_eq!(nothing.repaired(&known_none, &default), default);
    }

    #[test]
    fn a_good_layout_is_returned_as_it_was() {
        let known = Known { exists: &all_exist, primary: Some("viewport") };
        assert_eq!(build().repaired(&known, &Layout::new(Node::pane("viewport"))), build());
    }

    #[test]
    fn bad_shares_are_made_usable() {
        let known = Known { exists: &all_exist, primary: Some("viewport") };
        let bad = Layout::new(Node::split(
            Dir::Horizontal,
            vec![Node::pane("outliner"), Node::pane("viewport"), Node::pane("inspector")],
            &[f32::NAN, 600.0, -4.0],
        ));
        let fixed = bad.repaired(&known, &Layout::new(Node::pane("viewport")));
        let Node::Split { shares, .. } = &fixed.root else { panic!("still a split") };
        assert_eq!(shares, &[600.0, 600.0, 600.0]);
        // Zero, infinite and missing entries too, and extras are dropped.
        let odd = Layout::new(Node::Split {
            dir: Dir::Vertical,
            children: vec![Node::pane("viewport"), Node::pane("inspector")],
            shares: vec![0.0, f32::INFINITY, 5.0],
        });
        let Node::Split { shares, .. } = odd.repaired(&known, &Layout::new(Node::pane("viewport"))).root else {
            panic!("still a split")
        };
        assert_eq!(shares, [1.0, 1.0]);
        let sparse = Layout::new(Node::Split {
            dir: Dir::Vertical,
            children: vec![Node::pane("viewport"), Node::pane("inspector")],
            shares: vec![2.0],
        });
        let Node::Split { shares, .. } = sparse.repaired(&known, &Layout::new(Node::pane("viewport"))).root else {
            panic!("still a split")
        };
        assert_eq!(shares, [2.0, 1.0]);
        // The docking widget accepts the result.
        fixed.to_tree("t");
    }

    #[test]
    fn folding_joins_panels_to_an_anchors_tab_group_and_a_lone_anchor_becomes_a_group() {
        let folded = build().folded("inspector", &["agent", "history"]);
        assert_eq!(
            folded.panels(),
            ["outliner", "library", "viewport", "expressions", "inspector", "agent", "history"]
        );
        let Node::Split { children, shares, .. } = &folded.root else { panic!("root is a split") };
        // The old Agent and History group is gone, and the split left holding only the Inspector is flattened.
        assert_eq!(children[2], Node::tabs(&["inspector", "agent", "history"]));
        assert_eq!(shares.len(), 3);
        // An anchor that already sits in a group takes the others into it.
        let library = build().folded("outliner", &["library", "history"]);
        assert_eq!(
            library.panels(),
            ["outliner", "library", "history", "viewport", "expressions", "inspector", "agent"]
        );
        // Nothing to join, or no anchor: unchanged.
        assert_eq!(build().folded("inspector", &["nothing"]), build());
        assert_eq!(build().folded("nothing", &["agent"]), build());
    }

    #[test]
    fn root_widths_give_docks_their_points_and_share_the_rest() {
        let wanted = |p: &str| match p {
            "outliner" => Some(240.0),
            "inspector" => Some(300.0),
            _ => None,
        };
        let sized = build().with_root_widths(&wanted, 1366.0);
        let Node::Split { shares, .. } = &sized.root else { panic!("root is a split") };
        assert_eq!(shares, &[240.0, 826.0, 300.0]);
        let one = Layout::new(Node::pane("viewport"));
        assert_eq!(one.clone().with_root_widths(&wanted, 1366.0), one);
    }

    #[test]
    fn without_drops_panels_but_never_empties_the_layout() {
        assert_eq!(
            build().without(&["outliner", "library"]).panels(),
            ["viewport", "expressions", "inspector", "agent", "history"]
        );
        let one = Layout::new(Node::pane("viewport"));
        assert_eq!(one.without(&["viewport"]), one);
    }

    #[test]
    fn a_registered_default_is_converted_node_for_node() {
        use omuzaki_ui_api::layout::{Dir as ApiDir, Node as ApiNode};
        let api = ApiNode::split(
            ApiDir::Vertical,
            vec![ApiNode::tabs(&["outliner", "library"]), ApiNode::pane("viewport")],
            &[2.0, 3.0],
        );
        let expected =
            Node::split(Dir::Vertical, vec![Node::tabs(&["outliner", "library"]), Node::pane("viewport")], &[2.0, 3.0]);
        assert_eq!(Node::from_api(&api), Some(expected));
    }

    #[test]
    fn a_registered_default_loses_unknown_and_repeated_panels_and_gets_usable_shares() {
        use omuzaki_ui_api::layout::{Dir as ApiDir, Node as ApiNode};
        let api = ApiNode::split(
            ApiDir::Vertical,
            vec![ApiNode::tabs(&["outliner", "library", "gone"]), ApiNode::pane("viewport"), ApiNode::pane("viewport")],
            &[f32::NAN, 3.0],
        );
        let layout = Layout::from_registered(&api, &|p| p != "gone").expect("something is left");
        assert_eq!(layout.panels(), ["outliner", "library", "viewport"]);
        let Node::Split { dir, shares, .. } = &layout.root else { panic!("still a split") };
        assert_eq!(
            (*dir, shares.as_slice()),
            (Dir::Vertical, &[3.0, 3.0][..]),
            "the NaN share is the mean of the rest"
        );
        assert!(Layout::from_registered(&ApiNode::pane("gone"), &|p| p != "gone").is_none());
        layout.to_tree("t");
    }

    #[test]
    fn a_layout_is_not_held_to_a_primary_panel_when_its_workspace_has_none() {
        let known = Known { exists: &all_exist, primary: None };
        let tabs = Layout::new(Node::tabs(&["outliner", "inspector"]));
        assert_eq!(tabs.clone().repaired(&known, &Layout::new(Node::pane("inspector"))), tabs);
    }

    #[test]
    fn showing_a_panel_the_layout_lacks_joins_a_group_or_adds_a_column() {
        // Beside an anchor that is in the layout: a lone anchor becomes a group, an anchor in a group takes it in.
        let beside = build().with_panel("notes", Some("inspector"), false);
        assert_eq!(
            beside.panels(),
            ["outliner", "library", "viewport", "expressions", "inspector", "notes", "agent", "history"]
        );
        let tabbed = build().with_panel("notes", Some("agent"), false);
        assert!(tabbed.panels().ends_with(&["agent", "history", "notes"]), "{:?}", tabbed.panels());
        // No anchor: a column at the left edge or the right, a quarter of the shares in total.
        let Node::Split { children, shares, .. } = &build().with_panel("notes", None, true).root else {
            panic!("split")
        };
        assert_eq!((children[0].clone(), shares[0]), (Node::pane("notes"), (300.0 + 1260.0 + 360.0) / 4.0));
        let Node::Split { children, .. } = &build().with_panel("notes", Some("not in the layout"), false).root else {
            panic!("split")
        };
        assert_eq!(children.last(), Some(&Node::pane("notes")));
        // A root that is not a horizontal split is wrapped.
        let one = Layout::new(Node::pane("viewport")).with_panel("notes", None, false);
        assert_eq!(one.panels(), ["viewport", "notes"]);
        // Already there: unchanged.
        assert_eq!(build().with_panel("viewport", None, true), build());
        build().with_panel("notes", None, false).to_tree("t");
    }

    #[test]
    fn moving_a_panel_puts_it_in_the_outermost_column_on_that_side_or_a_new_one() {
        let layout = build();
        let right = layout.moved("outliner", false, "viewport");
        let Node::Split { children, .. } = &right.root else { panic!("a split") };
        assert!(children.last().is_some_and(|c| c.contains("outliner") && c.contains("inspector")), "{right:?}");
        assert!(children.first().is_some_and(|c| !c.contains("outliner")), "{right:?}");
        // The only panel on the left moves away: the right column takes it, and moving it back makes a new column.
        let alone = Layout::new(Node::split(
            Dir::Horizontal,
            vec![Node::pane("outliner"), Node::pane("viewport"), Node::pane("inspector")],
            &[1.0, 3.0, 1.0],
        ));
        let away = alone.moved("outliner", false, "viewport");
        assert_eq!(away.panels(), ["viewport", "inspector", "outliner"]);
        let back = away.moved("outliner", true, "viewport");
        assert_eq!(back.panels(), ["outliner", "viewport", "inspector"]);
        assert_eq!(layout.side_of("library", "viewport"), Some(true));
        assert_eq!(right.side_of("outliner", "viewport"), Some(false));
        assert_eq!(layout.side_of("expressions", "viewport"), None, "below the viewport, in its column");
        assert_eq!(layout.moved("viewport", true, "viewport"), layout, "the primary panel stays");
        assert_eq!(layout.moved("nowhere", true, "viewport"), layout);
    }

    #[test]
    fn a_panel_listed_twice_is_drawn_once() {
        let known = Known { exists: &all_exist, primary: Some("viewport") };
        let twice = Layout::new(Node::split(
            Dir::Horizontal,
            vec![Node::tabs(&["outliner", "library"]), Node::pane("viewport"), Node::tabs(&["library", "outliner"])],
            &[1.0, 2.0, 1.0],
        ));
        let fixed = twice.repaired(&known, &Layout::new(Node::pane("viewport")));
        assert_eq!(fixed.panels(), ["outliner", "library", "viewport"]);
        let Node::Split { children, shares, .. } = &fixed.root else { panic!("still a split") };
        assert_eq!((children.len(), shares.len()), (2, 2));
    }
}
