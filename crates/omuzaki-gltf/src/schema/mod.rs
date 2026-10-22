//! The glTF 2.0 JSON schema as typed serde structs (`KhronosGroup/glTF`, `specification/2.0`), one set for reading and
//! writing. Field names follow the specification; serde converts them to camelCase.
//!
//! **The model is faithful.** Every object keeps its `extensions` (raw JSON by name) and `extras` (raw JSON), so a
//! format built on glTF (VRM adds `VRMC_*` to the root, nodes and materials) can read and write its own data without
//! this crate knowing it. Every array is a `Vec` in file order and every cross reference is the plain `usize` index
//! the file uses: nothing is renumbered, deduplicated or dropped, so an index inside an extension body means the same
//! object in the model as it does in the file.
//!
//! **Lenient reading, canonical writing.** Reading takes every field as optional and ignores unknown keys. Writing
//! emits fields in declaration order and skips the empty ones, which is what keeps the output deterministic. The two
//! typed material extensions (`KHR_materials_unlit`, `KHR_materials_emissive_strength`) and `KHR_texture_transform`
//! are fields of their own so `f32` numbers print as `f32` ("0.05", not "0.05000000074505806"); every other
//! extension is raw JSON (build such bodies with [`to_json`](crate::to_json)).

mod data;
mod material;
mod mesh;

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_io_api::IoError;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use self::data::{Accessor, Buffer, BufferView, Sparse, SparseIndices, SparseValues};
pub use self::material::{
    EmissiveStrength, Empty, Image, Material, MaterialExtensions, Pbr, Sampler, Texture, TextureInfo,
    TextureInfoExtensions, TextureTransform,
};
pub use self::mesh::{Mesh, Primitive};

/// Extension objects by name, verbatim.
pub type Extensions = BTreeMap<String, Value>;

/// Numeric codes the specification gives to accessor component types, buffer view targets, primitive modes and
/// sampler settings (the OpenGL enums glTF borrows).
pub mod code {
    /// `accessor.componentType`: signed byte.
    pub const I8: u32 = 5120;
    /// `accessor.componentType`: unsigned byte.
    pub const U8: u32 = 5121;
    /// `accessor.componentType`: signed short.
    pub const I16: u32 = 5122;
    /// `accessor.componentType`: unsigned short.
    pub const U16: u32 = 5123;
    /// `accessor.componentType`: unsigned int.
    pub const U32: u32 = 5125;
    /// `accessor.componentType`: 32-bit float.
    pub const F32: u32 = 5126;
    /// `bufferView.target`: vertex attributes.
    pub const ARRAY_BUFFER: u32 = 34962;
    /// `bufferView.target`: vertex indices.
    pub const ELEMENT_ARRAY_BUFFER: u32 = 34963;
    /// `primitive.mode`: the default.
    pub const TRIANGLES: u32 = 4;
    /// `primitive.mode`.
    pub const TRIANGLE_STRIP: u32 = 5;
    /// `primitive.mode`.
    pub const TRIANGLE_FAN: u32 = 6;
    /// `sampler.magFilter` / `minFilter`.
    pub const NEAREST: u32 = 9728;
    /// `sampler.magFilter` / `minFilter`.
    pub const LINEAR: u32 = 9729;
    /// `sampler.minFilter`.
    pub const NEAREST_MIPMAP_NEAREST: u32 = 9984;
    /// `sampler.minFilter`.
    pub const NEAREST_MIPMAP_LINEAR: u32 = 9986;
    /// `sampler.minFilter`.
    pub const LINEAR_MIPMAP_LINEAR: u32 = 9987;
    /// `sampler.wrapS` / `wrapT`: the default.
    pub const REPEAT: u32 = 10497;
    /// `sampler.wrapS` / `wrapT`.
    pub const CLAMP_TO_EDGE: u32 = 33071;
    /// `sampler.wrapS` / `wrapT`.
    pub const MIRRORED_REPEAT: u32 = 33648;
}

// serde's `skip_serializing_if` hands over a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
pub(crate) fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The glTF root object.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Document {
    /// Version and authoring information (the only required member).
    pub asset: Asset,
    /// Every extension some object uses (`extensionsUsed`). The writer fills it in; see
    /// [`Document::extension_names`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions_used: Vec<String>,
    /// Extensions a reader must understand to load the file correctly.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extensions_required: Vec<String>,
    /// The default scene.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scene: Option<usize>,
    /// Scenes: lists of root nodes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub scenes: Vec<Scene>,
    /// The node hierarchy, flat; `children` index into this.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<Node>,
    /// Meshes.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub meshes: Vec<Mesh>,
    /// Skins.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub skins: Vec<Skin>,
    /// Materials.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub materials: Vec<Material>,
    /// Textures: an image plus a sampler.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub textures: Vec<Texture>,
    /// Images, by `uri` or buffer view.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<Image>,
    /// Samplers.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub samplers: Vec<Sampler>,
    /// Accessors: typed views of buffer data.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub accessors: Vec<Accessor>,
    /// Buffer views: byte ranges of buffers.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub buffer_views: Vec<BufferView>,
    /// Buffers: the binary data.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub buffers: Vec<Buffer>,
    /// Animations, kept as raw JSON: the scene model holds none, but a format that needs them (VRM Animation) can
    /// read them from here. Their accessor indices are only valid against the buffers they came with.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub animations: Vec<Value>,
    /// Cameras, kept as raw JSON.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cameras: Vec<Value>,
    /// Root extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

impl Document {
    /// The root nodes of the default scene (`scene`, else the first), or, for a file with no scenes, the nodes nobody
    /// lists as a child. Indices are the file's.
    ///
    /// # Errors
    /// [`IoError::Malformed`] when `scene` names a scene that doesn't exist.
    pub fn default_roots(&self) -> Result<Vec<usize>, IoError> {
        if self.scenes.is_empty() {
            let children: BTreeSet<usize> = self.nodes.iter().flat_map(|n| n.children.iter().copied()).collect();
            return Ok((0..self.nodes.len()).filter(|i| !children.contains(i)).collect());
        }
        let wanted = self.scene.unwrap_or(0);
        let scene = self.scenes.get(wanted).ok_or_else(|| IoError::Malformed {
            message: format!("scene {wanted} doesn't exist"),
            at: Some("/scene".into()),
        })?;
        if let Some((at, &node)) = scene.nodes.iter().enumerate().find(|&(_, &n)| n >= self.nodes.len()) {
            // Refused here rather than carried on: a file of dangling roots would otherwise cost one finding each.
            return Err(IoError::Malformed {
                message: format!("scene {wanted} names node {node}, and there are {} nodes", self.nodes.len()),
                at: Some(format!("/scenes/{wanted}/nodes/{at}")),
            });
        }
        Ok(scene.nodes.clone())
    }

    /// The texture that shows `image`, adding one (without a sampler) if there is none yet. VRM 0.x names its
    /// thumbnail by texture index, so a writer that only has images needs this.
    pub fn texture_for_image(&mut self, image: usize) -> usize {
        let existing = self.textures.iter().position(|t| t.source == Some(image));
        existing.unwrap_or_else(|| {
            self.textures.push(Texture { source: Some(image), ..Texture::default() });
            self.textures.len() - 1
        })
    }

    /// The name of every extension that appears anywhere in the document: on the root, on any object, and inside
    /// raw extension bodies (a texture reference inside `VRMC_materials_mtoon` may carry its own
    /// `KHR_texture_transform`). This is exactly what `extensionsUsed` must list; the writer sets it from here.
    pub fn extension_names(&self) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        let mut add = |extensions: &Extensions| collect(extensions, &mut names);
        add(&self.extensions);
        add(&self.asset.extensions);
        self.scenes.iter().for_each(|s| add(&s.extensions));
        self.nodes.iter().for_each(|n| add(&n.extensions));
        self.skins.iter().for_each(|s| add(&s.extensions));
        self.textures.iter().for_each(|t| add(&t.extensions));
        self.images.iter().for_each(|i| add(&i.extensions));
        self.samplers.iter().for_each(|s| add(&s.extensions));
        self.buffer_views.iter().for_each(|v| add(&v.extensions));
        self.buffers.iter().for_each(|b| add(&b.extensions));
        for a in &self.accessors {
            add(&a.extensions);
            if let Some(sparse) = &a.sparse {
                add(&sparse.extensions);
                add(&sparse.indices.extensions);
                add(&sparse.values.extensions);
            }
        }
        for m in &self.meshes {
            m.add_extension_names(&mut names);
        }
        for m in &self.materials {
            m.add_extension_names(&mut names);
        }
        names
    }
}

/// Adds the keys of `extensions` and every extension nested in their bodies. `extras` are application data, not
/// glTF extension use, so they are never searched.
pub(crate) fn collect(extensions: &Extensions, names: &mut BTreeSet<String>) {
    for (name, body) in extensions {
        names.insert(name.clone());
        collect_nested(body, names);
    }
}

/// Finds objects named `extensions` inside raw JSON and adds their keys. glTF allows extensions inside extension
/// bodies (texture references in `VRMC_materials_mtoon`), and `extensionsUsed` must list those too.
fn collect_nested(value: &Value, names: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                if key == "extensions"
                    && let Value::Object(nested) = inner
                {
                    names.extend(nested.keys().cloned());
                }
                collect_nested(inner, names);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_nested(item, names)),
        _ => {}
    }
}

/// Version and authoring information.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Asset {
    /// The glTF version the file targets ("2.0").
    pub version: String,
    /// The least version a reader must support.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_version: Option<String>,
    /// The tool that wrote the file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    /// Copyright notice.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub copyright: Option<String>,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// A scene: the root nodes to show together.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scene {
    /// Display name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Root node indices.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub nodes: Vec<usize>,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// A node of the hierarchy.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Node {
    /// Display name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Child node indices.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<usize>,
    /// The mesh this node shows.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mesh: Option<usize>,
    /// The skin that deforms the mesh.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skin: Option<usize>,
    /// Local translation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub translation: Option<[f32; 3]>,
    /// Local rotation, a quaternion `[x, y, z, w]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rotation: Option<[f32; 4]>,
    /// Local scale.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scale: Option<[f32; 3]>,
    /// Local transform as a column-major matrix (16 numbers; a wrong length is reported when the node is read, not
    /// when the document is parsed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matrix: Option<Vec<f32>>,
    /// A camera index; cameras are kept raw in [`Document::cameras`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera: Option<usize>,
    /// Default morph target weights.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weights: Option<Vec<f32>>,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// A skin: joints and their inverse bind matrices.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Skin {
    /// Display name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Joint node indices.
    pub joints: Vec<usize>,
    /// A `MAT4` float accessor with one inverse bind matrix per joint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inverse_bind_matrices: Option<usize>,
    /// The root of the skeleton hierarchy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skeleton: Option<usize>,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// Array lengths of a document, read without building any of the arrays.
///
/// `Vec<IgnoredAny>` stores nothing (the element is zero-sized), so counting a hostile file with millions of empty
/// objects costs no memory, whichever array they are in. Every top-level array of [`Document`] is here, because a typed
/// element is far larger than the `{}` that names it (a material is 888 bytes for 3 bytes of JSON): the reader checks
/// these against the limits *before* the real parse allocates anything. The arrays nested in meshes, scenes, nodes and
/// skins (primitives, morph targets, root and child indices, joints) are summed over the document the same way, with
/// [`Sum`], which keeps no element either (#633).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct Counts {
    pub scenes: Sum<SceneCount>,
    pub nodes: Sum<NodeCount>,
    pub meshes: Sum<MeshCount>,
    pub skins: Sum<SkinCount>,
    pub materials: Vec<IgnoredAny>,
    pub textures: Vec<IgnoredAny>,
    pub images: Vec<IgnoredAny>,
    pub samplers: Vec<IgnoredAny>,
    pub accessors: Vec<IgnoredAny>,
    pub buffer_views: Vec<IgnoredAny>,
    pub buffers: Vec<IgnoredAny>,
    pub animations: Vec<IgnoredAny>,
    pub cameras: Vec<IgnoredAny>,
}

/// What an element of a counted array adds to the array's total.
pub(crate) trait Tally: Default {
    /// Adds `other`'s counts to these.
    fn add(&mut self, other: &Self);
}

/// The length of a JSON array and the [`Tally`] of its elements, read one element at a time and kept by no one: an
/// array of a million elements costs the size of one. `null` counts as empty.
#[derive(Debug, Default)]
pub(crate) struct Sum<T> {
    /// How many elements the array has.
    pub len: u64,
    /// The elements' counts, added up.
    pub total: T,
}

impl<T: Tally> Tally for Sum<T> {
    fn add(&mut self, other: &Self) {
        self.len += other.len;
        self.total.add(&other.total);
    }
}

impl<'de, T: Tally + Deserialize<'de>> Deserialize<'de> for Sum<T> {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visit<T>(std::marker::PhantomData<T>);
        impl<'de, T: Tally + Deserialize<'de>> serde::de::Visitor<'de> for Visit<T> {
            type Value = Sum<T>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an array")
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Sum<T>, E> {
                Ok(Sum::default())
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Sum<T>, A::Error> {
                let mut sum = Sum::<T>::default();
                while let Some(element) = seq.next_element::<T>()? {
                    sum.len += 1;
                    sum.total.add(&element);
                }
                Ok(sum)
            }
        }
        deserializer.deserialize_any(Visit(std::marker::PhantomData))
    }
}

/// An element counted for its place only.
#[derive(Debug, Default)]
pub(crate) struct Any;

impl Tally for Any {
    fn add(&mut self, _: &Self) {}
}

impl<'de> Deserialize<'de> for Any {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        IgnoredAny::deserialize(deserializer).map(|_| Self)
    }
}

/// A scene's root indices.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SceneCount {
    pub nodes: Sum<Any>,
}

impl Tally for SceneCount {
    fn add(&mut self, other: &Self) {
        self.nodes.add(&other.nodes);
    }
}

/// A node's child indices.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct NodeCount {
    pub children: Sum<Any>,
}

impl Tally for NodeCount {
    fn add(&mut self, other: &Self) {
        self.children.add(&other.children);
    }
}

/// A mesh's primitives, and their morph targets.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct MeshCount {
    pub primitives: Sum<PrimitiveCount>,
}

impl Tally for MeshCount {
    fn add(&mut self, other: &Self) {
        self.primitives.add(&other.primitives);
    }
}

/// A primitive's morph targets.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct PrimitiveCount {
    pub targets: Sum<Any>,
}

impl Tally for PrimitiveCount {
    fn add(&mut self, other: &Self) {
        self.targets.add(&other.targets);
    }
}

/// A skin's joint indices.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(crate) struct SkinCount {
    pub joints: Sum<Any>,
}

impl Tally for SkinCount {
    fn add(&mut self, other: &Self) {
        self.joints.add(&other.joints);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn counts_see_array_lengths_without_building_them() {
        let counts: Counts = serde_json::from_str(
            r#"{"asset":{"version":"2.0"},"nodes":[{},{"a":[1,2]},{}],"images":[{}],"bufferViews":[{},{}],"cameras":[]}"#,
        )
        .unwrap();
        assert_eq!((counts.nodes.len, counts.images.len(), counts.buffer_views.len()), (3, 1, 2));
        assert_eq!((counts.cameras.len(), counts.materials.len()), (0, 0), "empty and missing arrays count zero");
    }

    #[test]
    fn counts_sum_the_nested_arrays() {
        let counts: Counts = serde_json::from_str(
            r#"{"meshes":[{"primitives":[{"targets":[{},{}]},{}]},{"primitives":[{"targets":[{}]}]},{}],
                "scenes":[{"nodes":[0,1]},{"nodes":null}],"nodes":[{"children":[1,2]},{"children":[3]},{}],
                "skins":[{"joints":[0,1,2]}]}"#,
        )
        .unwrap();
        assert_eq!((counts.meshes.len, counts.meshes.total.primitives.len), (3, 3));
        assert_eq!(counts.meshes.total.primitives.total.targets.len, 3);
        assert_eq!((counts.scenes.len, counts.scenes.total.nodes.len), (2, 2));
        assert_eq!((counts.nodes.len, counts.nodes.total.children.len), (3, 3));
        assert_eq!(counts.skins.total.joints.len, 3);
    }

    #[test]
    fn the_document_reads_leniently_and_skips_empties_on_write() {
        let doc: Document =
            serde_json::from_str(r#"{"asset":{"version":"2.0"},"unknown":{"x":1},"animations":[{}]}"#).unwrap();
        assert_eq!(doc.animations.len(), 1, "animations are kept raw");
        let text = serde_json::to_string(&doc).unwrap();
        assert_eq!(text, r#"{"asset":{"version":"2.0"},"animations":[{}]}"#);
        assert_eq!(serde_json::to_string(&Document::default()).unwrap(), r#"{"asset":{"version":""}}"#);
    }

    #[test]
    fn extensions_and_extras_survive_a_round_trip_on_every_kind_of_object() {
        let text = json!({
            "asset": { "version": "2.0", "extras": { "a": 1 }, "extensions": { "X_asset": {} } },
            "extensions": { "X_root": { "k": [1, 2] } }, "extras": { "root": true },
            "scenes": [{ "nodes": [0], "extensions": { "X_scene": {} }, "extras": 1 }],
            "nodes": [{ "extensions": { "X_node": {} }, "extras": [1] }],
            "meshes": [{ "extensions": { "X_mesh": {} }, "extras": { "targetNames": ["a"] },
                "primitives": [{ "attributes": { "POSITION": 0 }, "extensions": { "X_prim": {} }, "extras": "p" }] }],
            "skins": [{ "joints": [0], "extensions": { "X_skin": {} }, "extras": null }],
            "materials": [{ "extensions": { "X_mat": {}, "KHR_materials_unlit": {} }, "extras": { "m": 1 },
                "pbrMetallicRoughness": { "extensions": { "X_pbr": {} }, "baseColorTexture": { "index": 0,
                    "extensions": { "KHR_texture_transform": { "offset": [0.5, 0.0] }, "X_info": {} }, "extras": 7 } },
                "normalTexture": { "index": 0, "scale": 2.0, "extensions": { "X_normal": {} } },
                "occlusionTexture": { "index": 0, "strength": 0.5 } }],
            "textures": [{ "source": 0, "extensions": { "EXT_texture_webp": { "source": 1 } }, "extras": 2 }],
            "images": [{ "uri": "a.png", "extensions": { "X_image": {} } }],
            "samplers": [{ "extensions": { "X_sampler": {} } }],
            "accessors": [{ "componentType": 5126, "count": 1, "type": "SCALAR", "extensions": { "X_acc": {} },
                "sparse": { "count": 1, "indices": { "bufferView": 0, "componentType": 5123, "extensions": { "X_idx": {} } },
                    "values": { "bufferView": 1, "extras": 3 }, "extensions": { "X_sparse": {} } } }],
            "bufferViews": [{ "buffer": 0, "byteLength": 4, "extensions": { "X_view": {} } }],
            "buffers": [{ "byteLength": 4, "extensions": { "X_buffer": {} } }],
            "cameras": [{ "type": "perspective" }],
            "animations": [{ "channels": [] }]
        });
        let doc: Document = serde_json::from_value(text.clone()).unwrap();
        let again: Value = serde_json::to_value(&doc).unwrap();
        // `"extras": null` is "no extras", the one thing a typed round trip normalises.
        let mut want = text;
        want["skins"][0].as_object_mut().unwrap().remove("extras");
        assert_eq!(again, want);
        let names: Vec<String> = doc.extension_names().into_iter().collect();
        for expected in [
            "X_asset",
            "X_root",
            "X_scene",
            "X_node",
            "X_mesh",
            "X_prim",
            "X_skin",
            "X_mat",
            "KHR_materials_unlit",
            "X_pbr",
            "X_info",
            "KHR_texture_transform",
            "X_normal",
            "EXT_texture_webp",
            "X_image",
            "X_sampler",
            "X_acc",
            "X_sparse",
            "X_idx",
            "X_view",
            "X_buffer",
        ] {
            assert!(names.iter().any(|n| n == expected), "{expected} in {names:?}");
        }
    }

    #[test]
    fn extensions_nested_in_extension_bodies_are_listed_too() {
        let doc: Document = serde_json::from_value(json!({
            "asset": { "version": "2.0" },
            "materials": [{ "extensions": { "VRMC_materials_mtoon": {
                "shadeMultiplyTexture": { "index": 0, "extensions": { "KHR_texture_transform": { "scale": [2, 2] } } },
                "list": [{ "extensions": { "X_deep": {} } }]
            } } }]
        }))
        .unwrap();
        let names: Vec<String> = doc.extension_names().into_iter().collect();
        assert_eq!(names, ["KHR_texture_transform", "VRMC_materials_mtoon", "X_deep"]);
    }

    #[test]
    fn default_roots_follow_the_default_scene_or_the_parentless_nodes() {
        let node = |children: &[usize]| Node { children: children.to_vec(), ..Node::default() };
        let mut doc = Document { nodes: vec![node(&[1]), node(&[]), node(&[])], ..Document::default() };
        assert_eq!(doc.default_roots().unwrap(), [0, 2], "no scenes: nodes nobody parents");
        doc.scenes = vec![Scene { nodes: vec![2], ..Scene::default() }, Scene { nodes: vec![0], ..Scene::default() }];
        assert_eq!(doc.default_roots().unwrap(), [2], "the first scene when none is named");
        doc.scene = Some(1);
        assert_eq!(doc.default_roots().unwrap(), [0]);
        doc.scenes[1].nodes = vec![0, 3];
        let Err(IoError::Malformed { at, .. }) = doc.default_roots() else {
            panic!("a root past the nodes is refused")
        };
        assert_eq!(at.as_deref(), Some("/scenes/1/nodes/1"));
        doc.scenes[1].nodes = vec![0];
        doc.scene = Some(9);
        assert!(doc.default_roots().is_err());
    }

    #[test]
    fn a_texture_for_an_image_is_reused() {
        let mut doc = Document::default();
        assert_eq!(doc.texture_for_image(3), 0);
        assert_eq!(doc.texture_for_image(5), 1);
        assert_eq!(doc.texture_for_image(3), 0, "the existing texture shows image 3");
        assert_eq!(doc.textures[1].source, Some(5));
    }
}
