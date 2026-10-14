//! A parsed glTF document to an [`IoScene`].
//!
//! The stages, in the order their output is needed: buffers, images and textures, materials, meshes, skins and nodes.
//! Everything the scene can't hold is dropped *with a finding*; everything that would make the result wrong is an
//! error. The hooks ([`ImportHooks`]) are consulted for the format's own extensions at each step.

mod appearance;
mod extensions;
mod geometry;
mod nodes;

use omuzaki_io_api::{ImportCtx, ImportInput, ImportOutcome, IoError, IoScene, LicenceEvidence, Report};

use self::appearance::Appearance;
pub use self::appearance::TextureMap;
#[cfg(test)]
pub(crate) use self::appearance::sampler_of;
use self::geometry::Geometry;
use crate::accessor::Accessors;
use crate::buffers::{Buffers, Loader};
use crate::fraction;
use crate::hooks::{Carrier, ImportHooks, MaterialCx, MeshCx, NodeCx, RootCx};
use crate::parse::{Parsed, parse};
use crate::schema::Document;
use crate::tally::Tally;

/// A scene read from a document, with the findings so far still open: the format can add its own to the tally before
/// turning it into a report.
pub struct Imported {
    /// The scene.
    pub scene: IoScene,
    /// The findings, merged.
    pub tally: Tally,
}

impl Imported {
    /// Finishes the tally into a report and wraps scene and report. `licence` is what the format found about the file's
    /// licence ([`LicenceEvidence`]): this library reads no licence terms itself, so the format that owns the
    /// extensions says whether it read them, left them behind or found none.
    pub fn into_outcome(self, licence: LicenceEvidence) -> ImportOutcome {
        let mut report = Report::new();
        self.tally.finish(&mut report);
        let mut outcome = ImportOutcome::new(self.scene, licence);
        outcome.report = report;
        outcome
    }
}

/// Imports a GLB or a text glTF with `hooks`: [`parse`], then [`read_scene`], then the report. This is the whole of an
/// `Importer::import` for a format that needs nothing in between and whose `licence` evidence is known up front. A
/// format whose hooks find the evidence while reading calls the three steps itself and passes it to
/// [`Imported::into_outcome`].
///
/// # Errors
/// Whatever [`parse`] and [`read_scene`] return, or [`IoError::Cancelled`].
pub fn import(
    input: &ImportInput<'_>,
    ctx: &ImportCtx<'_>,
    hooks: &mut dyn ImportHooks,
    licence: LicenceEvidence,
) -> Result<ImportOutcome, IoError> {
    ctx.progress.checkpoint("read glTF", 0.0)?;
    let parsed = parse(input.bytes, &ctx.options.limits)?;
    read_scene(&parsed, ctx, hooks).map(|imported| imported.into_outcome(licence))
}

/// Reads the scene out of `parsed`, with arenas (nodes, meshes, skins, materials, images and textures) in the file's
/// order: an index inside an extension body means the same object in the scene as in the file. (Images and textures
/// keep their order only if nothing is dropped, which [`Unusable::Refuse`](crate::Unusable::Refuse) guarantees.)
///
/// Sizes the file declares are checked against `ctx.options.limits` before anything is allocated from them. `ctx`
/// supplies the resolver for sidecar files and the progress and cancellation handle.
///
/// # Errors
/// [`IoError::Unsupported`] for a required extension nobody reads, [`IoError::Malformed`] for data that contradicts
/// itself, [`IoError::Limit`] when a size is over the limits, [`IoError::Resource`] for a sidecar file the host
/// can't give, [`IoError::Cancelled`], and whatever the hooks return.
pub fn read_scene(parsed: &Parsed<'_>, ctx: &ImportCtx<'_>, hooks: &mut dyn ImportHooks) -> Result<Imported, IoError> {
    let doc = &parsed.document;
    let limits = &ctx.options.limits;
    parsed.check_required(hooks.supported_extensions())?;

    let mut tally = Tally::default();
    let mut loader = Loader::new(ctx.resolver, limits.max_input_bytes);
    let buffers = Buffers::load(doc, parsed.chunk, &mut loader)?;
    let acc = Accessors::new(doc, &buffers);
    ctx.progress.checkpoint("read textures", 0.1)?;
    let appearance = Appearance::load(doc, &acc, &mut loader, hooks.unusable(), &mut tally)?;

    let mut scene = IoScene::new();
    for (i, def) in doc.materials.iter().enumerate() {
        let mut material = appearance.material(i, def, hooks, &mut tally);
        hooks.material(&mut MaterialCx {
            index: i,
            def,
            material: &mut material,
            textures: &appearance.map,
            tally: &mut tally,
        })?;
        scene.add_material(material);
    }
    scene.textures = appearance.textures;
    scene.images = appearance.images;

    let mut geometry = Geometry::new(acc, limits);
    for (i, def) in doc.meshes.iter().enumerate() {
        ctx.progress.checkpoint("read meshes", 0.2 + 0.6 * fraction(i, doc.meshes.len()))?;
        let mut mesh = geometry.mesh(i, def, hooks, &mut tally)?;
        hooks.mesh(&mut MeshCx { index: i, def, mesh: &mut mesh, tally: &mut tally })?;
        scene.add_mesh(mesh);
    }
    for (i, def) in doc.skins.iter().enumerate() {
        scene.add_skin(geometry.skin(i, def)?);
    }
    ctx.progress.checkpoint("read nodes", 0.9)?;
    for (i, def) in doc.nodes.iter().enumerate() {
        let mut node = nodes::node(i, def, hooks, &mut tally)?;
        hooks.node(&mut NodeCx { index: i, def, node: &mut node, tally: &mut tally })?;
        scene.add_node(node);
    }
    scene.roots = nodes::roots(doc)?;
    describe_asset(doc, &mut scene, hooks, &mut tally);
    extensions::report_unmapped(doc, &mut tally);
    hooks.root(&mut RootCx { document: doc, scene: &mut scene, tally: &mut tally })?;
    Ok(Imported { scene, tally })
}

/// The file-level facts: generator, copyright, root extensions and extras, and what the scene has no place for.
fn describe_asset(doc: &Document, scene: &mut IoScene, hooks: &mut dyn ImportHooks, tally: &mut Tally) {
    scene.source_generator.clone_from(&doc.asset.generator);
    scene.meta.copyright_information.clone_from(&doc.asset.copyright);
    scene.passthrough.extensions = extensions::carried(doc.extensions.clone(), Carrier::Root, "", hooks, tally);
    scene.passthrough.extras.clone_from(&doc.extras);
    if !doc.animations.is_empty() {
        let msg = format!("{} animation(s) were dropped: the scene holds no animation", doc.animations.len());
        tally.warn("gltf.animations_dropped", msg, "/animations", None);
    }
    if !doc.cameras.is_empty() {
        tally.info("gltf.cameras_dropped", format!("{} camera(s) were dropped", doc.cameras.len()), "/cameras");
    }
    if doc.scenes.len() > 1 {
        let msg =
            format!("{} other scene(s) were ignored; only the default scene's roots are kept", doc.scenes.len() - 1);
        tally.info("gltf.scenes_dropped", msg, "/scenes");
    }
}
