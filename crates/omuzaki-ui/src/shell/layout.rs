//! A workspace's layout (`shell.md` section 4): a small tree of panel ids, saved per person.
//!
//! The tree is ours, not the docking widget's: it is what is written to disk, so a change inside `egui_tiles` can never
//! make a saved file unreadable. [`Layout::to_tree`] and [`Layout::from_tree`] convert to and from the widget's tree,
//! and [`Layout::repaired`] makes a loaded layout safe (a panel that no longer exists is dropped, the primary panel is
//! always there) before it is shown.

use std::cell::RefCell;
use std::collections::HashSet;

use egui_tiles::{Container, Linear, LinearDir, Tabs, Tile, TileId, Tiles, Tree};
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
    /// The panel that must be in the layout: the workspace's primary panel.
    pub primary: &'a str,
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
        // A panel can be drawn once: the first of a duplicate wins (`pruned` visits panes in reading order).
        let seen = RefCell::new(HashSet::new());
        let kept = self.root.pruned(&|p| (known.exists)(p) && seen.borrow_mut().insert(p.to_owned()));
        let Some(mut root) = kept else { return default.clone() };
        root.sanitise_shares();
        let repaired = Self { version: VERSION, root };
        if repaired.contains(known.primary) { repaired } else { default.clone() }
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
        let known = Known { exists: &|p| p != "library" && p != "expressions", primary: "viewport" };
        let repaired = build().repaired(&known, &build());
        assert_eq!(repaired.panels(), ["outliner", "viewport", "inspector", "agent", "history"]);
        let Node::Split { children, shares, .. } = &repaired.root else { panic!("root is a split") };
        assert_eq!(children[0], Node::pane("outliner"), "a tab group of one is just the panel");
        assert_eq!(shares, &[300.0, 1260.0, 360.0], "the other shares are kept");
    }

    #[test]
    fn a_layout_without_the_primary_panel_or_with_another_version_is_replaced_by_the_default() {
        let known = Known { exists: &all_exist, primary: "viewport" };
        let default = build();
        let no_viewport = Layout::new(Node::tabs(&["outliner", "inspector"]));
        assert_eq!(no_viewport.repaired(&known, &default), default);
        let mut old = build();
        old.version = 0;
        assert_eq!(old.repaired(&known, &default), default);
        let nothing = Layout::new(Node::tabs(&["gone"]));
        let known_none = Known { exists: &|_| false, primary: "viewport" };
        assert_eq!(nothing.repaired(&known_none, &default), default);
    }

    #[test]
    fn a_good_layout_is_returned_as_it_was() {
        let known = Known { exists: &all_exist, primary: "viewport" };
        assert_eq!(build().repaired(&known, &Layout::new(Node::pane("viewport"))), build());
    }

    #[test]
    fn bad_shares_are_made_usable() {
        let known = Known { exists: &all_exist, primary: "viewport" };
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
    fn a_panel_listed_twice_is_drawn_once() {
        let known = Known { exists: &all_exist, primary: "viewport" };
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
