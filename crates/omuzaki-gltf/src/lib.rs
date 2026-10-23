//! The shared glTF 2.0 format library (spec v3 §27): what `omuzaki-io-gltf` (plain glTF and GLB) and `omuzaki-io-vrm`
//! (VRM 1.0 and 0.x) both build on, so the many rules a glTF reader and writer must get right exist once.
//!
//! A VRM file *is* a glTF file with `VRMC_*` extensions on top. This crate owns everything that is glTF, in two
//! layers, and gives the format crates hooks for what is not.
//!
//! # The document layer
//!
//! - **[`schema`]**: the glTF JSON as typed serde structs, one set for reading and writing. Every object keeps its raw
//!   `extensions` and `extras`; every array is in file order with the file's own indices.
//! - **[`container`], [`uri`], [`buffers`]**: the GLB layout, `data:` URIs and sidecar files (through the host's
//!   [`Resolver`](omuzaki_io_api::Resolver); network and absolute URLs are refused), buffers cut to their declared length.
//! - **[`accessor`]**: accessors decoded into typed arrays: interleaved (strided), sparse, normalised and quantised
//!   integers, matrix column padding. Counts are checked against a [`Bound`](accessor::Bound) and offsets with
//!   checked arithmetic *before* anything is allocated, and readers never index, so a lie in a file is an error.
//! - **[`parse`] and [`probe`]**: open a file (checking sizes, the version and `extensionsRequired`), or recognise one
//!   cheaply for `Importer::sniff`.
//! - **[`animation`]**: typed animations (samplers, channels, accessors), read on request and written
//!   deterministically; the document itself keeps them as raw JSON.
//! - **[`pack`]**: the writer's side of accessors: aligned views, bounds, 16-bit indices when they fit, sparse morph
//!   targets when smaller.
//!
//! # The scene layer
//!
//! - **[`read_scene`]** turns a [`Parsed`] document into an [`IoScene`](omuzaki_io_api::IoScene): meshes with morph
//!   targets and several skinning sets (folded to the four strongest influences), strips and fans as triangle lists,
//!   materials (`KHR_texture_transform`, `KHR_materials_unlit`, `KHR_materials_emissive_strength`), PNG and JPEG
//!   images with their original bytes, samplers, nodes (matrices decomposed) and skins.
//! - **[`write_scene`]** does the reverse, deterministically: `export(import(export(x))) == export(x)`.
//! - **[`check`]** finds what can't be written from the scene alone.
//!
//! # Hooks
//!
//! [`ImportHooks`] and [`ExportHooks`] are how a format crate adds its own extensions: it says which extensions it
//! reads itself and which to drop, and gets each node, mesh and material (with the file's raw JSON and index) to
//! finish. [`NoHooks`] is plain glTF. See [`hooks`].
//!
//! # Rules
//!
//! - **Arena order.** Nodes, meshes, skins and materials keep the file's order, and so do images and textures unless one
//!   is dropped as unusable (`gltf.image_dropped`, `gltf.texture_dropped`; a format that indexes into them chooses
//!   [`Unusable::Refuse`]). The writer writes every arena in scene order for the same reason.
//! - **Safe on untrusted input.** Sizes the file declares are checked against [`Limits`](omuzaki_io_api::Limits) before any
//!   allocation follows them, `data:` URIs and sidecar files are bounded, and no input can make the reader panic (the
//!   tests truncate and mutate real files, and replace values throughout their JSON with nasty ones). That includes
//!   the length of every top-level array (`materials`, `accessors`, ...), counted before the typed parse builds any of
//!   them: a typed element is hundreds of times the `{}` that names it. Nodes and images have limits of their own;
//!   any other array may hold as many elements as fit in the memory `max_nodes` nodes take, and at least 4096.
//! - **Transforms are data.** Translations, scales and inverse bind matrices are read and written exactly. A rotation is
//!   normalised only when it is further than 1e-6 from unit length, so a file this crate wrote reads back bit for bit.
//! - **Sidecar URIs.** A relative `uri` is percent-decoded and handed to the resolver as a path. Whether `..` or an
//!   absolute path is allowed is the host's decision, made where it checks the `fs` grant.
//!
//! # Findings
//!
//! Rule ids are part of the contract (`omuzaki-io-api` report rules). `gltf.*` rules this crate emits, besides the
//! contract's own `gltf.passthrough`:
//!
//! | Rule | Severity | Meaning |
//! |---|---|---|
//! | `gltf.normals_generated` | info | the file had no `NORMAL`; flat normals were made (vertices un-shared) |
//! | `gltf.attribute_dropped` | info | a vertex attribute the scene has no place for (`TANGENT`, `TEXCOORD_1`+, `COLOR_1`+, unusable `_NAME`, skinning sets after a gap) |
//! | `gltf.primitive_converted` | info | a triangle strip or fan became a triangle list |
//! | `gltf.primitive_dropped` | warning | points, lines, or a primitive with no vertices or triangles |
//! | `gltf.indices_truncated` | warning | trailing indices short of a whole triangle were dropped |
//! | `gltf.joints_folded` | warning | several skinning sets were folded to the four strongest influences (or sets past `JOINTS_3` ignored) |
//! | `gltf.target_names_dropped` | warning | `targetNames` didn't match the number of morph targets |
//! | `gltf.morph_weights_dropped` | warning | non-zero default morph weights on a mesh or node |
//! | `gltf.extras_dropped` | info | mesh `extras` other than `targetNames` |
//! | `gltf.extension_dropped` | info or warning | an extension with no place in the scene (on a mesh, primitive, texture, accessor, ...), one the format asked to drop, or a buffer-bound one |
//! | `gltf.image_dropped`, `gltf.texture_dropped` | warning | an image that isn't PNG or JPEG, a texture with no usable image |
//! | `gltf.texture_transform_dropped` | warning | `KHR_texture_transform.rotation` |
//! | `gltf.alpha_mode_unknown` | warning | an `alphaMode` that isn't `OPAQUE`, `MASK` or `BLEND` |
//! | `gltf.occlusion_dropped` | info | an ambient occlusion texture |
//! | `gltf.matrix_decomposed` | info | a node `matrix` became translation, rotation and scale |
//! | `gltf.matrix_approximated`, `gltf.node_transform_ambiguous` | warning | a matrix with shear or none to speak of; both a matrix and TRS on one node |
//! | `gltf.animations_dropped` | warning | the scene holds no animation |
//! | `gltf.cameras_dropped`, `gltf.scenes_dropped` | info | cameras; scenes other than the default |
//! | `gltf.skin_dropped` | warning | (export) a skin on a node without a mesh |
//! | `gltf.passthrough_conflict` | warning | (export) a carried extension clashed with one the writer produces |
//! | `gltf.non_finite`, `gltf.mesh_empty`, `gltf.skin_empty` | error | (export) the scene can't be written as valid glTF |
//!
//! The plain glTF format adds `gltf.avatar_dropped` and `gltf.toon_dropped` on export and the `VRMC_*` drop warning
//! on import (`omuzaki-io-gltf`); other formats add their own through the hooks.
#![deny(missing_docs)]

pub mod accessor;
pub mod animation;
pub mod buffers;
pub mod container;
pub mod hooks;
pub mod pack;
mod parse;
mod read;
pub mod schema;
pub mod tally;
pub mod uri;
mod write;

pub use hooks::{
    Carrier, Disposition, ExportHooks, ImportHooks, MaterialCx, MeshCx, NoHooks, NodeCx, RootCx, TargetNames, Unusable,
};
pub use parse::{BUFFER_BOUND_EXTENSIONS, MODELLED_EXTENSIONS, Parsed, Probe, parse, probe, trim_json};
pub use read::{Imported, TextureMap, import, read_scene};
pub use schema::Document;
pub use tally::Tally;
pub use write::{Written, check, export, glb, split_emissive, texture_info, write_scene};

/// `i` out of `n` as a progress fraction.
#[allow(clippy::cast_precision_loss)] // progress only needs a few digits
fn fraction(i: usize, n: usize) -> f32 {
    i as f32 / n.max(1) as f32
}

/// A file index as the scene's `u32`. An index past `u32::MAX` can't be real; saturating makes it fail the scene's
/// range check instead of wrapping onto a valid one.
pub(crate) fn ix32(i: usize) -> u32 {
    u32::try_from(i).unwrap_or(u32::MAX)
}

/// `value` as a JSON tree whose `f32` numbers print the way an `f32` should (`0.05`, not `0.05000000074505806`).
///
/// `serde_json::to_value` widens every `f32` to its exact `f64`, which is correct but doubles the digits in the
/// file and differs from what every other tool writes. Going through text uses the shortest `f32` form, and parsing
/// that back gives the `f64` nearest the same decimal, which reads back as the original `f32`. Use it to build the
/// body of an extension (a [`schema::Extensions`] value) from a typed struct.
///
/// Returns `Value::Null` if `value` can't be serialised (a map with non-string keys); typed structs always can.
pub fn to_json<T: serde::Serialize>(value: &T) -> serde_json::Value {
    serde_json::to_string(value)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn f32_numbers_keep_their_short_form_in_extension_bodies() {
        #[derive(serde::Serialize)]
        struct Body {
            factor: f32,
            offset: [f32; 2],
        }
        let value = to_json(&Body { factor: 0.05, offset: [0.1, 0.3] });
        assert_eq!(value.to_string(), r#"{"factor":0.05,"offset":[0.1,0.3]}"#);
        assert_eq!(value["factor"], json!(0.05));
        assert_eq!(serde_json::to_value(0.05f32).unwrap().to_string(), "0.05000000074505806", "what to_value does");
    }

    #[test]
    fn indices_saturate_instead_of_wrapping() {
        assert_eq!(ix32(7), 7);
        assert_eq!(ix32(usize::MAX), u32::MAX);
    }
}
