//! The mock's four registered formats: `glb` and `vrm1`, each an importer and an exporter. They are what the walking
//! skeleton and op-layer tests link against until the real `omuzaki-io-gltf` and `omuzaki-io-vrm` exist; anything linking
//! this crate sees them in [`omuzaki_io_api::formats`].

use omuzaki_io_api::{
    Confidence, ExportCtx, ExportOptions, ExportOutcome, Exporter, Finding, FormatInfo, ImportCtx, ImportInput,
    ImportOutcome, Importer, IoError, IoScene, Layout, LicenceEvidence, OutputFile, Report, VrmVersion,
    register_exporter, register_importer, vrm_profile,
};

use crate::{glb, read, vrm, write};

const MEDIA_TYPE: &str = "model/gltf-binary";

fn glb_info() -> FormatInfo {
    FormatInfo { id: "glb", name: "glTF binary (mock)", extensions: &["glb"], media_type: MEDIA_TYPE }
}

fn vrm1_info() -> FormatInfo {
    FormatInfo { id: "vrm1", name: "VRM 1.0 (mock)", extensions: &["vrm"], media_type: MEDIA_TYPE }
}

/// True if the GLB's JSON chunk has a top-level `extensions.VRMC_vrm`. A real sniffer would stream the JSON; the mock
/// parses it, which is fine for the sizes tests use.
fn has_vrm1_extension(bytes: &[u8]) -> bool {
    let Ok((json, _)) = glb::split(bytes) else { return false };
    serde_json::from_slice::<serde_json::Value>(json).is_ok_and(|v| v.pointer("/extensions/VRMC_vrm").is_some())
}

fn single_file(path: &str, bytes: Vec<u8>, report: Report) -> ExportOutcome {
    let mut out =
        ExportOutcome::new(vec![OutputFile { path: path.to_owned(), media_type: MEDIA_TYPE.to_owned(), bytes }]);
    out.report = report;
    out
}

fn layout_check(options: &ExportOptions, what: &str) -> Report {
    let mut r = Report::new();
    if options.layout != Layout::Binary {
        r.push(
            Finding::error("gltf.layout_unsupported", format!("the mock writes {what} as a single binary file only"))
                .hint("use the binary layout"),
        );
    }
    r
}

/// Plain glTF binary import.
#[derive(Default)]
pub struct MockGlbImporter;

impl Importer for MockGlbImporter {
    fn info(&self) -> FormatInfo {
        glb_info()
    }

    fn sniff(&self, input: &ImportInput<'_>) -> Confidence {
        if glb::is_glb(input.bytes) { Confidence::Possible } else { Confidence::No }
    }

    fn import(&self, input: &ImportInput<'_>, ctx: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
        ctx.progress.checkpoint("read glTF", 0.0)?;
        let mut report = Report::new();
        let parsed = read::read(input.bytes, &ctx.options.limits, &mut report)?;
        // The mock reads no licence data from a plain glTF. If the file is a VRM, say that its licence was left behind.
        let licence = if parsed.gltf.extensions.contains_key("VRMC_vrm") {
            report.push(
                Finding::info("gltf.extension_dropped", "VRMC_vrm avatar data was ignored; import as vrm1 to keep it")
                    .at("/extensions/VRMC_vrm"),
            );
            LicenceEvidence::PresentNotImported {
                summary: "the VRMC_vrm meta (VRM 1.0 avatar)".to_owned(),
                hint_format: "vrm1",
            }
        } else {
            LicenceEvidence::Absent
        };
        let mut out = ImportOutcome::new(parsed.scene, licence);
        out.report = report;
        Ok(out)
    }
}

/// Plain glTF binary export.
#[derive(Default)]
pub struct MockGlbExporter;

impl Exporter for MockGlbExporter {
    fn info(&self) -> FormatInfo {
        glb_info()
    }

    fn check(&self, scene: &IoScene, options: &ExportOptions) -> Report {
        let mut r = layout_check(options, "glTF");
        if scene.avatar.is_some() {
            r.push(
                Finding::info(
                    "gltf.avatar_dropped",
                    "humanoid, expressions, look-at, first-person and spring bones are VRM data; export as vrm1 to keep them",
                )
                .at("avatar"),
            );
        }
        r
    }

    fn export(&self, scene: &IoScene, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError> {
        let mut report = Report::new();
        let written = write::write(scene, ctx.options.generator.as_deref(), &mut report);
        let json = serde_json::to_vec(&written.gltf).map_err(|e| IoError::Internal(e.to_string()))?;
        Ok(single_file("model.glb", glb::join(&json, &written.bin), report))
    }
}

/// VRM 1.0 import.
#[derive(Default)]
pub struct MockVrm1Importer;

impl Importer for MockVrm1Importer {
    fn info(&self) -> FormatInfo {
        vrm1_info()
    }

    fn sniff(&self, input: &ImportInput<'_>) -> Confidence {
        if has_vrm1_extension(input.bytes) { Confidence::Certain } else { Confidence::No }
    }

    fn import(&self, input: &ImportInput<'_>, ctx: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
        ctx.progress.checkpoint("read VRM 1.0", 0.0)?;
        let mut report = Report::new();
        let mut parsed = read::read(input.bytes, &ctx.options.limits, &mut report)?;
        vrm::read(&parsed.gltf, &mut parsed.scene, &mut report)?;
        let mut out = ImportOutcome::new(parsed.scene, LicenceEvidence::Imported);
        out.report = report;
        Ok(out)
    }
}

/// VRM 1.0 export.
#[derive(Default)]
pub struct MockVrm1Exporter;

impl Exporter for MockVrm1Exporter {
    fn info(&self) -> FormatInfo {
        vrm1_info()
    }

    fn check(&self, scene: &IoScene, options: &ExportOptions) -> Report {
        let mut r = layout_check(options, "VRM");
        r.extend(vrm_profile(scene, VrmVersion::V1));
        r
    }

    fn export(&self, scene: &IoScene, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError> {
        let mut report = Report::new();
        let mut written = write::write(scene, ctx.options.generator.as_deref(), &mut report);
        vrm::write(scene, &mut written.gltf, &mut report);
        let json = serde_json::to_vec(&written.gltf).map_err(|e| IoError::Internal(e.to_string()))?;
        Ok(single_file("model.vrm", glb::join(&json, &written.bin), report))
    }
}

register_importer!(MockGlbImporter);
register_exporter!(MockGlbExporter);
register_importer!(MockVrm1Importer);
register_exporter!(MockVrm1Exporter);
