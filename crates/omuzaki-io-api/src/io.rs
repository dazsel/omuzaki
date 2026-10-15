//! The importer and exporter traits, their inputs and outputs, and the two entry points every caller uses.
//!
//! **Pure by design.** An importer is bytes in, scene out; an exporter is scene in, bytes out. Neither touches the file
//! system, the network or the clock. Files a `.gltf` refers to arrive through a [`Resolver`] the host provides (the op
//! layer checks the `fs` grant there), and output goes back as [`OutputFile`]s the host writes (checking `publish`).
//! That makes both testable without disk, safe to run on untrusted input, runnable on the web build, and
//! deterministic: **the same scene and options must produce byte-identical files**, so the content-addressed store
//! dedups exports and golden tests can compare bytes.
//!
//! **Call [`run_import`] and [`run_export`], not the trait methods directly.** They add what every format needs and
//! no implementation should repeat or forget: input limits, the scene invariants, the rights rules, perfect-sync
//! completeness and strict mode. A format implements only what is specific to it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Confidence, FormatInfo, IoError, IoScene, LicenceEvidence, Report, preflight};

/// Bytes to import, with the file name when there is one (extension hint, provenance, diagnostics).
#[derive(Copy, Clone, Debug)]
#[non_exhaustive]
pub struct ImportInput<'a> {
    /// The file name, without directories.
    pub name: Option<&'a str>,
    /// The whole file. Imports are authoring-plane work, so holding a 45 MB VRM in memory is fine.
    pub bytes: &'a [u8],
}

impl<'a> ImportInput<'a> {
    /// Input without a name.
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { name: None, bytes }
    }

    /// Adds the file name (builder style).
    #[must_use]
    pub fn named(mut self, name: &'a str) -> Self {
        self.name = Some(name);
        self
    }
}

/// Reads files a glTF refers to (`buffers[*].uri`, `images[*].uri`). `data:` URIs never reach it; the importer decodes
/// those itself.
pub trait Resolver: Send + Sync {
    /// The bytes at `uri`, relative to the importing file.
    ///
    /// # Errors
    /// [`IoError::Resource`] if it can't be read or the host doesn't allow it.
    fn read(&self, uri: &str) -> Result<Vec<u8>, IoError>;
}

/// A resolver for self-contained files: every external reference fails with a message saying so.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoResolver;

impl Resolver for NoResolver {
    fn read(&self, uri: &str) -> Result<Vec<u8>, IoError> {
        Err(IoError::Resource { uri: uri.to_owned(), reason: "this import has no access to other files".to_owned() })
    }
}

/// Progress and cancellation for long imports and exports (they run as `OpKind::Job`).
///
/// This duplicates what the job manager needs, and the two stay separate types. An io op that runs as a job passes an
/// adapter over its `JobCtx` instead: `omuzaki_io_ops::ops::progress::JobProgress` implements this trait by forwarding to
/// the job. Decided on review; see `docs/design/io-api.md`.
pub trait Progress: Send + Sync {
    /// Reports that `stage` is `fraction` (0..=1) done.
    fn stage(&self, name: &str, fraction: f32);
    /// True once the caller wants the work stopped.
    fn is_cancelled(&self) -> bool;

    /// Reports progress, then fails with [`IoError::Cancelled`] if cancelled. Call between stages and every few
    /// thousand items of a long loop.
    ///
    /// # Errors
    /// [`IoError::Cancelled`].
    fn checkpoint(&self, name: &str, fraction: f32) -> Result<(), IoError> {
        self.stage(name, fraction);
        if self.is_cancelled() { Err(IoError::Cancelled) } else { Ok(()) }
    }
}

/// Progress that goes nowhere and never cancels.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoProgress;

impl Progress for NoProgress {
    fn stage(&self, _: &str, _: f32) {}
    fn is_cancelled(&self) -> bool {
        false
    }
}

static NO_RESOLVER: NoResolver = NoResolver;
static NO_PROGRESS: NoProgress = NoProgress;

/// Caps that keep a hostile or broken file from exhausting memory. Importers check the *declared* sizes (accessor
/// counts, buffer lengths, image counts) against these **before allocating**; [`run_import`] re-checks the result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct Limits {
    /// Largest accepted input file.
    pub max_input_bytes: u64,
    /// Most nodes.
    pub max_nodes: u32,
    /// Most vertices over the whole scene.
    pub max_vertices: u32,
    /// Most images.
    pub max_images: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_input_bytes: 512 << 20, max_nodes: 100_000, max_vertices: 8_000_000, max_images: 1024 }
    }
}

/// Import settings. `#[non_exhaustive]`: build with [`ImportOptions::default`] and set fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[non_exhaustive]
pub struct ImportOptions {
    /// Treat warnings as errors (CI and conformance runs). Lenient by default: real-world files are messy.
    pub strict: bool,
    /// Size caps.
    pub limits: Limits,
}

impl ImportOptions {
    /// Strict mode on (builder style).
    #[must_use]
    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }

    /// Replaces the limits (builder style).
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }
}

/// What an importer receives besides the bytes.
#[derive(Copy, Clone)]
#[non_exhaustive]
pub struct ImportCtx<'a> {
    /// Settings.
    pub options: &'a ImportOptions,
    /// Reads sidecar files.
    pub resolver: &'a dyn Resolver,
    /// Progress and cancellation.
    pub progress: &'a dyn Progress,
}

impl<'a> ImportCtx<'a> {
    /// A context with no sidecar files and no progress.
    pub fn new(options: &'a ImportOptions) -> Self {
        Self { options, resolver: &NO_RESOLVER, progress: &NO_PROGRESS }
    }

    /// Sets the resolver (builder style).
    #[must_use]
    pub fn with_resolver(mut self, resolver: &'a dyn Resolver) -> Self {
        self.resolver = resolver;
        self
    }

    /// Sets the progress handle (builder style).
    #[must_use]
    pub fn with_progress(mut self, progress: &'a dyn Progress) -> Self {
        self.progress = progress;
        self
    }
}

/// A successful import: the scene, and what was noticed on the way (warnings, dropped extensions, lossy steps).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ImportOutcome {
    /// The scene, in canonical space.
    pub scene: IoScene,
    /// Findings about the file.
    pub report: Report,
    /// What the importer found about the file's licence. [`run_import`] turns it into the rights note, so a file whose
    /// licence data was left behind is never reported as having none.
    pub licence: LicenceEvidence,
}

impl ImportOutcome {
    /// An outcome with an empty report. There is no default for `licence`: every importer states what it found about
    /// the file's licence ([`LicenceEvidence`]), because a wrong default is a wrong rights report.
    pub fn new(scene: IoScene, licence: LicenceEvidence) -> Self {
        Self { scene, report: Report::new(), licence }
    }
}

/// Reads one format into an [`IoScene`]. Register with [`register_importer!`](crate::register_importer).
///
/// An importer must: convert to canonical space; decode every accessor into the scene's typed arrays; keep image bytes
/// as they are; map the file's meta into `scene.meta`; **report, don't hide,** anything it drops or approximates
/// (unknown extensions, extra UV sets, matrix nodes it had to decompose) as findings; say in
/// [`ImportOutcome::licence`] whether it read the file's licence data, left it behind or found none; and never
/// allocate from a declared size before checking it against `ctx.options.limits`.
pub trait Importer: Send + Sync {
    /// Static facts about the format.
    fn info(&self) -> FormatInfo;

    /// Could this importer read `input`? Cheap: look at magic bytes and top-level JSON keys only.
    fn sniff(&self, input: &ImportInput<'_>) -> Confidence;

    /// Reads `input`.
    ///
    /// # Errors
    /// [`IoError::Malformed`] for broken files, [`IoError::Unsupported`] for required features it lacks,
    /// [`IoError::Resource`] for unreadable sidecars, [`IoError::Limit`] over the limits, [`IoError::Cancelled`].
    fn import(&self, input: &ImportInput<'_>, ctx: &ImportCtx<'_>) -> Result<ImportOutcome, IoError>;
}

/// What an export is for. It decides which rights rules apply.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// A file for the user's own machine (trying a VRM in `VSeeFace`). The licence gate is not applied, but the file's
    /// terms are capped by the provenance, and private test material is flagged.
    #[default]
    Private,
    /// A file that will leave the machine. The licence gate (spec §7, §40) must pass.
    Publish,
}

/// How glTF output is laid out. VRM is always `Binary` (a `.vrm` is a GLB).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    /// One `.glb`.
    #[default]
    Binary,
    /// A `.gltf` with its `.bin` and image files beside it.
    Separate,
}

/// Export settings. `#[non_exhaustive]`: build with [`ExportOptions::default`] and set fields.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
#[non_exhaustive]
pub struct ExportOptions {
    /// Private or published.
    pub purpose: Purpose,
    /// Single file or separate files.
    pub layout: Layout,
    /// How many face channels the file carries. On by default (52 `ARKit` channels, no extras); has no effect on a scene
    /// without an avatar.
    pub perfect_sync: crate::PerfectSync,
    /// Written to `asset.generator`. The exporter must not add a version or time on its own, or output would change
    /// between builds and break determinism; the host passes `Some("Omuzaki 1.0")` or similar.
    pub generator: Option<String>,
    /// Treat warnings as errors.
    pub strict: bool,
    /// The caller knows a real person's likeness or voice is involved, which makes the gate require recorded consent.
    pub involves_real_person: bool,
}

impl ExportOptions {
    /// Sets the purpose (builder style).
    #[must_use]
    pub fn with_purpose(mut self, purpose: Purpose) -> Self {
        self.purpose = purpose;
        self
    }

    /// Sets the layout (builder style).
    #[must_use]
    pub fn with_layout(mut self, layout: Layout) -> Self {
        self.layout = layout;
        self
    }

    /// Sets how many face channels to carry (builder style).
    #[must_use]
    pub fn with_perfect_sync(mut self, perfect_sync: crate::PerfectSync) -> Self {
        self.perfect_sync = perfect_sync;
        self
    }

    /// Sets the `asset.generator` string (builder style).
    #[must_use]
    pub fn with_generator(mut self, generator: impl Into<String>) -> Self {
        self.generator = Some(generator.into());
        self
    }

    /// Strict mode on (builder style).
    #[must_use]
    pub fn strict(mut self) -> Self {
        self.strict = true;
        self
    }

    /// Declares that a real person's likeness or voice is involved (builder style).
    #[must_use]
    pub fn with_real_person(mut self) -> Self {
        self.involves_real_person = true;
        self
    }
}

/// What an exporter receives besides the scene.
#[derive(Copy, Clone)]
#[non_exhaustive]
pub struct ExportCtx<'a> {
    /// Settings.
    pub options: &'a ExportOptions,
    /// Progress and cancellation.
    pub progress: &'a dyn Progress,
}

impl<'a> ExportCtx<'a> {
    /// A context with no progress.
    pub fn new(options: &'a ExportOptions) -> Self {
        Self { options, progress: &NO_PROGRESS }
    }

    /// Sets the progress handle (builder style).
    #[must_use]
    pub fn with_progress(mut self, progress: &'a dyn Progress) -> Self {
        self.progress = progress;
        self
    }
}

/// One file an export wants written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputFile {
    /// Path relative to the export directory (`model.vrm`, or `model.gltf` and `model.bin`).
    pub path: String,
    /// Media type.
    pub media_type: String,
    /// The bytes.
    pub bytes: Vec<u8>,
}

/// A successful export.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ExportOutcome {
    /// The files to write. The first is the main file.
    pub files: Vec<OutputFile>,
    /// What was lost or approximated, plus any warnings from the preflight.
    pub report: Report,
}

impl ExportOutcome {
    /// An outcome with the given files and an empty report.
    pub fn new(files: Vec<OutputFile>) -> Self {
        Self { files, report: Report::new() }
    }
}

/// Writes one format from an [`IoScene`]. Register with [`register_exporter!`](crate::register_exporter).
///
/// An exporter must be deterministic; write every spec default explicitly rather than rely on a reader's idea of it;
/// omit empty arrays the format forbids (VRM expression binds need at least one entry); put what it cannot represent
/// in the report as findings, not silently; and write inverse bind matrices and node transforms **as given**.
pub trait Exporter: Send + Sync {
    /// Static facts about the format.
    fn info(&self) -> FormatInfo;

    /// Format-specific checks: would this scene export cleanly? For VRM, call
    /// [`vrm_profile`](crate::vrm_profile). Don't repeat the shared checks; [`run_export`] runs those.
    fn check(&self, scene: &IoScene, options: &ExportOptions) -> Report;

    /// Writes the scene. Called only after the checks passed.
    ///
    /// # Errors
    /// [`IoError::Unsupported`] for scene content it cannot write, [`IoError::Cancelled`], [`IoError::Internal`].
    fn export(&self, scene: &IoScene, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError>;
}

/// Imports `input` with `importer`, then enforces the contract: size limits, the scene invariants (an importer that
/// emits a dangling index is rejected here, with the rule id and location, instead of crashing a consumer), the
/// provenance record (derived conservatively from the file's meta if the importer didn't set one), the notes about
/// what the file's terms allow (or, by the importer's [`LicenceEvidence`], that the licence was left behind or absent),
/// and strict mode.
///
/// # Errors
/// Anything the importer returns, [`IoError::Limit`] over the limits, and [`IoError::Rejected`] when the scene breaks
/// an invariant or (in strict mode) the report has warnings.
pub fn run_import(
    importer: &dyn Importer,
    input: &ImportInput<'_>,
    ctx: &ImportCtx<'_>,
) -> Result<ImportOutcome, IoError> {
    let limits = &ctx.options.limits;
    let size = input.bytes.len() as u64;
    if size > limits.max_input_bytes {
        return Err(IoError::Limit { what: "input size in bytes", limit: limits.max_input_bytes, got: size });
    }
    let mut outcome = importer.import(input, ctx)?;
    let s = outcome.scene.summary();
    for (what, limit, got) in [
        ("node count", u64::from(limits.max_nodes), s.nodes),
        ("vertex count", u64::from(limits.max_vertices), s.vertices),
        ("image count", u64::from(limits.max_images), s.images),
    ] {
        if got > limit {
            return Err(IoError::Limit { what, limit, got });
        }
    }
    let structural = outcome.scene.check();
    outcome.report.extend(structural);
    if outcome.report.has_errors() {
        return Err(IoError::Rejected(outcome.report));
    }
    if outcome.scene.provenance.is_none() {
        outcome.scene.provenance = Some(outcome.scene.meta.imported_provenance(input.name));
    }
    let rights = outcome.licence.findings(&outcome.scene.meta);
    outcome.report.extend(Report { findings: rights });
    outcome.report.findings.extend(outcome.scene.passthrough_finding());
    if ctx.options.strict {
        outcome.report = outcome.report.strict();
        if outcome.report.has_errors() {
            return Err(IoError::Rejected(outcome.report));
        }
    }
    Ok(outcome)
}

/// Exports `scene` with `exporter`: [`preflight`] (scene invariants, rights, perfect-sync completeness), then the
/// exporter's own [`Exporter::check`]; if anything is an error (or a warning in strict mode) nothing is written and
/// the whole report comes back in [`IoError::Rejected`]. Otherwise the exporter runs and its outcome carries the
/// preflight's warnings and its own lossy-conversion notes together.
///
/// # Errors
/// [`IoError::Rejected`] with every finding, or whatever the exporter returns.
pub fn run_export(exporter: &dyn Exporter, scene: &IoScene, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError> {
    let mut report = preflight(scene, ctx.options);
    report.extend(exporter.check(scene, ctx.options));
    if ctx.options.strict {
        report = report.strict();
    }
    if report.has_errors() {
        return Err(IoError::Rejected(report));
    }
    ctx.progress.checkpoint("export", 0.0)?;
    let mut outcome = exporter.export(scene, ctx)?;
    report.extend(std::mem::take(&mut outcome.report));
    outcome.report = report;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Severity, VrmMeta};

    /// An importer that reads nothing and states `evidence` and `meta`.
    struct Stub(LicenceEvidence, VrmMeta);

    impl Importer for Stub {
        fn info(&self) -> FormatInfo {
            FormatInfo { id: "stub", name: "stub", extensions: &["stub"], media_type: "application/octet-stream" }
        }

        fn sniff(&self, _: &ImportInput<'_>) -> Confidence {
            Confidence::No
        }

        fn import(&self, _: &ImportInput<'_>, _: &ImportCtx<'_>) -> Result<ImportOutcome, IoError> {
            let mut scene = IoScene::new();
            scene.meta = self.1.clone();
            Ok(ImportOutcome::new(scene, self.0.clone()))
        }
    }

    fn rights_rules(evidence: LicenceEvidence, meta: VrmMeta, options: &ImportOptions) -> Result<Vec<String>, IoError> {
        let out = run_import(&Stub(evidence, meta), &ImportInput::new(b"x"), &ImportCtx::new(options))?;
        Ok(out.report.findings.into_iter().filter(|f| f.rule.starts_with("rights.")).map(|f| f.rule).collect())
    }

    fn present() -> LicenceEvidence {
        LicenceEvidence::PresentNotImported { summary: "licenseName=CC_BY_NC".into(), hint_format: "vrm0" }
    }

    #[test]
    fn the_rights_note_follows_the_importers_evidence() {
        let options = ImportOptions::default();
        let filled = VrmMeta::new("n", vec!["a".into()], crate::VRM_LICENSE_URLS[0]);
        let rules = |evidence, meta| rights_rules(evidence, meta, &options).unwrap();
        // Only a file with no licence data at all is reported as having none.
        assert_eq!(rules(LicenceEvidence::Absent, VrmMeta::default()), ["rights.no_license_info"]);
        // Licence data left behind is a warning of its own, and never "no licence information".
        assert_eq!(rules(present(), VrmMeta::default()), ["rights.license_not_imported"]);
        // Imported terms describe themselves (the defaults forbid redistribution), and a blank URL is still blank.
        assert_eq!(rules(LicenceEvidence::Imported, filled), ["rights.import_restricted"]);
        assert_eq!(rules(LicenceEvidence::Imported, VrmMeta::default()), ["rights.no_license_info"]);
    }

    #[test]
    fn strict_mode_rejects_licence_data_that_was_left_behind() {
        let strict = ImportOptions::default().strict();
        let Err(IoError::Rejected(report)) = rights_rules(present(), VrmMeta::default(), &strict) else {
            panic!("a warning is an error in strict mode")
        };
        let f = report.errors().next().expect("an error");
        assert_eq!((f.rule.as_str(), f.severity), ("rights.license_not_imported", Severity::Error));
        // "No licence information" is only an info, so it does not stop a strict import.
        assert!(rights_rules(LicenceEvidence::Absent, VrmMeta::default(), &strict).is_ok());
    }
}
