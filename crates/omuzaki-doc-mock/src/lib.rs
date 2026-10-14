//! An in-memory [`DocWrite`] for tests: every op, MCP and UI test runs against it without touching disk.
//!
//! Patch application is the shared [`apply_to`], which is all or nothing; the real store (`omuzaki-doc`) gets the same
//! atomicity with structural sharing.
#![deny(missing_docs)]

use omuzaki_doc_api::{DocPath, DocRead, DocVersion, DocWrite, Patch, PatchError, Value, apply_to, lookup};
use serde_json::Map;

/// A document held in memory.
#[derive(Clone, Debug, PartialEq)]
pub struct MemDoc {
    root: Value,
    version: DocVersion,
}

impl MemDoc {
    /// An empty document (an empty object at the root).
    pub fn new() -> Self {
        Self::from_value(Value::Object(Map::new()))
    }

    /// A document starting from `root`, at version 0.
    pub fn from_value(root: Value) -> Self {
        Self { root, version: DocVersion(0) }
    }

    /// The whole tree.
    pub fn root(&self) -> &Value {
        &self.root
    }
}

impl Default for MemDoc {
    fn default() -> Self {
        Self::new()
    }
}

impl DocRead for MemDoc {
    fn get(&self, path: &DocPath) -> Option<&Value> {
        lookup(&self.root, path)
    }

    fn version(&self) -> DocVersion {
        self.version
    }
}

impl DocWrite for MemDoc {
    fn apply(&mut self, patch: &Patch) -> Result<DocVersion, PatchError> {
        apply_to(&mut self.root, patch)?;
        self.version = DocVersion(self.version.0.saturating_add(1));
        Ok(self.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn p(s: &str) -> DocPath {
        DocPath::parse(s).unwrap()
    }

    #[test]
    fn apply_then_inverse_restores_the_document() {
        let mut d = MemDoc::from_value(json!({"chars": {"d": {"face": {"profile": "round"}}}}));
        let before = d.clone();
        let patch = Patch::new()
            .with(p("chars/d/face/profile"), Some(json!("round")), Some(json!("oval-long")))
            .with(p("chars/d/face/blend"), None, Some(json!(0.5)))
            .with(p("chars/e"), None, Some(json!({"name": "Elf"})));
        assert_eq!(d.apply(&patch).unwrap(), DocVersion(1));
        assert_eq!(d.get(&p("chars/d/face/profile")), Some(&json!("oval-long")));
        assert_eq!(d.get(&p("chars/e/name")), Some(&json!("Elf")));
        d.apply(&patch.inverse()).unwrap();
        assert_eq!(d.root(), before.root());
        assert_eq!(d.version(), DocVersion(2));
    }

    #[test]
    fn parents_are_never_created_implicitly() {
        let mut d = MemDoc::new();
        let err = d.apply(&Patch::new().with(p("a/b"), None, Some(json!(1)))).unwrap_err();
        assert!(matches!(err, PatchError::Unreachable { .. }));
        assert_eq!(d.version(), DocVersion(0));
    }

    #[test]
    fn conflicts_leave_the_document_untouched() {
        let mut d = MemDoc::from_value(json!({"a": 1, "b": 2}));
        let patch =
            Patch::new().with(p("a"), Some(json!(1)), Some(json!(10))).with(p("b"), Some(json!(99)), Some(json!(3)));
        let err = d.apply(&patch).unwrap_err();
        assert!(matches!(err, PatchError::Conflict { ref path, .. } if path.to_string() == "b"));
        assert_eq!(d.root(), &json!({"a": 1, "b": 2}));
        assert_eq!(d.version(), DocVersion(0));
    }

    #[test]
    fn arrays_change_in_place_append_and_pop() {
        let mut d = MemDoc::from_value(json!({"l": [1, 2]}));
        d.apply(&Patch::new().with(p("l/2"), None, Some(json!(3)))).unwrap();
        d.apply(&Patch::new().with(p("l/0"), Some(json!(1)), Some(json!(0)))).unwrap();
        assert_eq!(d.root(), &json!({"l": [0, 2, 3]}));
        d.apply(&Patch::new().with(p("l/2"), Some(json!(3)), None)).unwrap();
        assert_eq!(d.root(), &json!({"l": [0, 2]}));
        assert!(d.apply(&Patch::new().with(p("l/0"), Some(json!(0)), None)).is_err());
    }
}
