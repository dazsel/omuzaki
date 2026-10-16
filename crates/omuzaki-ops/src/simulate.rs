//! Trying patches without touching the document: a scratch copy of just the parts a set of patches can reach.
//!
//! Undo of a step with effects must know its patch applies **before** it runs any effect call, so a refused undo
//! changes nothing anywhere; undo-session must know which of an agent's steps revert, one on top of the other, before
//! it applies their combined inverse as one change (one journal entry, one version). Both try the patches here first.
//!
//! The scratch holds, for every distinct parent of a changed path, a copy of the document's value there, at the same
//! path. A change only reads and writes at or under its parent, so the doc-api's own [`apply_to`] on the scratch
//! decides exactly as the document would. Copies are only of those parents, never the whole tree unless a change sits
//! at the top level.

use omuzaki_doc_api::{DocPath, DocRead, Patch, PatchError, Value, apply_to};

/// A scratch copy of the parts of a document that `patches` can reach, to try them on in order.
pub(crate) struct Trial {
    root: Value,
}

impl Trial {
    /// Copies from `doc` what any of `patches` can reach.
    pub(crate) fn of<'a>(doc: &dyn DocRead, patches: impl IntoIterator<Item = &'a Patch>) -> Self {
        let mut parents: Vec<DocPath> = patches
            .into_iter()
            .flat_map(|patch| patch.changes.iter())
            .map(|change| change.path.split_last().map_or_else(DocPath::root, |(parent, _)| parent))
            .collect();
        parents.sort();
        parents.dedup();
        // Sorted segment-wise, a path is followed by everything under it: keeping only paths not under the last kept
        // one leaves the minimal set.
        let mut minimal: Vec<DocPath> = Vec::with_capacity(parents.len());
        for parent in parents {
            if !minimal.last().is_some_and(|last| parent.starts_with(last)) {
                minimal.push(parent);
            }
        }
        if minimal.first().is_some_and(|first| first.segments().is_empty()) {
            return Self { root: doc.get(&DocPath::root()).cloned().unwrap_or(Value::Null) };
        }
        let mut root = Value::Object(Default::default());
        for parent in minimal {
            if let Some(value) = doc.get(&parent) {
                place(&mut root, parent.segments(), value.clone());
            }
        }
        Self { root }
    }

    /// Tries `patch` on the scratch: applied on success, untouched on failure, as the document would be.
    ///
    /// # Errors
    /// The conflict or unreachable path the document would report.
    pub(crate) fn apply(&mut self, patch: &Patch) -> Result<(), PatchError> {
        apply_to(&mut self.root, patch)
    }
}

/// Puts `value` at `segments` under `root`, making the objects on the way. Only the parents of changed paths are
/// placed, and changes never touch what lies above them, so the objects on the way are never read.
fn place(root: &mut Value, segments: &[String], value: Value) {
    let Some((last, above)) = segments.split_last() else {
        *root = value;
        return;
    };
    let mut at = root;
    for segment in above {
        if !at.is_object() {
            *at = Value::Object(Default::default());
        }
        let Value::Object(map) = at else { return };
        at = map.entry(segment.clone()).or_insert_with(|| Value::Object(Default::default()));
    }
    if !at.is_object() {
        *at = Value::Object(Default::default());
    }
    if let Value::Object(map) = at {
        map.insert(last.clone(), value);
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_doc_api::{DocWrite, Patch};
    use omuzaki_doc_mock::MemDoc;
    use serde_json::json;

    use super::*;

    fn p(s: &str) -> DocPath {
        DocPath::parse(s).unwrap()
    }

    #[test]
    fn a_trial_decides_as_the_document_does_in_order() {
        let doc = MemDoc::from_value(json!({"chars": {"a": {"x": 1, "list": [1, 2]}}, "other": 0}));
        let first = Patch::new().with(p("chars/a/x"), Some(json!(1)), Some(json!(2)));
        let second = Patch::new().with(p("chars/a/x"), Some(json!(2)), Some(json!(3)));
        let append = Patch::new().with(p("chars/a/list/2"), None, Some(json!(3)));
        let stale = Patch::new().with(p("chars/a/x"), Some(json!(1)), Some(json!(9)));
        let mut trial = Trial::of(&doc, [&first, &second, &append, &stale]);
        trial.apply(&first).unwrap();
        trial.apply(&second).unwrap();
        trial.apply(&append).unwrap();
        assert!(matches!(trial.apply(&stale), Err(PatchError::Conflict { .. })), "x is 3 by now");
        // The same order on the document itself agrees.
        let mut real = doc.clone();
        for patch in [&first, &second, &append] {
            real.apply(patch).unwrap();
        }
        assert!(real.apply(&stale).is_err());
        let orphan = Patch::new().with(p("missing/x"), None, Some(json!(1)));
        assert!(matches!(Trial::of(&doc, [&orphan]).apply(&orphan), Err(PatchError::Unreachable { .. })));
        let top = Patch::new().with(p("other"), Some(json!(0)), Some(json!(1)));
        Trial::of(&doc, [&top]).apply(&top).unwrap();
    }
}
