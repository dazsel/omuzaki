//! Meshes and their primitives.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Extensions, collect};

/// A mesh: one or more primitives drawn together.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mesh {
    /// Display name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The pieces, each with its own material.
    pub primitives: Vec<Primitive>,
    /// Default morph target weights.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weights: Option<Vec<f32>>,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data. `targetNames` lives here (VRM 1.0, Blender) or on the primitives (older exporters).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

impl Mesh {
    /// Morph target names by convention: `extras.targetNames` of the mesh, else of its first primitive that has
    /// them (VRM 0.x exporters wrote them there). `None` when absent or not a list of strings.
    pub fn target_names(&self) -> Option<Vec<String>> {
        let from = |extras: &Option<Value>| {
            let names = extras.as_ref()?.get("targetNames")?.as_array()?;
            names.iter().map(|n| n.as_str().map(str::to_owned)).collect::<Option<Vec<String>>>()
        };
        from(&self.extras).or_else(|| self.primitives.iter().find_map(|p| from(&p.extras)))
    }

    pub(super) fn add_extension_names(&self, names: &mut BTreeSet<String>) {
        collect(&self.extensions, names);
        for p in &self.primitives {
            collect(&p.extensions, names);
        }
    }
}

/// One drawable piece of a mesh.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Primitive {
    /// Vertex attributes by semantic (`POSITION`, `NORMAL`, `TEXCOORD_0`, `_CUSTOM`, ...) to accessor index.
    pub attributes: BTreeMap<String, usize>,
    /// The index accessor; none means the vertices are drawn in order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indices: Option<usize>,
    /// The material.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub material: Option<usize>,
    /// Topology code (see [`code`](super::code)); the default is triangles.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
    /// Morph targets: per target, attribute name (`POSITION`, `NORMAL`, `TANGENT`) to a delta accessor.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<BTreeMap<String, usize>>,
    /// Extensions by name (Draco compression, for example).
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data; some exporters put `targetNames` here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn target_names_come_from_the_mesh_or_the_first_primitive() {
        let mut def = Mesh { extras: Some(json!({"targetNames": ["a", "b"]})), ..Mesh::default() };
        assert_eq!(def.target_names(), Some(vec!["a".into(), "b".into()]));
        def.extras = None;
        def.primitives = vec![
            Primitive::default(),
            Primitive { extras: Some(json!({"targetNames": ["c"]})), ..Primitive::default() },
        ];
        assert_eq!(def.target_names(), Some(vec!["c".into()]), "VRM 0.x writes the names on the primitives");
        def.primitives[1].extras = Some(json!({"targetNames": [1]}));
        assert_eq!(def.target_names(), None);
    }
}
