//! The four registered formats: `vrm1` and `vrm0`, each an importer and an exporter.
//!
//! Everything that is glTF (the container, accessors, meshes, skins, textures, plain materials, limits, determinism)
//! is `omuzaki-gltf`'s. Each format here is a set of hooks (`vrm1`, `vrm0`) that gives the VRM extensions their meaning,
//! plus the checks and findings that are VRM's.

use omuzaki_gltf::{Imported, NoHooks};
use omuzaki_io_api::{
    Confidence, ExportCtx, ExportOptions, ExportOutcome, Exporter, Finding, FormatInfo, ImportCtx, ImportInput,
    ImportOutcome, Importer, IoError, IoScene, Layout, LicenceEvidence, Report, VrmVersion, register_exporter,
    register_importer, vrm_profile,
};

use crate::{avatar, orientation, pose, vrm0, vrm1};

/// A `.vrm` is a binary glTF.
const MEDIA_TYPE: &str = "model/gltf-binary";

/// The name the single file of an export gets; the host renames it.
const FILE: &str = "model.vrm";

fn vrm1_info() -> FormatInfo {
    FormatInfo { id: "vrm1", name: "VRM 1.0", extensions: &["vrm"], media_type: MEDIA_TYPE }
}

fn vrm0_info() -> FormatInfo {
    FormatInfo { id: "vrm0", name: "VRM 0.x", extensions: &["vrm"], media_type: MEDIA_TYPE }
}

/// `Certain` if the file declares the extension `marker` at the root, else `No`: a plain glTF isn't a VRM.
fn sniff_extension(input: &ImportInput<'_>, marker: &str) -> Confidence {
    match omuzaki_gltf::probe(input.bytes) {
        Some(probe) if probe.root_extensions.iter().any(|n| n == marker) => Confidence::Certain,
        _ => Confidence::No,
    }
}

/// Everything an export checks before writing: a VRM is one binary file, the VRM profile holds, and the scene can be
/// written as valid glTF. (The model's own licence is checked by `preflight`, for every format.)
fn check(scene: &IoScene, options: &ExportOptions, version: VrmVersion) -> Report {
    let mut r = Report::new();
    if options.layout != Layout::Binary {
        r.push(
            Finding::error("gltf.layout_unsupported", "a VRM is a single binary glTF; separate files aren't allowed")
                .hint("use the binary layout"),
        );
    }
    r.extend(vrm_profile(scene, version));
    r.extend(pose::check(scene));
    r.extend(omuzaki_gltf::check(scene, &mut NoHooks));
    r
}

/// Finishes an import: the avatar layer's findings follow the glTF layer's, then the profile and perfect-sync notes.
/// Both VRM versions read the file's licence data into the scene's meta, hence [`LicenceEvidence::Imported`].
fn finish_import(imported: Imported, avatar_findings: Report, version: VrmVersion) -> ImportOutcome {
    let mut outcome = imported.into_outcome(LicenceEvidence::Imported);
    outcome.report.extend(avatar_findings);
    avatar::assess(&outcome.scene, version, &mut outcome.report);
    outcome
}

/// Finishes an export: the avatar layer's findings follow the glTF layer's, and the single file gets its name.
fn finish_export(written: omuzaki_gltf::Written, avatar_findings: Report) -> Result<ExportOutcome, IoError> {
    let mut outcome = written.into_outcome()?;
    outcome.report.extend(avatar_findings);
    if let Some(file) = outcome.files.first_mut() {
        FILE.clone_into(&mut file.path);
    }
    Ok(outcome)
}

/// VRM 1.0 import.
#[derive(Default)]
pub struct Vrm1Importer;

impl Importer for Vrm1Importer {
    fn info(&self) -> FormatInfo {
        vrm1_info()
    }

    fn sniff(&self, input: &ImportInput<'_>) -> Confidence {
        sniff_extension(input, vrm1::EXTENSION)
    }

    fn import(&self, input: &ImportInput<'_>, ctx: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
        ctx.progress.checkpoint("read VRM 1.0", 0.0)?;
        let parsed = omuzaki_gltf::parse(input.bytes, &ctx.options.limits)?;
        let mut hooks = vrm1::Import::default();
        let imported = omuzaki_gltf::read_scene(&parsed, ctx, &mut hooks)?;
        Ok(finish_import(imported, hooks.report, VrmVersion::V1))
    }
}

/// VRM 1.0 export.
#[derive(Default)]
pub struct Vrm1Exporter;

impl Exporter for Vrm1Exporter {
    fn info(&self) -> FormatInfo {
        vrm1_info()
    }

    fn check(&self, scene: &IoScene, options: &ExportOptions) -> Report {
        check(scene, options, VrmVersion::V1)
    }

    fn export(&self, scene: &IoScene, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError> {
        let mut hooks = vrm1::Export::new(scene);
        let written = omuzaki_gltf::write_scene(scene, ctx, &mut hooks)?;
        finish_export(written, hooks.report)
    }
}

/// VRM 0.x import. The model is rotated to face +Z, as the scene requires.
#[derive(Default)]
pub struct Vrm0Importer;

impl Importer for Vrm0Importer {
    fn info(&self) -> FormatInfo {
        vrm0_info()
    }

    fn sniff(&self, input: &ImportInput<'_>) -> Confidence {
        sniff_extension(input, vrm0::EXTENSION)
    }

    fn import(&self, input: &ImportInput<'_>, ctx: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
        ctx.progress.checkpoint("read VRM 0.x", 0.0)?;
        let parsed = omuzaki_gltf::parse(input.bytes, &ctx.options.limits)?;
        let mut hooks = vrm0::Import::new(input.name);
        let imported = omuzaki_gltf::read_scene(&parsed, ctx, &mut hooks)?;
        Ok(finish_import(imported, hooks.report, VrmVersion::V0))
    }
}

/// VRM 0.x export. A copy of the scene is rotated to face −Z before it is written.
#[derive(Default)]
pub struct Vrm0Exporter;

impl Exporter for Vrm0Exporter {
    fn info(&self) -> FormatInfo {
        vrm0_info()
    }

    fn check(&self, scene: &IoScene, options: &ExportOptions) -> Report {
        check(scene, options, VrmVersion::V0)
    }

    fn export(&self, scene: &IoScene, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError> {
        let mut facing_back = scene.clone();
        orientation::rotate_y180(&mut facing_back);
        let mut hooks = vrm0::Export::default();
        let written = omuzaki_gltf::write_scene(&facing_back, ctx, &mut hooks)?;
        finish_export(written, hooks.report)
    }
}

register_importer!(Vrm1Importer);
register_exporter!(Vrm1Exporter);
register_importer!(Vrm0Importer);
register_exporter!(Vrm0Exporter);
