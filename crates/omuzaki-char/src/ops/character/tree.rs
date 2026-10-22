//! What makes a character's parts a buildable tree, checked before an edit is planned.
//!
//! [`CharacterDef::build_order`] already rejects a dangling parent and a loop among parents, layers and kind
//! requirements. It does not notice a part whose required kind is missing altogether (a shirt with no body), because
//! "requires" only orders the parts that exist; [`problems`] adds that rule. An edit is refused only for the problems
//! it introduces ([`check_edit`]), so a character that was already broken can still be repaired step by step.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_char_api::{
    Attach, CharacterDef, PartCategory, PartId, PartKindId, PartKindRegistration, TreeError, requires_of,
};
use omuzaki_ops_api::OpError;

use super::access::list;

/// The layer a new part of a kind in `category` gets when none is given, from the convention in the character
/// contract: body and head 0, shirts 20, outer garments 40 and up, accessories 50 and up. Hair builds with the head.
pub(crate) fn default_layer(category: PartCategory) -> i32 {
    match category {
        PartCategory::Body | PartCategory::Head | PartCategory::Hair => 0,
        PartCategory::Garment | PartCategory::Footwear => 20,
        PartCategory::Accessory => 50,
    }
}

/// The layer a new part of `kind` goes on: the kind's own ([`PartKind::layer`](omuzaki_char_api::PartKind::layer)), else
/// its category's.
pub(crate) fn layer_of(kind: &PartKindRegistration) -> i32 {
    (kind.layer)().unwrap_or_else(|| default_layer((kind.meta)().category))
}

/// One reason a single part cannot be built. A problem is about one part and one rule, so "the same problem before
/// and after an edit" means the same part and rule, however many other parts share it: that is what lets an edit be
/// judged by what *it* introduces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Problem {
    /// An enabled part names a parent the character does not have.
    MissingParent {
        /// The child.
        part: PartId,
        /// The parent that is not there.
        parent: PartId,
    },
    /// An enabled part needs a part of a kind that no enabled part has.
    Unmet {
        /// The part that needs it.
        part: PartId,
        /// The kind it needs built before it.
        needs: PartKindId,
    },
    /// The part cannot be put in build order: parents, layers and kind requirements contradict each other, or it
    /// waits on a part that does.
    Blocked {
        /// The part left without a place.
        part: PartId,
    },
}

/// What the contract's own tree check found, as problems of single parts.
fn from_tree_error(error: TreeError) -> Vec<Problem> {
    match error {
        TreeError::MissingParent { part, parent } => vec![Problem::MissingParent { part, parent }],
        TreeError::Cycle(parts) => parts.into_iter().map(|part| Problem::Blocked { part }).collect(),
    }
}

/// Every reason `def` cannot be built, in a stable order (empty when it can).
pub(crate) fn problems(def: &CharacterDef) -> Vec<Problem> {
    let active = match def.active_parts() {
        Ok(active) => active,
        Err(e) => return from_tree_error(e),
    };
    let mut found = Vec::new();
    for id in &active {
        for needs in requires_of(&def.parts[id].kind) {
            if !active.iter().any(|other| other != id && def.parts[other].kind == needs) {
                found.push(Problem::Unmet { part: id.clone(), needs });
            }
        }
    }
    if let Err(e) = def.build_order(&requires_of) {
        found.extend(from_tree_error(e));
    }
    found
}

/// The problems of `after` that `known` (those of the character before the edit) did not already have.
fn introduced(known: &[Problem], after: &CharacterDef) -> Vec<Problem> {
    problems(after).into_iter().filter(|p| !known.contains(p)).collect()
}

/// The problems as sentences, those that share a rule together (`shirt`, `vest` need a part of kind `body`).
pub(crate) fn describe(found: &[Problem]) -> String {
    let mut sentences = Vec::new();
    let mut unmet: BTreeMap<&PartKindId, Vec<&PartId>> = BTreeMap::new();
    let mut blocked = Vec::new();
    for problem in found {
        match problem {
            Problem::MissingParent { part, parent } => {
                sentences.push(format!("`{part}` has parent `{parent}`, which isn't in the character"));
            }
            Problem::Unmet { part, needs } => unmet.entry(needs).or_default().push(part),
            Problem::Blocked { part } => blocked.push(part),
        }
    }
    for (needs, parts) in unmet {
        let verb = if parts.len() == 1 { "needs" } else { "need" };
        sentences.push(format!("{} {verb} a part of kind `{needs}`, and none is enabled", list(parts)));
    }
    if !blocked.is_empty() {
        sentences.push(format!(
            "these parts can't be put in build order, because they wait on each other in a loop or on a part that \
             does: {}",
            list(blocked)
        ));
    }
    sentences.join("; ")
}

/// How to fix the problem `first`, out of the `found` ones, for the error's hint. `built` are the parts that built
/// before the edit: only those can sensibly be removed or disabled to resolve it (a part being added cannot).
fn hint(first: &Problem, found: &[Problem], built: &BTreeSet<PartId>) -> String {
    match first {
        Problem::MissingParent { part, parent } => {
            format!("add `{parent}` first, or give `{part}` an existing parent with `part.reorder`")
        }
        Problem::Unmet { needs, .. } => {
            let needing: Vec<&PartId> = found
                .iter()
                .filter_map(|p| match p {
                    Problem::Unmet { part, needs: n } if n == needs => Some(part),
                    _ => None,
                })
                .collect();
            if needing.iter().all(|p| built.contains(*p)) {
                format!("add or enable a part of kind `{needs}` first, or remove or disable {}", list(needing))
            } else {
                format!("add or enable a part of kind `{needs}` first")
            }
        }
        Problem::Blocked { .. } => "a part builds after its parent, after every part with a lower layer and after the \
                                    kinds it requires: change the layer or the parent of one of these parts"
            .to_owned(),
    }
}

/// Checks that moving the character from `before` to `after` introduces no problem, and returns the build order of
/// `after` (empty if it cannot be ordered, which only happens for a character that was already broken).
///
/// `action` reads as the start of a sentence ("removing `body`"). `field` names the argument to blame for a given
/// problem; it only runs when the edit is refused.
///
/// # Errors
/// `invalid` listing every problem the edit introduces, with a hint for the first. A problem the character already
/// had (the same part and the same rule) is not blamed on the edit, so a broken character can be repaired.
pub(crate) fn check_edit(
    before: &CharacterDef,
    after: &CharacterDef,
    action: &str,
    field: impl FnOnce(&Problem) -> &'static str,
) -> Result<Vec<PartId>, OpError> {
    let found = introduced(&problems(before), after);
    if let Some(first) = found.first() {
        let built = before.active_parts().unwrap_or_default();
        return Err(OpError::invalid(
            field(first),
            format!("{action} would leave the character unbuildable: {}", describe(&found)),
        )
        .hint(hint(first, &found, &built)));
    }
    Ok(after.build_order(&requires_of).unwrap_or_default())
}

/// Which argument to blame when an edit made of several (a layer and a parent) puts parts in a loop: the first of
/// `candidates` (the argument's name, and the character with only that argument's change applied) whose change alone
/// introduces a problem, or `fallback` when only the combination does.
pub(crate) fn culprit(
    before: &CharacterDef,
    candidates: &[(&'static str, CharacterDef)],
    fallback: &'static str,
) -> &'static str {
    let known = problems(before);
    candidates
        .iter()
        .find(|(_, after)| !introduced(&known, after).is_empty())
        .map_or(fallback, |(argument, _)| argument)
}

/// The parts that name `part` as their parent, enabled or not.
pub(crate) fn children(def: &CharacterDef, part: &PartId) -> Vec<PartId> {
    def.parts.iter().filter(|(_, p)| p.parent.as_ref() == Some(part)).map(|(id, _)| id.clone()).collect()
}

/// The parts fixed to an anchor that `part` publishes.
pub(crate) fn anchored_to(def: &CharacterDef, part: &PartId) -> Vec<PartId> {
    def.parts
        .iter()
        .filter(|(_, p)| matches!(&p.attach, Some(Attach::Anchor { part: target, .. }) if target == part))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Why an enabled `part` is not built even though it is enabled (a disabled ancestor), for the op summaries.
pub(crate) fn idle_reason(def: &CharacterDef, part: &PartId) -> Option<String> {
    let this = def.parts.get(part)?;
    if !this.enabled {
        return None;
    }
    let mut cursor = this.parent.as_ref();
    for _ in 0..def.parts.len() {
        let ancestor = cursor?;
        let parent = def.parts.get(ancestor)?;
        if !parent.enabled {
            return Some(format!("`{part}` is enabled but isn't built because its ancestor `{ancestor}` is disabled"));
        }
        cursor = parent.parent.as_ref();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_follow_the_documented_convention() {
        assert_eq!(default_layer(PartCategory::Body), 0);
        assert_eq!(default_layer(PartCategory::Hair), 0);
        assert_eq!(default_layer(PartCategory::Garment), 20);
        assert_eq!(default_layer(PartCategory::Accessory), 50);
    }

    #[test]
    fn a_dangling_parent_reads_as_a_problem_with_a_hint() {
        let hood = PartId::new("hood").unwrap();
        let robe = PartId::new("robe").unwrap();
        let problem = Problem::MissingParent { part: hood, parent: robe };
        assert_eq!(describe(std::slice::from_ref(&problem)), "`hood` has parent `robe`, which isn't in the character");
        assert!(hint(&problem, std::slice::from_ref(&problem), &BTreeSet::new()).contains("add `robe` first"));
    }

    #[test]
    fn parts_that_share_a_rule_are_described_together() {
        let (a, b, kind) = (PartId::new("a").unwrap(), PartId::new("b").unwrap(), PartKindId::new("body").unwrap());
        let one = [Problem::Unmet { part: a.clone(), needs: kind.clone() }];
        assert_eq!(describe(&one), "`a` needs a part of kind `body`, and none is enabled");
        let two = [one[0].clone(), Problem::Unmet { part: b, needs: kind }];
        assert_eq!(describe(&two), "`a`, `b` need a part of kind `body`, and none is enabled");
        let built_both = BTreeSet::from([a.clone(), PartId::new("b").unwrap()]);
        assert!(hint(&two[0], &two, &built_both).ends_with("remove or disable `a`, `b`"));
        assert!(!hint(&two[0], &two, &BTreeSet::from([a])).contains("remove"), "b isn't built, so it can't be removed");
    }
}
