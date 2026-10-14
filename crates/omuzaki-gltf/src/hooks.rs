//! How a format built on glTF plugs into the generic reader and writer.
//!
//! A VRM file is a glTF file with `VRMC_*` extensions on the root, on nodes and on materials. The generic code reads
//! and writes everything that is plain glTF; a format layers its own meaning on top through these hooks, without the
//! generic code knowing the format:
//!
//! - **What an extension means.** [`ImportHooks::extension`] says, per extension name and per kind of object, whether
//!   the generic code should carry the raw JSON in the scene's pass-through bag, leave it alone because the format
//!   reads it itself, or drop it with a finding. The plain glTF importer drops `VRMC_*` (with a warning that names the
//!   fix) so a plain export can't smuggle licence terms past the rights rules; the VRM importer interprets them.
//! - **Per object.** After the generic code builds a node, mesh or material, the matching hook receives the file's
//!   raw JSON object (so its extension bodies and the index) next to the scene object it built, and may change it.
//!   `VRMC_materials_mtoon` becomes `Shading::Toon` this way.
//! - **Per file.** [`ImportHooks::root`] runs last, with the whole document and the finished scene.
//! - **What a file may require.** [`ImportHooks::supported_extensions`] extends the list of extensions a file may list
//!   in `extensionsRequired`.
//! - **Arena strictness.** [`ImportHooks::unusable`] chooses between dropping an image or texture the scene can't
//!   hold (the arenas shrink, indices move) and refusing the file (they never move). Formats that refer to images and
//!   textures by file index need the second.
//!
//! The writer has the mirror hooks in [`ExportHooks`]. Every method has a default that does the plain glTF thing, and
//! [`NoHooks`] is exactly that.

use omuzaki_io_api::{IoError, IoScene, Material, Mesh, Node, Severity};
use serde_json::Value;

use crate::read::TextureMap;
use crate::schema;
use crate::tally::Tally;

/// The kinds of object whose extensions the generic reader consults the hooks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Carrier {
    /// The root object of the file.
    Root,
    /// A node.
    Node,
    /// A mesh. Meshes have no pass-through bag, so [`Disposition::Carry`] means "report as dropped".
    Mesh,
    /// A material.
    Material,
}

impl Carrier {
    /// The noun findings use for this kind of object ("scene" for the root).
    pub fn noun(self) -> &'static str {
        match self {
            Self::Root => "scene",
            Self::Node => "node",
            Self::Mesh => "mesh",
            Self::Material => "material",
        }
    }
}

/// What to do with one extension of one object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disposition {
    /// Keep the raw JSON in the scene's pass-through bag for that object and write it back on export. (The generic
    /// code still drops extensions that point into buffer data it re-packs, whatever the hook says.)
    Carry,
    /// The format reads this extension itself: keep it out of the bag and say nothing.
    Interpreted,
    /// Drop it and report why, as a `gltf.extension_dropped` finding.
    Discard {
        /// How serious the loss is.
        severity: Severity,
        /// What was dropped and why.
        message: String,
        /// What to do about it.
        hint: Option<&'static str>,
    },
}

/// What to do with an image, or a texture, the scene can't hold (not a PNG or JPEG, no data, no image source).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Unusable {
    /// Drop it with a warning. Later images and textures move up, and the materials that used a dropped texture lose
    /// that slot.
    #[default]
    Discard,
    /// Refuse the file: nothing is dropped, so images and textures keep their file indices.
    Refuse,
}

/// A node as it was read.
pub struct NodeCx<'a> {
    /// The node's index in the file.
    pub index: usize,
    /// The node as the file has it, extensions included.
    pub def: &'a schema::Node,
    /// The scene node built from it.
    pub node: &'a mut Node,
    /// Where to report findings.
    pub tally: &'a mut Tally,
}

/// A mesh as it was read.
pub struct MeshCx<'a> {
    /// The mesh's index in the file.
    pub index: usize,
    /// The mesh as the file has it, extensions and extras included.
    pub def: &'a schema::Mesh,
    /// The scene mesh built from it.
    pub mesh: &'a mut Mesh,
    /// Where to report findings.
    pub tally: &'a mut Tally,
}

/// A material as it was read.
pub struct MaterialCx<'a> {
    /// The material's index in the file.
    pub index: usize,
    /// The material as the file has it, extensions included.
    pub def: &'a schema::Material,
    /// The scene material built from it (PBR, or unlit if the file says `KHR_materials_unlit`).
    pub material: &'a mut Material,
    /// Maps the file's texture indices to the scene's, for texture references in extension bodies.
    pub textures: &'a TextureMap,
    /// Where to report findings.
    pub tally: &'a mut Tally,
}

/// The whole file once everything generic has been read.
pub struct RootCx<'a> {
    /// The document as parsed.
    pub document: &'a schema::Document,
    /// The scene built from it; root-level data (an avatar) goes here.
    pub scene: &'a mut IoScene,
    /// Where to report findings.
    pub tally: &'a mut Tally,
}

/// What a glTF-based format adds to reading. Every method has the plain-glTF default.
pub trait ImportHooks {
    /// Extensions, beyond the ones this crate models (`KHR_materials_unlit`, `KHR_materials_emissive_strength`,
    /// `KHR_texture_transform`, `KHR_mesh_quantization`), that a file may list in `extensionsRequired` and still be read.
    fn supported_extensions(&self) -> &[&str] {
        &[]
    }

    /// What to do with extension `name` on an object of kind `carrier`, whose body in the file is `body`. Called once
    /// per extension per object. The body is there so a message can say *what* is being dropped (the plain glTF
    /// importer quotes the licence terms of a VRM file it can't keep); most formats decide on the name alone.
    fn extension(&mut self, carrier: Carrier, name: &str, body: &Value) -> Disposition {
        let _ = (carrier, name, body);
        Disposition::Carry
    }

    /// Whether unusable images and textures are dropped or refuse the file.
    fn unusable(&self) -> Unusable {
        Unusable::Discard
    }

    /// Called after each node is built, in file order.
    ///
    /// # Errors
    /// Whatever the format wants to refuse the file for.
    fn node(&mut self, cx: &mut NodeCx<'_>) -> Result<(), IoError> {
        let _ = cx;
        Ok(())
    }

    /// Called after each mesh is built, in file order.
    ///
    /// # Errors
    /// Whatever the format wants to refuse the file for.
    fn mesh(&mut self, cx: &mut MeshCx<'_>) -> Result<(), IoError> {
        let _ = cx;
        Ok(())
    }

    /// Called after each material is built, in file order.
    ///
    /// # Errors
    /// Whatever the format wants to refuse the file for.
    fn material(&mut self, cx: &mut MaterialCx<'_>) -> Result<(), IoError> {
        let _ = cx;
        Ok(())
    }

    /// Called once, last.
    ///
    /// # Errors
    /// Whatever the format wants to refuse the file for.
    fn root(&mut self, cx: &mut RootCx<'_>) -> Result<(), IoError> {
        let _ = cx;
        Ok(())
    }
}

/// Where `extras.targetNames` goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetNames {
    /// On the mesh (VRM 1.0, Blender).
    #[default]
    OnMesh,
    /// On every primitive of the mesh (VRM 0.x exporters).
    OnPrimitives,
}

/// What a glTF-based format adds to writing. Every method has the plain-glTF default; the pass-through bags are
/// merged into the objects *before* the hooks run, so a hook sees (and may override) them.
pub trait ExportHooks {
    /// Where morph target names are written.
    fn target_names(&self) -> TargetNames {
        TargetNames::OnMesh
    }

    /// Findings that need only the scene, run first by [`check`](crate::check).
    fn check_scene(&mut self, scene: &IoScene, tally: &mut Tally) {
        let _ = (scene, tally);
    }

    /// Findings about one material, run in material order by [`check`](crate::check) before the generic ones.
    fn check_material(&mut self, index: usize, material: &Material, tally: &mut Tally) {
        let _ = (index, material, tally);
    }

    /// Called with each node's finished file object. Add extensions here.
    fn node(&mut self, index: usize, node: &Node, def: &mut schema::Node) {
        let _ = (index, node, def);
    }

    /// Called with each mesh's finished file object.
    fn mesh(&mut self, index: usize, mesh: &Mesh, def: &mut schema::Mesh) {
        let _ = (index, mesh, def);
    }

    /// Called with each material's finished file object. A toon material arrives as a matte PBR stand-in
    /// (metallic 0, roughness 1); add the format's own extension and any fallback here. Write the texture references
    /// inside that extension's body with [`texture_info`](crate::texture_info), so a UV transform is written the same
    /// way as in the core slots.
    fn material(&mut self, index: usize, material: &Material, def: &mut schema::Material, tally: &mut Tally) {
        let _ = (index, material, def, tally);
    }

    /// Called last with the finished document (before `extensionsUsed` is computed from it).
    fn root(&mut self, scene: &IoScene, document: &mut schema::Document, tally: &mut Tally) {
        let _ = (scene, document, tally);
    }
}

/// Plain glTF: no extension is interpreted, nothing is refused beyond the generic rules.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoHooks;

impl ImportHooks for NoHooks {}
impl ExportHooks for NoHooks {}
