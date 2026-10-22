//! The part tree: a character's parts with their parents, read from its `CharacterDef` once per document change.
//!
//! Siblings are in build order: by `layer`, then by id (the order `part.reorder` changes). A part whose parent is
//! missing (a document saved by another build) is listed at the top level rather than dropped.

use std::collections::BTreeMap;

use omuzaki_char_api::{CharacterDef, PartCategory, PartId, find_kind};

use crate::ds::icons;

/// One part in the tree.
#[derive(Clone, Debug)]
pub struct Node {
    /// The part's id, which is also its name in the tree.
    pub id: PartId,
    /// Its kind's id.
    pub kind: String,
    /// The type icon, from the kind's category.
    pub icon: &'static str,
    /// Its parent's index, when it has one in the tree.
    pub parent: Option<usize>,
    /// Its children's indices, in build order.
    pub children: Vec<usize>,
    /// Whether it is built (`part.set_enabled`).
    pub enabled: bool,
    /// Its layer (`part.reorder`).
    pub layer: i32,
    /// Its stored parameters fail its kind's check (the warning triangle).
    pub invalid: bool,
    /// How deep it is: 0 at the top level.
    pub depth: usize,
}

/// The parts of one character, as a tree.
#[derive(Clone, Debug, Default)]
pub struct Tree {
    nodes: Vec<Node>,
    roots: Vec<usize>,
    /// Each part's index, so a lookup by id is not a walk of the tree.
    index: BTreeMap<PartId, usize>,
}

impl Tree {
    /// The tree of `def`'s parts.
    pub fn of(def: &CharacterDef) -> Self {
        let index: BTreeMap<&PartId, usize> = def.parts.keys().enumerate().map(|(i, id)| (id, i)).collect();
        let mut nodes: Vec<Node> = def
            .parts
            .iter()
            .map(|(id, part)| {
                let kind = find_kind(part.kind.as_str());
                Node {
                    id: id.clone(),
                    kind: part.kind.as_str().to_owned(),
                    icon: kind.map_or(icons::CUBE, |k| icon_of((k.meta)().category)),
                    parent: part
                        .parent
                        .as_ref()
                        .and_then(|p| index.get(p).copied())
                        .filter(|p| index.get(id) != Some(p)),
                    children: Vec::new(),
                    enabled: part.enabled,
                    layer: part.layer,
                    invalid: kind.is_some_and(|k| (k.check)(&part.params).is_err()),
                    depth: 0,
                }
            })
            .collect();
        let order = |nodes: &[Node], list: &mut Vec<usize>| {
            list.sort_by(|&a, &b| (nodes[a].layer, &nodes[a].id).cmp(&(nodes[b].layer, &nodes[b].id)));
        };
        let mut roots = Vec::new();
        for i in 0..nodes.len() {
            match nodes[i].parent {
                Some(p) => nodes[p].children.push(i),
                None => roots.push(i),
            }
        }
        order(&nodes, &mut roots);
        for i in 0..nodes.len() {
            let mut children = std::mem::take(&mut nodes[i].children);
            order(&nodes, &mut children);
            nodes[i].children = children;
        }
        let index = nodes.iter().enumerate().map(|(i, n)| (n.id.clone(), i)).collect();
        let mut tree = Self { nodes, roots, index };
        // A cycle of parents (no root reaches it) is broken at its first part, so every part is listed. Each pass makes
        // one more root, so this ends.
        while let Some(i) = tree.set_depths().iter().position(|reached| !reached) {
            if let Some(p) = tree.nodes[i].parent.take() {
                tree.nodes[p].children.retain(|&c| c != i);
            }
            tree.roots.push(i);
            order(&tree.nodes, &mut tree.roots);
        }
        tree
    }

    /// Walks from the roots setting depths; returns which nodes were reached.
    fn set_depths(&mut self) -> Vec<bool> {
        let mut reached = vec![false; self.nodes.len()];
        let mut stack: Vec<(usize, usize)> = self.roots.iter().rev().map(|&r| (r, 0)).collect();
        while let Some((i, depth)) = stack.pop() {
            if std::mem::replace(&mut reached[i], true) {
                continue;
            }
            self.nodes[i].depth = depth;
            stack.extend(self.nodes[i].children.iter().rev().map(|&c| (c, depth + 1)));
        }
        reached
    }

    /// Every part, in no particular order (index with [`Tree::node`]).
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// The part at `index`.
    pub fn node(&self, index: usize) -> Option<&Node> {
        self.nodes.get(index)
    }

    /// The top-level parts, in build order.
    pub fn roots(&self) -> &[usize] {
        &self.roots
    }

    /// The index of part `id`.
    pub fn find(&self, id: &PartId) -> Option<usize> {
        self.index.get(id).copied()
    }

    /// The parts beside `index` (its parent's children, or the roots), in build order.
    pub fn siblings(&self, index: usize) -> &[usize] {
        match self.nodes.get(index).and_then(|n| n.parent) {
            Some(p) => &self.nodes[p].children,
            None => &self.roots,
        }
    }

    /// How many parts there are.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether there are no parts.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// The type icon for a kind's category.
const fn icon_of(category: PartCategory) -> &'static str {
    match category {
        PartCategory::Body => icons::USER,
        PartCategory::Head => icons::SMILEY,
        PartCategory::Hair => icons::HAIR_DRYER,
        PartCategory::Garment | PartCategory::Footwear => icons::T_SHIRT,
        PartCategory::Accessory => icons::CUBE,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[allow(clippy::needless_pass_by_value, reason = "the tests pass a `json!` literal")]
    fn def(parts: serde_json::Value) -> CharacterDef {
        CharacterDef::from_node(&json!({"name": "Test", "template": "mock", "parts": parts}))
            .unwrap_or_else(|e| panic!("a character: {e}"))
    }

    fn names(tree: &Tree, list: &[usize]) -> Vec<String> {
        list.iter().map(|&i| tree.nodes()[i].id.to_string()).collect()
    }

    #[test]
    fn siblings_are_in_build_order_and_children_sit_under_their_parent() {
        let tree = Tree::of(&def(json!({
            "vest": {"kind": "mock-garment", "layer": 30},
            "body": {"kind": "mock-body"},
            "shirt": {"kind": "mock-garment", "layer": 20},
            "hood": {"kind": "mock-garment", "parent": "vest", "layer": 40},
            "clasp": {"kind": "mock-garment", "parent": "vest", "layer": 40}
        })));
        assert_eq!(names(&tree, tree.roots()), ["body", "shirt", "vest"]);
        let vest = tree.find(&PartId::new("vest").unwrap_or_else(|_| panic!("id"))).unwrap_or(usize::MAX);
        assert_eq!(names(&tree, &tree.nodes()[vest].children), ["clasp", "hood"]);
        assert_eq!(tree.nodes()[tree.nodes()[vest].children[0]].depth, 1);
    }

    #[test]
    fn a_missing_parent_or_a_cycle_still_lists_every_part() {
        let tree = Tree::of(&def(json!({
            "a": {"kind": "mock-garment", "parent": "b"},
            "b": {"kind": "mock-garment", "parent": "a"},
            "c": {"kind": "mock-garment", "parent": "gone"}
        })));
        let mut seen = vec![false; tree.len()];
        let mut stack: Vec<usize> = tree.roots().to_vec();
        while let Some(i) = stack.pop() {
            seen[i] = true;
            stack.extend(&tree.nodes()[i].children);
        }
        assert!(seen.iter().all(|s| *s), "{tree:?}");
    }
}
