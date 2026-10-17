//! The tree a workspace's default layout is written in (`shell.md` section 4).
//!
//! It names panels by their registered ids and knows nothing of the docking widget, so a saved layout survives a
//! change of widget. The shell converts it to the widget's tree and repairs it (drops unknown panels, keeps the
//! primary one) before showing it.

use serde::{Deserialize, Serialize};

/// Which way a split runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Dir {
    /// Children side by side.
    Horizontal,
    /// Children stacked.
    Vertical,
}

/// One node of a layout. Build it with [`Node::pane`], [`Node::tabs`] and [`Node::split`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Node {
    /// A panel.
    #[non_exhaustive]
    Pane {
        /// Its registered id.
        panel: String,
    },
    /// A tab group; `active` indexes `children`.
    #[non_exhaustive]
    Tabs {
        /// The tabs, in order.
        children: Vec<Node>,
        /// The shown one.
        #[serde(default)]
        active: usize,
    },
    /// A split with a relative share per child.
    #[non_exhaustive]
    Split {
        /// Which way it runs.
        dir: Dir,
        /// The parts, in order.
        children: Vec<Node>,
        /// How much of the space each child takes, relative to the others; a missing entry counts as 1.
        #[serde(default)]
        shares: Vec<f32>,
    },
}

impl Node {
    /// The panel registered as `panel`.
    pub fn pane(panel: &str) -> Self {
        Self::Pane { panel: panel.to_owned() }
    }

    /// A tab group of `panels`, showing the first.
    pub fn tabs(panels: &[&str]) -> Self {
        Self::Tabs { children: panels.iter().map(|p| Self::pane(p)).collect(), active: 0 }
    }

    /// A split of `children`, each taking its entry of `shares` (a missing entry counts as 1).
    pub fn split(dir: Dir, children: Vec<Self>, shares: &[f32]) -> Self {
        Self::Split { dir, children, shares: shares.to_vec() }
    }

    /// Every panel id in the tree, depth first, in order.
    pub fn panels(&self) -> Vec<&str> {
        let mut out = Vec::new();
        self.collect(&mut out);
        out
    }

    fn collect<'a>(&'a self, out: &mut Vec<&'a str>) {
        match self {
            Self::Pane { panel } => out.push(panel),
            Self::Tabs { children, .. } | Self::Split { children, .. } => children.iter().for_each(|c| c.collect(out)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panels_are_listed_depth_first_in_order() {
        let tree = Node::split(
            Dir::Horizontal,
            vec![Node::tabs(&["outliner", "library"]), Node::pane("viewport"), Node::pane("inspector")],
            &[1.0, 3.0],
        );
        assert_eq!(tree.panels(), ["outliner", "library", "viewport", "inspector"]);
    }
}
