//! [`IoScene`] to glTF 2.0: a GLB, or a `.gltf` with its `.bin` and image files.
//!
//! **Deterministic.** Arrays are written in arena order, accessors in a fixed order, JSON keys come from structs and
//! sorted maps, and nothing depends on time, addresses or hash order. The same scene and options give the same bytes,
//! and `export(import(export(x))) == export(x)`: the reader reads back exactly what this module writes.
//!
//! **Arenas keep their order.** Every texture, image, material, mesh and node is written, in scene order, whether or not
//! anything uses it, so the raw-JSON pass-through of unknown extensions (which holds file indices) stays valid.
//!
//! **Explicit values.** Everything that shapes appearance is written out (samplers with every field, material
//! factors, alpha mode), so no reader has to guess a default. Identity transforms are omitted.
//!
//! The format's own data goes in through [`ExportHooks`], and through [`Written`], which a caller can add to before
//! it is turned into files.

mod appearance;
mod meshes;

use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_io_api::{
    ExportCtx, ExportOutcome, IoError, IoScene, Layout, MeshIx, NodeIx, OutputFile, Passthrough, Report, SkinIx,
};

pub use self::appearance::{split_emissive, texture_info};
use crate::container;
use crate::hooks::ExportHooks;
use crate::pack::Packer;
use crate::schema::{self as gltf, Asset, Buffer, Document, Extensions, Scene};
use crate::tally::Tally;

/// The base name of every file an export writes; the host renames them.
const BASE: &str = "model";

/// A document being written: the typed JSON, the binary buffer it will index and the image files of the separate
/// layout. Add the format's own data to [`Written::document`] (and bytes to [`Written::pack`]) before calling
/// [`Written::into_outcome`], which lists the extensions used and serialises.
pub struct Written {
    /// The document. `buffers` and `extensionsUsed` are set by [`Written::into_outcome`].
    pub document: Document,
    /// The accessors, buffer views and bytes written so far; `into_outcome` moves them into the document.
    pub pack: Packer,
    /// The findings so far. The format can add its own before the outcome is built.
    pub tally: Tally,
    layout: Layout,
    /// Image files, for the separate layout.
    files: Vec<OutputFile>,
}

impl Written {
    /// The texture that shows `image`, adding one if there is none yet (see [`Document::texture_for_image`]).
    pub fn texture_for_image(&mut self, image: usize) -> usize {
        self.document.texture_for_image(image)
    }

    /// Lists the extensions used, writes the buffer and serialises: one GLB, or a `.gltf` with `.bin` and images.
    ///
    /// # Errors
    /// [`IoError::Unsupported`] when a single GLB would pass 4 GiB, [`IoError::Internal`] if the JSON can't be written.
    pub fn into_outcome(mut self) -> Result<ExportOutcome, IoError> {
        self.document.extensions_used = self.document.extension_names().into_iter().collect();
        let Packer { bin, views, accessors } = self.pack;
        self.document.accessors = accessors;
        self.document.buffer_views = views;
        if !bin.is_empty() {
            let uri = (self.layout == Layout::Separate).then(|| format!("{BASE}.bin"));
            self.document.buffers = vec![Buffer { uri, byte_length: bin.len(), ..Buffer::default() }];
        }
        let mut report = Report::new();
        self.tally.finish(&mut report);
        let mut outcome = ExportOutcome::new(output_files(&self.document, bin, self.files, self.layout)?);
        outcome.report = report;
        Ok(outcome)
    }
}

/// Everything a glTF writer accumulates while it walks the scene. The stages that fill it live beside the data they
/// write.
struct Writer<'s> {
    scene: &'s IoScene,
    hooks: &'s mut dyn ExportHooks,
    out: Written,
}

impl Writer<'_> {
    /// Merges a pass-through bag into an object's extension map. A name the writer already wrote for that object wins.
    fn carry(&mut self, bag: &Passthrough, extensions: &mut Extensions, at: &str) {
        for (name, value) in &bag.extensions {
            if extensions.contains_key(name) {
                let msg =
                    format!("pass-through extension {name} clashes with one this writer produces and was dropped");
                self.out.tally.warn("gltf.passthrough_conflict", msg, format!("{at}/extensions/{name}"), None);
            } else {
                extensions.insert(name.clone(), value.clone());
            }
        }
    }

    fn nodes(&mut self) {
        for (i, node) in self.scene.nodes.iter().enumerate() {
            let t = &node.transform;
            let mut extensions = Extensions::new();
            self.carry(&node.passthrough, &mut extensions, &format!("/nodes/{i}"));
            let mut def = gltf::Node {
                name: node.name.clone(),
                children: node.children.iter().map(|c| c.index()).collect(),
                mesh: node.mesh.map(MeshIx::index),
                // glTF only skins meshes; `check` reports a skin set on a node without one.
                skin: node.skin.filter(|_| node.mesh.is_some()).map(SkinIx::index),
                translation: (t.translation != Vec3::ZERO).then(|| t.translation.to_array()),
                rotation: (t.rotation != Quat::IDENTITY).then(|| t.rotation.to_array()),
                scale: (t.scale != Vec3::ONE).then(|| t.scale.to_array()),
                extensions,
                extras: node.passthrough.extras.clone(),
                ..gltf::Node::default()
            };
            self.hooks.node(i, node, &mut def);
            self.out.document.nodes.push(def);
        }
    }

    fn skins(&mut self) {
        for skin in &self.scene.skins {
            let matrices = (!skin.inverse_bind.is_empty()).then(|| self.out.pack.matrices(&skin.inverse_bind));
            self.out.document.skins.push(gltf::Skin {
                name: skin.name.clone(),
                joints: skin.joints.iter().map(|j| j.index()).collect(),
                inverse_bind_matrices: matrices,
                skeleton: skin.skeleton.map(NodeIx::index),
                ..gltf::Skin::default()
            });
        }
    }

    /// The asset block, scenes and root extensions.
    fn finish(&mut self, generator: Option<&str>) {
        let scene = self.scene;
        let mut root = Extensions::new();
        self.carry(&scene.passthrough, &mut root, "");
        let doc = &mut self.out.document;
        doc.extensions = root;
        doc.extras.clone_from(&scene.passthrough.extras);
        doc.asset = Asset {
            version: "2.0".into(),
            generator: generator.map(str::to_owned),
            copyright: scene.meta.copyright_information.clone(),
            ..Asset::default()
        };
        doc.scene = Some(0);
        doc.scenes = vec![Scene { nodes: scene.roots.iter().map(|r| r.index()).collect(), ..Scene::default() }];
        self.hooks.root(scene, doc, &mut self.out.tally);
    }
}

/// Writes the generic parts of `scene` (textures, materials, meshes with morph targets, skins, nodes, images) and
/// returns the document to add the format's own data to. Findings that need only the scene (`MToon` written as PBR,
/// an avatar left out) come from [`check`](crate::check), which `run_export` calls first; this adds what only writing
/// can notice.
///
/// # Errors
/// [`IoError::Cancelled`] when the progress handle says so.
pub fn write_scene(scene: &IoScene, ctx: &ExportCtx<'_>, hooks: &mut dyn ExportHooks) -> Result<Written, IoError> {
    let mut w = Writer {
        scene,
        hooks,
        out: Written {
            document: Document::default(),
            pack: Packer::default(),
            tally: Tally::default(),
            layout: ctx.options.layout,
            files: Vec::new(),
        },
    };
    ctx.progress.checkpoint("write materials", 0.05)?;
    w.textures();
    w.materials();
    w.meshes(ctx)?;
    w.skins();
    ctx.progress.checkpoint("write nodes and images", 0.9)?;
    w.nodes();
    w.images();
    w.finish(ctx.options.generator.as_deref());
    Ok(w.out)
}

/// Writes `scene` with `hooks`: [`write_scene`], then the files. This is the whole of an `Exporter::export` for a format
/// that adds nothing in between.
///
/// # Errors
/// As [`write_scene`] and [`Written::into_outcome`].
pub fn export(scene: &IoScene, ctx: &ExportCtx<'_>, hooks: &mut dyn ExportHooks) -> Result<ExportOutcome, IoError> {
    write_scene(scene, ctx, hooks)?.into_outcome()
}

fn json_error(e: &serde_json::Error) -> IoError {
    IoError::Internal(format!("couldn't serialise the glTF JSON: {e}"))
}

/// The files for a finished document: the GLB, or the `.gltf`, its `.bin` and the image files.
fn output_files(
    doc: &Document,
    bin: Vec<u8>,
    images: Vec<OutputFile>,
    layout: Layout,
) -> Result<Vec<OutputFile>, IoError> {
    match layout {
        Layout::Binary => {
            let json = serde_json::to_vec(doc).map_err(|e| json_error(&e))?;
            let bytes = container::join(&json, &bin)?;
            Ok(vec![OutputFile { path: format!("{BASE}.glb"), media_type: "model/gltf-binary".into(), bytes }])
        }
        Layout::Separate => {
            let json = serde_json::to_vec_pretty(doc).map_err(|e| json_error(&e))?;
            let main = OutputFile { path: format!("{BASE}.gltf"), media_type: "model/gltf+json".into(), bytes: json };
            let mut files = vec![main];
            if !bin.is_empty() {
                let media_type = "application/octet-stream".into();
                files.push(OutputFile { path: format!("{BASE}.bin"), media_type, bytes: bin });
            }
            files.extend(images);
            Ok(files)
        }
    }
}

/// What a glTF file can't carry or can't write, found from the scene alone: meshes and skins that are empty, numbers
/// that would corrupt the JSON, and whatever `hooks` adds for its format (a skin on a node with no mesh is dropped by
/// the writer, and said so here).
pub fn check(scene: &IoScene, hooks: &mut dyn ExportHooks) -> Report {
    let mut tally = Tally::default();
    hooks.check_scene(scene, &mut tally);
    for (i, node) in scene.nodes.iter().enumerate() {
        if node.skin.is_some() && node.mesh.is_none() {
            let msg = "a skin on a node with no mesh was dropped: glTF skins meshes only";
            tally.warn("gltf.skin_dropped", msg, format!("nodes/{i}/skin"), None);
        }
    }
    appearance::check(scene, hooks, &mut tally);
    meshes::check(scene, &mut tally);
    let mut report = Report::new();
    tally.finish(&mut report);
    report
}
