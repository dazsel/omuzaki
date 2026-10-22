//! `io.export`: gather the character, bake the face with the registered baker, `run_export`, and write the files
//! through the host. A job; a dry run returns the full report and writes nothing.
//!
//! **One verdict.** The export runs `io.check_export`'s check ([`CheckExport::run_scene`]) and refuses on any error it
//! finds, so an export and its check can never disagree. The plan runs it too, so a dry run (and an executor's plan
//! before the job is queued) refuses what the job would, and says whether the file may be written there.
//!
//! **The face.** A kit character's painted face becomes the 52 perfect-sync expressions through the face baker the app
//! registered (`omuzaki_face_api::face_baker`). A build without one, or a character with no painted face, exports the
//! VRM presets only and says so in a finding (`perfect_sync.not_baked`). An imported model carries its own expressions:
//! it keeps perfect sync when it has all 52 channels and exports its presets only otherwise, saying which.
//!
//! **Writing.** The main file goes to `path`, written to a temporary file in its folder and renamed over the old one,
//! so a reader never sees half a file and a failed export leaves the old file as it was. `vrm1`, `vrm0` and `glb` are
//! one binary file each.

use omuzaki_char_api::{CharId, VariantId};
use omuzaki_core::time::SystemClock;
use omuzaki_face_api::{BakerInput, FaceProfile, face_baker};
use omuzaki_io_api::{
    ARKIT_52, BakeReport, BakeRequest, ExportCtx, ExportOptions, ExpressionName, Finding, FormatId, IoScene,
    OutputFile, PerfectSync, Purpose, Report, find_exporter, formats, run_export,
};
use omuzaki_ops_api::{
    CostHint, FsMode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op,
};
use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::Write;

use super::check_export::{CheckExport, ExportCheck, ExportScene, Shape};
use super::progress::JobProgress;
use crate::convert::channels::{channel_for, named_expression};
use crate::error::from_io;

/// What `asset.generator` says in every file Omuzaki writes. No version: the bytes must not change between builds.
pub const GENERATOR: &str = "Omuzaki";

/// Writes a character (built from the kit or imported) to a VRM or glTF file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "format": "vrm1", "path": "exports/kit.vrm"},
        {"character": "kit", "format": "glb", "path": "exports/kit.glb", "perfect_sync": "off", "target_height": 1.6}
    ],
    "x-omuzaki-fixture" = "character-licensed"
))]
pub struct Export {
    /// The character to export.
    pub character: CharId,
    /// The format (see `io.list_formats`): `vrm1` (VRM 1.0), `vrm0` (VRM 0.x) or `glb`.
    pub format: FormatId,
    /// Where to write the file. Its extension must be the format's (`.vrm`, `.glb`). A relative path is inside the
    /// project folder; anywhere else needs the `fs` scope and a granted folder.
    pub path: String,
    /// What the file is for: `private` (the default) for your own machine; `publish` for anything that leaves it, which
    /// the licence gate must pass.
    #[serde(default)]
    pub purpose: Purpose,
    /// The face channels a VRM carries: `arkit52` (the default, the 52 perfect-sync channels plus the presets),
    /// `arkit52_with_extras`, or `off` (the presets only).
    #[serde(default)]
    pub perfect_sync: PerfectSync,
    /// A variant of a kit character to export instead of its base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<VariantId>,
    /// Scale the character to this height in metres (feet to the top of the hair). Leave it out to export life-size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_height: Option<f32>,
}

/// One file the export wrote (or, in a dry run, would write).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WrittenFile {
    /// Where, as given.
    pub path: String,
    /// Its media type.
    pub media_type: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// Its content hash.
    pub hash: String,
}

/// What `io.export` returns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportResult {
    /// The character exported.
    pub character: String,
    /// The format written.
    pub format: String,
    /// What the file is for.
    pub purpose: Purpose,
    /// The files written, the main one first.
    pub files: Vec<WrittenFile>,
    /// The face channels the file carries: what was asked for, or `off` when they could not be baked (a finding says
    /// why).
    pub perfect_sync: PerfectSync,
    /// How each face channel was realised, when the face was baked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bake: Option<BakeReport>,
    /// Every finding: what the check, the bake and the exporter noticed, errors first.
    pub report: Report,
}

/// What a dry run of `io.export` returns: the check's verdict and where the file would go.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PlannedExport {
    /// Where the file will be written.
    pub path: String,
    /// Whether the face will be baked (`false` with a reason in `report` when it can't be).
    pub bakes_face: bool,
    /// The check's findings, as `io.check_export` gives them.
    pub report: Report,
}

impl Export {
    fn check(&self) -> CheckExport {
        CheckExport { character: self.character.clone(), format: self.format.clone(), purpose: self.purpose }
    }

    fn shape(&self) -> Shape {
        Shape { variant: self.variant.clone(), target_height: self.target_height }
    }

    /// The arguments that can be checked without the document: the height and the file's extension.
    fn check_arguments(&self) -> Result<(), OpError> {
        if let Some(h) = self.target_height
            && !(h.is_finite() && (0.1..=100.0).contains(&h))
        {
            return Err(OpError::invalid("target_height", format!("{h} m is not a height: give 0.1 to 100 metres"))
                .hint("leave it out to export life-size"));
        }
        let registered = formats();
        let format = registered.iter().find(|f| f.export && f.info.id == self.format.as_str()).ok_or_else(|| {
            OpError::invalid("format", format!("no exporter is registered for format {}", self.format))
                .hint(exportable_formats())
        })?;
        let extension = std::path::Path::new(&self.path).extension().and_then(|e| e.to_str()).unwrap_or_default();
        if !format.info.extensions.iter().any(|e| e.eq_ignore_ascii_case(extension)) {
            let wanted = format.info.extensions.first().copied().unwrap_or_default();
            return Err(OpError::invalid(
                "path",
                format!("a {} file ends in .{}, and '{}' doesn't", format.info.name, wanted, self.path),
            )
            .hint(format!("name the file <name>.{wanted}")));
        }
        Ok(())
    }
}

/// Refuses an export the check found errors in, naming the first few and how to fix the first.
fn refuse(check: &ExportCheck) -> OpError {
    let errors: Vec<&Finding> = check.report.errors().collect();
    let shown: Vec<String> = errors.iter().take(3).map(|f| format!("{} ({})", f.message, f.rule)).collect();
    let more = errors.len().saturating_sub(shown.len());
    let tail = if more > 0 { format!("; and {more} more") } else { String::new() };
    let error = OpError::invalid("character", format!("the export is refused: {}{tail}", shown.join("; ")));
    match errors.iter().find_map(|f| f.hint.as_deref()) {
        Some(hint) => error.hint(format!("{hint} (io.check_export lists every finding)")),
        None => error.hint("io.check_export lists every finding with its fix"),
    }
}

/// Why the face is not baked, or `None` when it will be.
fn why_not_baked(exported: &ExportScene, perfect_sync: PerfectSync, vrm: bool) -> Option<String> {
    if !vrm || !perfect_sync.wants_arkit() {
        return None;
    }
    if exported.face.is_none() {
        return Some("the character has no painted face on a head mesh to bake".to_owned());
    }
    match face_baker() {
        Ok(Some(_)) => None,
        Ok(None) => Some("this build has no face baker".to_owned()),
        Err(conflict) => Some(conflict.to_string()),
    }
}

/// The 52 `ARKit` channel names, in order.
fn arkit_channels() -> Result<Vec<ExpressionName>, OpError> {
    ARKIT_52
        .iter()
        .map(|c| ExpressionName::parse(c).map_err(|e| OpError::internal(format!("the ARKit channel {c}: {e}"))))
        .collect()
}

/// The face's own expressions that aren't a VRM preset, by the names they export under (`omuzaki_smug`), for the
/// baker to realise from their channel deltas. A name a channel already exports under (an extra like `omuzaki_blush`)
/// stays the channel's.
fn named_expressions(profile: &FaceProfile) -> Vec<ExpressionName> {
    profile
        .expressions
        .0
        .keys()
        .filter_map(named_expression)
        .filter(|name| !matches!(name, ExpressionName::Preset(_)) && channel_for(name.as_str()).is_none())
        .collect()
}

/// How many of the 52 channels `scene`'s avatar has.
fn arkit_present(scene: &IoScene) -> usize {
    let Some(avatar) = &scene.avatar else { return 0 };
    ARKIT_52.iter().filter(|c| avatar.expressions.keys().any(|e| e.as_str() == **c)).count()
}

/// "exportable formats: glb, vrm1", for hints.
fn exportable_formats() -> String {
    let ids: Vec<&str> = formats().iter().filter(|f| f.export).map(|f| f.info.id).collect();
    format!("exportable formats: {}", ids.join(", "))
}

/// Bakes the face into `scene` when it can, and says what the file will carry: the perfect-sync setting to export
/// with, the bake's report, and a finding when it falls back to the presets.
fn bake_face(
    exported: &ExportScene,
    scene: &mut IoScene,
    wanted: PerfectSync,
    vrm: bool,
    job: &JobCtx<'_>,
) -> Result<(PerfectSync, Option<BakeReport>, Option<Finding>), OpError> {
    if !vrm || !wanted.wants_arkit() {
        return Ok((if vrm { wanted } else { PerfectSync::Off }, None, None));
    }
    let not_baked = |why: String| {
        Finding::warning("perfect_sync.not_baked", format!("the file carries the VRM presets only: {why}"))
            .hint("the presets (happy, aa, blink...) work everywhere; perfect-sync trackers need the 52 channels")
    };
    // An imported model brings its own face: keep perfect sync only if it has every channel.
    if scene.avatar.is_some() && exported.face.is_none() && arkit_present(scene) > 0 {
        let present = arkit_present(scene);
        if present == ARKIT_52.len() {
            return Ok((wanted, None, None));
        }
        let why = format!("the model has {present} of the 52 perfect-sync channels");
        return Ok((PerfectSync::Off, None, Some(not_baked(why))));
    }
    if let Some(why) = why_not_baked(exported, wanted, vrm) {
        return Ok((PerfectSync::Off, None, Some(not_baked(why))));
    }
    let (Some(face), Ok(Some(factory))) = (&exported.face, face_baker()) else {
        return Ok((PerfectSync::Off, None, Some(not_baked("the face can't be baked".to_owned()))));
    };
    let input = BakerInput { profile: &face.profile, projection: face.projection, head_centre: face.head_centre };
    let baker = factory.create(input).map_err(|e| from_io(e, "character"))?;
    let mut channels = arkit_channels()?;
    channels.extend(named_expressions(&face.profile));
    let request = BakeRequest::new(channels, face.target);
    let report = baker.bake(scene, &request, &JobProgress(job)).map_err(|e| from_io(e, "character"))?;
    Ok((wanted, Some(report), None))
}

/// Writes `file` at `path`: a temporary file in its folder, renamed over the old one once it is all on the disk.
fn write(job: &JobCtx<'_>, path: &str, file: &OutputFile) -> Result<WrittenFile, OpError> {
    let target = job.fs_access(path, FsMode::Write)?;
    if let Some(folder) = target.parent() {
        folder.create_dir_all()?;
    }
    let mut replacement = target.replace()?;
    replacement
        .write_all(&file.bytes)
        .map_err(|e| OpError::invalid("path", format!("couldn't write {path}: {e}")).hint("check the disk has room"))?;
    replacement.commit()?;
    Ok(WrittenFile {
        path: path.to_owned(),
        media_type: file.media_type.clone(),
        bytes: file.bytes.len() as u64,
        hash: ContentHash::of(&file.bytes).to_string(),
    })
}

impl Op for Export {
    fn meta() -> OpMeta {
        OpMeta {
            name: "io.export",
            summary: "Export a character (built or imported) to a VRM or glTF file: the licence gate, the face baked to \
                      the 52 perfect-sync channels, and the file written in one step. Returns every finding.",
            kind: OpKind::Job,
            // An export is a file that may leave the machine; the write itself also needs `fs` (checked on the path).
            scope: Scope::Publish,
            // It changes nothing in the document; the file is the result.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.check_arguments()?;
        // Fails now, not after the job is queued, when the actor may not write there.
        ctx.fs_access(&self.path, FsMode::Write)?;
        let exported = self.check().run_scene(ctx, &self.shape())?;
        if !exported.check.ok {
            return Err(refuse(&exported.check));
        }
        let vrm = self.format.as_str().starts_with("vrm");
        let why = why_not_baked(&exported, self.perfect_sync, vrm);
        let mut report = exported.check.report.clone();
        // The check says the channels are left for the export; the plan says what the export will do with them.
        report.findings.retain(|f| f.rule != "perfect_sync.deferred");
        if let Some(why) = &why {
            report.push(Finding::warning("perfect_sync.not_baked", format!("the file will carry the VRM presets only: {why}")));
        }
        let planned = PlannedExport {
            path: self.path.clone(),
            bakes_face: vrm && self.perfect_sync.wants_arkit() && why.is_none(),
            report,
        };
        let output = serde_json::to_value(planned).map_err(|e| OpError::internal(format!("the planned export: {e}")))?;
        Ok(Plan { output: Some(output), cost: Some(CostHint { seconds: Some(3.0), gpu_mib: Some(0), usd: None }), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        self.check_arguments()?;
        job.checkpoint("building", 0.0)?;
        let clock = SystemClock::default();
        let mut verdict = job.read(|doc| {
            let ctx = OpCtx::new(doc, job.actor(), &clock).with_services(job.services());
            self.check().run_scene(&ctx, &self.shape())
        })?;
        // The document may have changed since the plan: the verdict is the one on what is written.
        if !verdict.check.ok {
            return Err(refuse(&verdict.check));
        }
        let Some(mut scene) = verdict.scene.take() else {
            return Err(OpError::internal("the check passed without a scene to write"));
        };
        let vrm = self.format.as_str().starts_with("vrm");
        job.checkpoint("baking the face", 0.3)?;
        let (perfect_sync, bake, fallback) = bake_face(&verdict, &mut scene, self.perfect_sync, vrm, job)?;
        let exporter = find_exporter(self.format.as_str())
            .ok_or_else(|| OpError::invalid("format", format!("no exporter for {}", self.format)))?;
        let options = ExportOptions::default()
            .with_purpose(self.purpose)
            .with_perfect_sync(perfect_sync)
            .with_generator(GENERATOR);
        job.checkpoint("writing", 0.6)?;
        let progress = JobProgress(job);
        let outcome = run_export(exporter.as_ref(), &scene, &ExportCtx::new(&options).with_progress(&progress))
            .map_err(|e| from_io(e, "character"))?;
        let mut files = Vec::with_capacity(outcome.files.len());
        for (i, file) in outcome.files.iter().enumerate() {
            // The main file goes where it was asked to; any other (a separate glTF's buffers) beside it.
            let path = if i == 0 {
                self.path.clone()
            } else {
                let folder = std::path::Path::new(&self.path).parent().map(|p| p.join(&file.path));
                folder.and_then(|p| p.to_str().map(str::to_owned)).unwrap_or_else(|| file.path.clone())
            };
            files.push(write(job, &path, file)?);
        }
        let mut report = verdict.check.report;
        report.findings.retain(|f| f.rule != "perfect_sync.deferred");
        let mut written = outcome.report;
        if let Some(bake) = &bake {
            // The preflight sees an empty expression; the bake already said which channels it left inert, and why.
            written.findings.retain(|f| f.rule != "perfect_sync.inert_channel");
            written.extend(bake.findings.clone());
        }
        report.extend(written);
        if let Some(finding) = fallback {
            report.push(finding);
        }
        // Errors first, as `io.check_export` orders them (a stable sort keeps each group's order).
        report.findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
        job.checkpoint("done", 1.0)?;
        let result = ExportResult {
            character: self.character.to_string(),
            format: self.format.to_string(),
            purpose: self.purpose,
            files,
            perfect_sync,
            bake,
            report,
        };
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("the export's result: {e}")))
    }
}

register_op!(Export);
