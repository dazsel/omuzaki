//! The one CPU-side triangle mesh every subsystem exchanges.
//!
//! Procedural parts (`omuzaki-char`), importers (`omuzaki-io`) and the renderer all speak [`MeshData`], so there is no
//! conversion layer between them and one place that defines what a valid mesh is. It lives in `omuzaki-core` rather than
//! the render contract because the character kit and the I/O contract need it without depending on a renderer.

use glam::{Vec2, Vec3};

use crate::id::Slug;

/// Why a mesh is invalid: the first rule it breaks.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid mesh: {0}")]
pub struct MeshError(pub String);

/// A named per-vertex attribute beyond the standard set, for shading that needs more than position, normal and UV
/// (the hair shader's strand coordinates, for example). Which names a renderer understands is up to the shading
/// program that reads them; an attribute nothing reads is ignored.
#[derive(Clone, Debug, PartialEq)]
pub struct ExtraAttribute {
    /// The attribute's name, as the shading program asks for it.
    pub name: Slug,
    /// Floats per vertex, `1..=4`.
    pub components: u8,
    /// `components` floats for every vertex, vertex after vertex.
    pub data: Vec<f32>,
}

/// A triangle mesh in CPU memory. Triangles are counter-clockwise seen from the front; the coordinate system is
/// right-handed, +Y up, +Z toward the viewer of a character (glTF and three.js conventions).
///
/// One mesh has one material: importers split multi-material meshes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeshData {
    /// Vertex positions, metres, in the mesh's own space.
    pub positions: Vec<Vec3>,
    /// Unit normals, one per vertex. Producers decide flat or smooth; the renderer never recomputes them.
    pub normals: Vec<Vec3>,
    /// Texture coordinates, glTF convention: `(0, 0)` is the top-left of the image. Empty or one per vertex.
    pub uvs: Vec<Vec2>,
    /// Linear vertex colours (RGBA), multiplied into the base colour when the material asks. Empty or one per vertex.
    pub colours: Vec<[f32; 4]>,
    /// The four joints that move each vertex, as indices into the joint run that skins the instance. Empty (rigid)
    /// or one per vertex, and then `weights` has one per vertex too.
    pub joints: Vec<[u16; 4]>,
    /// Skinning weights for `joints`, summing to 1 (unused slots are 0).
    pub weights: Vec<[f32; 4]>,
    /// Named extra attributes, at most [`MeshData::MAX_EXTRA_ATTRIBUTES`], each with a distinct name.
    pub extra: Vec<ExtraAttribute>,
    /// Triangle list: three indices per triangle.
    pub indices: Vec<u32>,
}

impl MeshData {
    /// The most extra attributes one mesh may carry (GPUs guarantee 16 vertex inputs; the standard set uses 6).
    pub const MAX_EXTRA_ATTRIBUTES: usize = 4;

    /// True if the vertices carry skinning data.
    pub fn is_skinned(&self) -> bool {
        !self.joints.is_empty()
    }

    /// The number of triangles.
    pub fn triangle_count(&self) -> usize {
        self.indices.len() / 3
    }

    /// The extra attribute called `name`, if the mesh has it.
    pub fn extra_attribute(&self, name: &str) -> Option<&ExtraAttribute> {
        self.extra.iter().find(|a| a.name.as_str() == name)
    }

    /// Checks every rule a consumer relies on, so none has to re-check them (or crash on a bad index).
    ///
    /// # Errors
    /// [`MeshError`] naming the first broken rule: empty mesh, mismatched attribute lengths, an index past the last
    /// vertex, a partial triangle, a non-finite position or normal, or a malformed extra attribute.
    pub fn validate(&self) -> Result<(), MeshError> {
        let bad = |reason: String| Err(MeshError(reason));
        let n = self.positions.len();
        if n == 0 || self.indices.is_empty() {
            return bad("a mesh needs at least one triangle".into());
        }
        for (name, len, required) in [
            ("normals", self.normals.len(), true),
            ("uvs", self.uvs.len(), false),
            ("colours", self.colours.len(), false),
            ("joints", self.joints.len(), false),
            ("weights", self.weights.len(), false),
        ] {
            if len != n && (required || len != 0) {
                return bad(format!("{name} has {len} entries for {n} vertices"));
            }
        }
        if self.joints.is_empty() != self.weights.is_empty() {
            return bad("joints and weights come together".into());
        }
        if !self.indices.len().is_multiple_of(3) {
            return bad(format!("{} indices isn't a whole number of triangles", self.indices.len()));
        }
        if let Some(i) = self.indices.iter().find(|&&i| i as usize >= n) {
            return bad(format!("index {i} is past the last vertex ({})", n - 1));
        }
        if self.positions.iter().chain(&self.normals).any(|v| !v.is_finite()) {
            return bad("positions and normals must be finite".into());
        }
        self.validate_extra(n)
    }

    fn validate_extra(&self, vertices: usize) -> Result<(), MeshError> {
        let bad = |reason: String| Err(MeshError(reason));
        if self.extra.len() > Self::MAX_EXTRA_ATTRIBUTES {
            return bad(format!("{} extra attributes (at most {})", self.extra.len(), Self::MAX_EXTRA_ATTRIBUTES));
        }
        for (i, attr) in self.extra.iter().enumerate() {
            if !(1..=4).contains(&attr.components) {
                return bad(format!(
                    "extra attribute {:?} has {} components (needs 1 to 4)",
                    attr.name, attr.components
                ));
            }
            let expected = vertices * usize::from(attr.components);
            if attr.data.len() != expected {
                return bad(format!(
                    "extra attribute {:?} has {} floats, expected {expected}",
                    attr.name,
                    attr.data.len()
                ));
            }
            if attr.data.iter().any(|v| !v.is_finite()) {
                return bad(format!("extra attribute {:?} must be finite", attr.name));
            }
            if self.extra[..i].iter().any(|other| other.name == attr.name) {
                return bad(format!("extra attribute {:?} appears twice", attr.name));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Break = fn(&mut MeshData);

    fn triangle() -> MeshData {
        MeshData {
            positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
            normals: vec![Vec3::Z; 3],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        }
    }

    fn strand(name: &str, components: u8, floats: usize) -> ExtraAttribute {
        ExtraAttribute { name: Slug::new(name).unwrap(), components, data: vec![0.5; floats] }
    }

    #[test]
    fn a_plain_triangle_is_valid() {
        assert_eq!(triangle().validate(), Ok(()));
        assert_eq!((triangle().triangle_count(), triangle().is_skinned()), (1, false));
    }

    #[test]
    fn each_broken_rule_is_reported() {
        let cases: [(&str, Break); 7] = [
            ("at least one", |m| m.indices.clear()),
            ("normals", |m| {
                m.normals.pop();
            }),
            ("uvs", |m| m.uvs = vec![Vec2::ZERO]),
            ("together", |m| m.joints = vec![[0; 4]; 3]),
            ("whole number", |m| m.indices.push(0)),
            ("past the last", |m| m.indices[2] = 9),
            ("finite", |m| m.positions[1] = Vec3::NAN),
        ];
        for (needle, break_it) in cases {
            let mut m = triangle();
            break_it(&mut m);
            let err = m.validate().unwrap_err().to_string();
            assert!(err.contains(needle), "expected {needle:?} in {err:?}");
        }
    }

    #[test]
    fn skinned_meshes_need_joints_and_weights_together() {
        let mut m = triangle();
        m.joints = vec![[0; 4]; 3];
        m.weights = vec![[1.0, 0.0, 0.0, 0.0]; 3];
        assert_eq!(m.validate(), Ok(()));
        assert!(m.is_skinned());
    }

    #[test]
    fn extra_attributes_are_checked_for_shape_and_names() {
        let mut m = triangle();
        m.extra = vec![strand("hair-uv", 2, 6), strand("hair-info", 3, 9)];
        assert_eq!(m.validate(), Ok(()));
        assert_eq!(m.extra_attribute("hair-info").map(|a| a.components), Some(3));
        assert!(m.extra_attribute("nope").is_none());

        let cases: [(&str, Vec<ExtraAttribute>); 5] = [
            ("1 to 4", vec![strand("a", 5, 15)]),
            ("1 to 4", vec![strand("a", 0, 0)]),
            ("expected 6", vec![strand("a", 2, 5)]),
            ("twice", vec![strand("a", 1, 3), strand("a", 1, 3)]),
            ("at most 4", (0..5).map(|i| strand(&format!("a{i}"), 1, 3)).collect()),
        ];
        for (needle, extra) in cases {
            m.extra = extra;
            let err = m.validate().unwrap_err().to_string();
            assert!(err.contains(needle), "expected {needle:?} in {err:?}");
        }
        m.extra = vec![ExtraAttribute { data: vec![f32::NAN; 3], ..strand("a", 1, 3) }];
        assert!(m.validate().unwrap_err().to_string().contains("finite"));
    }
}
