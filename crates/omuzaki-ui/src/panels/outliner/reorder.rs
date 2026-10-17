//! Which layers put a part where the person dropped it, and the order to apply them in.
//!
//! Siblings are listed by `(layer, id)`, so moving a part among them means changing layers. But a layer is more than a
//! sort key: a part builds after *every* part with a lower layer, after its parent and after the kinds it requires
//! ([`CharacterDef::build_order`]). A layer that gives the right order among siblings can still contradict one of those
//! rules (a child ends up below its parent, a hair below the head it needs), and `part.reorder` refuses it, so a plan
//! is only offered when the real build order accepts it, and it is applied in an order every step of which the build
//! order accepts too (the ops are run one at a time, and each is checked on its own).
//!
//! A plan is the first of these that works, fewest disturbances first:
//! 1. **in place**: only the moved part changes layer, to one that fits between its new neighbours;
//! 2. **forward**: the moved part takes a layer just past the one before it, and the parts after it are pushed along
//!    as far as they must be;
//! 3. **backward**: the parts before the moved part are pulled down instead.
//!
//! When the character is not buildable to begin with (a dangling parent, say) nothing can be checked, so the first plan
//! is returned and the ops decide.

use omuzaki_char_api::{CharacterDef, PartId, requires_of};
use omuzaki_ops_api::OpError;

use super::tree::Tree;

/// A part (its index in the [`Tree`]) and the layer it takes.
pub(super) type Change = (usize, i32);

/// The layer changes, in the order to run them, that make `order` (sibling indices, with the part at `moved` in its
/// new place) the build order of those siblings. Empty when `order` already is.
///
/// # Errors
/// A layer would pass the end of `i32`.
pub(super) fn plan(def: &CharacterDef, tree: &Tree, order: &[usize], moved: usize) -> Result<Vec<Change>, OpError> {
    let forward = forward(tree, order)?;
    if forward.is_empty() {
        return Ok(forward);
    }
    // In place first; it is empty only when the part already sorts there, which `forward` would have said.
    let mut candidates: Vec<Vec<Change>> = in_place(tree, order, moved).into_iter().collect();
    candidates.push(forward);
    candidates.extend(backward(tree, order));
    if def.build_order(&requires_of).is_ok() {
        for candidate in &candidates {
            let mut reversed = candidate.clone();
            reversed.reverse();
            for sequence in [candidate, &reversed] {
                if every_step_builds(def, tree, sequence) {
                    return Ok(sequence.clone());
                }
            }
        }
    }
    // No plan is known to work (or nothing can be checked): the first one, and the ops say what they refuse.
    Ok(candidates.swap_remove(0))
}

/// Whether the character still has a build order after each of `changes` in turn.
fn every_step_builds(def: &CharacterDef, tree: &Tree, changes: &[Change]) -> bool {
    let mut after = def.clone();
    changes.iter().all(|&(node, layer)| {
        let Some(part) = tree.node(node).and_then(|n| after.parts.get_mut(&n.id)) else { return false };
        part.layer = layer;
        after.build_order(&requires_of).is_ok()
    })
}

fn key(tree: &Tree, node: usize) -> Option<(i32, &PartId)> {
    tree.node(node).map(|n| (n.layer, &n.id))
}

/// Only the part at `moved` changes layer, to a layer its neighbours have or one either side of theirs that sorts it
/// between them. `None` when there is none.
fn in_place(tree: &Tree, order: &[usize], moved: usize) -> Option<Vec<Change>> {
    let node = *order.get(moved)?;
    let (own, id) = key(tree, node)?;
    let before = moved.checked_sub(1).and_then(|i| key(tree, order[i]));
    let after = order.get(moved + 1).and_then(|&n| key(tree, n));
    let fits = |layer: i32| before.is_none_or(|b| (layer, id) > b) && after.is_none_or(|a| (layer, id) < a);
    let mut tries = Vec::with_capacity(4);
    if let Some((layer, _)) = before {
        tries.extend([Some(layer), layer.checked_add(1)]);
    }
    if let Some((layer, _)) = after {
        tries.extend([Some(layer), layer.checked_sub(1)]);
    }
    // Of the layers that fit, the one nearest the part's own: it moves the least from where the person put it.
    let layer = tries.into_iter().flatten().filter(|&l| fits(l)).min_by_key(|&l| (l.abs_diff(own), l))?;
    Some(vec![(node, layer)])
}

/// Walks `order` from the front: a part that already sorts after the one before it keeps its layer; any other takes
/// the layer of the one before it if its id sorts after that one's, else the layer just past it.
fn forward(tree: &Tree, order: &[usize]) -> Result<Vec<Change>, OpError> {
    let mut changes = Vec::new();
    let mut before: Option<(i32, &PartId)> = None;
    for &node in order {
        let Some((own, id)) = key(tree, node) else { continue };
        let layer = match before {
            Some(b) if (own, id) <= b => {
                let layer = if id > b.1 { b.0 } else { b.0.checked_add(1).ok_or_else(past_the_end)? };
                changes.push((node, layer));
                layer
            }
            _ => own,
        };
        before = Some((layer, id));
    }
    Ok(changes)
}

/// The mirror of [`forward`], from the back: parts are pulled down. `None` when a layer would pass the end of `i32`.
fn backward(tree: &Tree, order: &[usize]) -> Option<Vec<Change>> {
    let mut changes = Vec::new();
    let mut after: Option<(i32, &PartId)> = None;
    for &node in order.iter().rev() {
        let (own, id) = key(tree, node)?;
        let layer = match after {
            Some(a) if (own, id) >= a => {
                let layer = if id < a.1 { a.0 } else { a.0.checked_sub(1)? };
                changes.push((node, layer));
                layer
            }
            _ => own,
        };
        after = Some((layer, id));
    }
    changes.reverse();
    Some(changes)
}

fn past_the_end() -> OpError {
    OpError::invalid("layer", "no layer is left after the last one")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[allow(clippy::needless_pass_by_value, reason = "the tests pass a `json!` literal")]
    fn def(parts: serde_json::Value) -> CharacterDef {
        omuzaki_char::link();
        omuzaki_char_mock::link();
        CharacterDef::from_node(&json!({"name": "Test", "template": "mock", "parts": parts}))
            .unwrap_or_else(|e| panic!("a character: {e}"))
    }

    fn node(tree: &Tree, id: &str) -> usize {
        PartId::new(id).ok().and_then(|id| tree.find(&id)).unwrap_or(usize::MAX)
    }

    fn layers(tree: &Tree, changes: &[Change]) -> Vec<(String, i32)> {
        changes.iter().map(|&(n, l)| (tree.nodes()[n].id.to_string(), l)).collect()
    }

    #[test]
    fn a_free_gap_in_the_moved_parts_own_layer_is_taken_before_anything_is_pushed() {
        // vest (30) above shirt (20); hood (20) is the shirt's child, so raising the shirt would put it under its child.
        let d = def(json!({
            "body": {"kind": "mock-body"},
            "shirt": {"kind": "mock-garment", "layer": 20},
            "hood": {"kind": "mock-garment", "parent": "shirt", "layer": 20},
            "vest": {"kind": "mock-garment", "layer": 30}
        }));
        let tree = Tree::of(&d);
        let order = [node(&tree, "body"), node(&tree, "vest"), node(&tree, "shirt")];
        let plan = plan(&d, &tree, &order, 1).unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(layers(&tree, &plan), [("vest".to_owned(), 19)]);
    }

    #[test]
    fn changes_that_cannot_run_one_by_one_in_sibling_order_run_from_the_back() {
        // a moves below b; c needs a head built first, so c can't stay at 0 while a goes to 1.
        let d = def(json!({
            "a": {"kind": "mock-head"},
            "b": {"kind": "mock-head"},
            "c": {"kind": "mock-hair"}
        }));
        let tree = Tree::of(&d);
        let order = [node(&tree, "b"), node(&tree, "a"), node(&tree, "c")];
        let plan = plan(&d, &tree, &order, 1).unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(layers(&tree, &plan), [("c".to_owned(), 1), ("a".to_owned(), 1)]);
    }

    #[test]
    fn a_part_already_in_place_changes_nothing() {
        let d = def(json!({"a": {"kind": "mock-garment"}, "b": {"kind": "mock-garment"}}));
        let tree = Tree::of(&d);
        let order = [node(&tree, "a"), node(&tree, "b")];
        let none = plan(&d, &tree, &order, 0).unwrap_or_else(|e| panic!("{}", e.message));
        assert_eq!(none.len(), 0, "{none:?}");
    }
}
