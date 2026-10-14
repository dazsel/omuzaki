//! Unknown extensions and extras, carried through a round trip untouched.
//!
//! The scene models what Omuzaki understands. A third-party file also carries things it doesn't: vendor extensions
//! (`KHR_materials_sheen`, a studio's `EXT_…`), and `extras` blobs that tools use to stash their own data. Dropping
//! them silently would make re-exporting a purchased model destructive, so importers put them here verbatim, as
//! raw JSON, on the object they belong to (the scene as a whole, a node, a material), and exporters write them back.
//!
//! This is **best effort, not interpretation**:
//! - nothing in the bag is validated or understood, and Omuzaki never edits it;
//! - indices *inside* the JSON (a `{"index": 3}` texture reference in a material extension) are not remapped. The
//!   scene keeps its arenas in file order, so an unmodified round trip stays valid; an edit that deletes or reorders
//!   textures, images, nodes or materials can leave them dangling, which only the extension's own reader could tell;
//! - importers only capture *optional* extensions. A file that *requires* one the importer doesn't know is refused
//!   (`IoError::Unsupported`), because rendering it without would be wrong.
//!
//! [`IoScene::passthrough_finding`](crate::IoScene::passthrough_finding) lists what is carried; `run_import` and the
//! export preflight both report it, so nobody is surprised.

use std::collections::BTreeMap;

use serde_json::Value;

/// Raw JSON an importer didn't interpret, for one scene object. Empty for almost every object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Passthrough {
    /// Extension objects by extension name (`extensions` in the glTF object), verbatim.
    pub extensions: BTreeMap<String, Value>,
    /// The object's `extras`, verbatim.
    pub extras: Option<Value>,
}

impl Passthrough {
    /// True if nothing is carried.
    pub fn is_empty(&self) -> bool {
        self.extensions.is_empty() && self.extras.is_none()
    }

    /// A short description for reports: `extensions A, B; extras`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.extensions.is_empty() {
            parts.push(format!("extensions {}", self.extensions.keys().cloned().collect::<Vec<_>>().join(", ")));
        }
        if self.extras.is_some() {
            parts.push("extras".to_owned());
        }
        parts.join("; ")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn describes_what_it_carries() {
        let mut p = Passthrough::default();
        assert!(p.is_empty() && p.describe().is_empty());
        p.extensions.insert("EXT_b".into(), json!({}));
        p.extensions.insert("EXT_a".into(), json!({}));
        p.extras = Some(json!({"k": 1}));
        assert!(!p.is_empty());
        assert_eq!(p.describe(), "extensions EXT_a, EXT_b; extras");
    }
}
