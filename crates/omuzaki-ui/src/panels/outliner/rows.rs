//! The flat list of visible rows, cached on (tree build, expanded set, filter) so a frame only draws the rows on screen:
//! a 10,000-part tree costs what a 40-part one does (`shell.md` section 6, `design-system.md` section 9 rule 3).

use std::collections::BTreeSet;

use omuzaki_char_api::PartId;

use super::tree::Tree;
use crate::ds::widgets::Expand;

/// One visible row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Row {
    /// The part's index in the tree.
    pub node: usize,
    /// Whether it has children and whether they show.
    pub expand: Expand,
    /// The byte range of its name the filter matched.
    pub matched: Option<(usize, usize)>,
}

/// Which parts are collapsed. Parts start expanded; a collapsed part stays collapsed across rebuilds, by id.
#[derive(Clone, Debug, Default)]
pub struct Collapsed {
    parts: BTreeSet<PartId>,
    generation: u64,
}

impl Collapsed {
    /// Whether `part` is collapsed.
    pub fn contains(&self, part: &PartId) -> bool {
        self.parts.contains(part)
    }

    /// Collapses or expands `part`; true when that changed something.
    pub fn set(&mut self, part: &PartId, collapsed: bool) -> bool {
        let changed = if collapsed { self.parts.insert(part.clone()) } else { self.parts.remove(part) };
        if changed {
            self.generation += 1;
        }
        changed
    }
}

/// What the cached rows were made from.
#[derive(Clone, Debug, Default, PartialEq)]
struct Key {
    tree: u64,
    collapsed: u64,
    filter: String,
}

/// The visible rows, rebuilt only when what they are made from changes.
#[derive(Clone, Debug, Default)]
pub struct Rows {
    key: Option<Key>,
    rows: Vec<Row>,
}

impl Rows {
    /// The rows of `tree` (its build counted by `generation`) with `collapsed` parts closed and only the parts that
    /// match `filter` (and their ancestors) shown. Allocates only when one of those changed.
    pub fn get(&mut self, tree: &Tree, generation: u64, collapsed: &Collapsed, filter: &str) -> &[Row] {
        let fresh = self
            .key
            .as_ref()
            .is_some_and(|k| k.tree == generation && k.collapsed == collapsed.generation && k.filter == filter);
        if !fresh {
            self.rows.clear();
            if filter.is_empty() {
                visible(tree, collapsed, &mut self.rows);
            } else {
                filtered(tree, filter, &mut self.rows);
            }
            let key = self.key.get_or_insert_with(Key::default);
            key.tree = generation;
            key.collapsed = collapsed.generation;
            key.filter.clear();
            key.filter.push_str(filter);
        }
        &self.rows
    }
}

/// Every part whose ancestors are all expanded, in tree order.
fn visible(tree: &Tree, collapsed: &Collapsed, out: &mut Vec<Row>) {
    let mut stack: Vec<usize> = tree.roots().iter().rev().copied().collect();
    while let Some(i) = stack.pop() {
        let Some(node) = tree.node(i) else { continue };
        let expand = match (node.children.is_empty(), collapsed.contains(&node.id)) {
            (true, _) => Expand::Leaf,
            (false, true) => Expand::Closed,
            (false, false) => Expand::Open,
        };
        out.push(Row { node: i, expand, matched: None });
        if expand == Expand::Open {
            stack.extend(node.children.iter().rev());
        }
    }
}

/// The parts that match `filter`, with their ancestors open so each match shows where it is.
fn filtered(tree: &Tree, filter: &str, out: &mut Vec<Row>) {
    let matches: Vec<Option<(usize, usize)>> = tree.nodes().iter().map(|n| fuzzy(n.id.as_str(), filter)).collect();
    let mut keep: Vec<bool> = matches.iter().map(Option::is_some).collect();
    for i in 0..tree.len() {
        if !keep[i] || matches[i].is_none() {
            continue;
        }
        let mut up = tree.node(i).and_then(|n| n.parent);
        while let Some(p) = up {
            // Already kept: its ancestors are kept too, or will be when its own match is walked.
            if std::mem::replace(&mut keep[p], true) {
                break;
            }
            up = tree.node(p).and_then(|n| n.parent);
        }
    }
    let mut stack: Vec<usize> = tree.roots().iter().rev().copied().collect();
    while let Some(i) = stack.pop() {
        let Some(node) = tree.node(i) else { continue };
        if !keep[i] {
            continue;
        }
        let shown_children = node.children.iter().any(|&c| keep[c]);
        let expand = match (node.children.is_empty(), shown_children) {
            (true, _) => Expand::Leaf,
            (false, false) => Expand::Closed,
            (false, true) => Expand::Open,
        };
        out.push(Row { node: i, expand, matched: matches[i] });
        stack.extend(node.children.iter().rev().filter(|&&c| keep[c]));
    }
}

/// Where `query` matches `name`, ignoring case: as a run of characters when it can, else as characters in order with
/// gaps (fuzzy). The range covers the match, as byte offsets into `name`.
pub fn fuzzy(name: &str, query: &str) -> Option<(usize, usize)> {
    if query.is_empty() {
        return None;
    }
    let lower = |c: char| c.to_lowercase().next().unwrap_or(c);
    // A run: compare character by character, so no lower-cased copy is made.
    for (start, _) in name.char_indices() {
        let mut rest = name[start..].char_indices();
        let mut end = start;
        let run = query.chars().all(|q| match rest.next() {
            Some((at, c)) if lower(c) == lower(q) => {
                end = start + at + c.len_utf8();
                true
            }
            _ => false,
        });
        if run {
            return Some((start, end));
        }
    }
    let mut wanted = query.chars().map(lower).peekable();
    let (mut first, mut last) = (None, 0);
    for (at, c) in name.char_indices() {
        if wanted.peek().is_some_and(|&q| q == lower(c)) {
            wanted.next();
            first.get_or_insert(at);
            last = at + c.len_utf8();
        }
    }
    if wanted.peek().is_some() { None } else { first.map(|f| (f, last)) }
}

#[cfg(test)]
mod tests {
    use omuzaki_char_api::CharacterDef;
    use serde_json::json;

    use super::*;

    fn tree() -> Tree {
        Tree::of(
            &CharacterDef::from_node(&json!({"name": "T", "template": "mock", "parts": {
                "body": {"kind": "mock-body"},
                "vest": {"kind": "mock-garment", "layer": 30},
                "hood": {"kind": "mock-garment", "parent": "vest"},
                "clasp-left": {"kind": "mock-garment", "parent": "vest"}
            }}))
            .unwrap_or_else(|e| panic!("{e}")),
        )
    }

    fn names(tree: &Tree, rows: &[Row]) -> Vec<String> {
        rows.iter().map(|r| tree.nodes()[r.node].id.to_string()).collect()
    }

    #[test]
    fn rows_follow_the_collapsed_set_and_are_cached() {
        let tree = tree();
        let (mut rows, mut collapsed) = (Rows::default(), Collapsed::default());
        assert_eq!(names(&tree, rows.get(&tree, 1, &collapsed, "")), ["body", "vest", "clasp-left", "hood"]);
        let vest = PartId::new("vest").unwrap_or_else(|_| panic!("id"));
        assert!(collapsed.set(&vest, true));
        assert_eq!(names(&tree, rows.get(&tree, 1, &collapsed, "")), ["body", "vest"]);
        assert_eq!(rows.get(&tree, 1, &collapsed, "")[1].expand, Expand::Closed);
        assert!(!collapsed.set(&vest, true), "already collapsed");
    }

    #[test]
    fn a_filter_keeps_ancestors_and_marks_the_match() {
        let tree = tree();
        let (mut rows, mut collapsed) = (Rows::default(), Collapsed::default());
        collapsed.set(&PartId::new("vest").unwrap_or_else(|_| panic!("id")), true);
        let found = rows.get(&tree, 1, &collapsed, "HOO").to_vec();
        assert_eq!(names(&tree, &found), ["vest", "hood"], "the ancestor shows even though it was collapsed");
        assert_eq!((found[0].matched, found[1].matched), (None, Some((0, 3))));
        assert_eq!(names(&tree, rows.get(&tree, 1, &collapsed, "cl")), ["vest", "clasp-left"]);
    }

    #[test]
    fn fuzzy_prefers_a_run_then_takes_characters_in_order() {
        assert_eq!(fuzzy("clasp-left", "left"), Some((6, 10)));
        assert_eq!(fuzzy("clasp-left", "clt"), Some((0, 10)));
        assert_eq!(fuzzy("Hair", "AI"), Some((1, 3)));
        assert_eq!(fuzzy("hair", "z"), None);
        assert_eq!(fuzzy("hair", ""), None);
    }
}
