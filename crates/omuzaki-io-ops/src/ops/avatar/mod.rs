//! A model as an avatar (design D6): the preparation `io.inspect {as: avatar}` and `io.import {as: avatar}` share, and
//! the `avatar.*` ops on an imported character.
//!
//! | Op | Kind | Does |
//! |---|---|---|
//! | `io.inspect {as: avatar}` | query | prepares in memory: the summary, the one report, and the confirm token |
//! | `io.import {as: avatar}` | job | the same, then stores the original and its images and writes the character |
//! | [`remap`] `avatar.remap` | edit | sets or clears one humanoid override |
//! | [`map_channel`] `avatar.map_channel` | edit | sets or clears one channel driver override |
//! | [`describe`] `avatar.describe` | query | the current report and what the model drives |
//!
//! The preparation is [`to_avatar`] on the imported scene, plus the rights findings of the file's meta
//! (`VrmMeta::perform_findings`, D5's table), timed per stage (`AvatarSummary::timings`) and sealed by the
//! [`confirm`] token.

pub mod confirm;
pub mod describe;
pub mod import;
pub mod map_channel;
pub mod remap;

use std::sync::Mutex;
use std::time::Duration;

use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_avatar_api::summary::{AvatarSummary, Stage};
use omuzaki_char_api::CharId;
use omuzaki_char_api::paths;
use omuzaki_core::time::Clock;
use omuzaki_doc_api::DocRead;
use omuzaki_io_api::{
    FormatId, ImportCtx, ImportInput, ImportOptions, Importer, IoError, IoScene, Progress, Report, Resolver, find_importer,
    run_import, sniff,
};
use omuzaki_ops_api::OpError;
use omuzaki_render_api::{Capabilities, MtoonSupport};
use omuzaki_store_api::{ContentHash, Provenance, Store};

use crate::error::from_io;
use crate::prepare::avatar::{between, to_avatar_timed};

/// The `MToon` groups the renderer draws: from the renderer's [`Capabilities`] when the host provides them as a
/// service, otherwise none (what every renderer draws today), so the report never promises a group it can't show.
pub(crate) fn mtoon(capabilities: Option<&Capabilities>) -> MtoonSupport {
    capabilities.map_or(MtoonSupport::NONE, |c| c.mtoon)
}

/// The file format an importer reads, as the avatar node records it; `None` for a format that cannot be an avatar yet.
fn source_format(importer: &str) -> Option<SourceFormat> {
    match importer {
        "vrm1" => Some(SourceFormat::Vrm1),
        "vrm0" => Some(SourceFormat::Vrm0),
        "glb" => Some(SourceFormat::Gltf),
        "fbx" => Some(SourceFormat::Fbx),
        _ => None,
    }
}

/// A file to prepare as an avatar.
pub(crate) struct Source<'a> {
    /// The file's name, as the person knows it.
    pub name: &'a str,
    /// Its bytes.
    pub bytes: &'a [u8],
    /// The format to read it as, or `None` to detect it.
    pub format: Option<&'a FormatId>,
    /// The files it refers to.
    pub resolver: &'a dyn Resolver,
    /// Where progress and cancellation go.
    pub progress: &'a dyn Progress,
    /// The argument the bytes came from (`path`, `asset`, `bytes`), which errors about the file blame.
    pub field: &'a str,
}

/// What one preparation produced.
#[derive(Debug)]
pub(crate) struct Prepared {
    /// The avatar.
    pub avatar: ImportedAvatar,
    /// The node it was prepared with.
    pub def: AvatarDef,
    /// The scene as the importer read it, before any preparation step.
    pub scene: IoScene,
    /// The whole report: the importer's findings, every preparation step's, and the rights rows of D5.
    pub report: Report,
    /// The summary for the confirm screen.
    pub summary: AvatarSummary,
    /// The confirm token over all of it.
    pub confirm: String,
    /// The record the original is stored with (the importer's conservative reading of the file's rights).
    pub provenance: Provenance,
    /// The importer's format id.
    pub format: String,
}

/// Records the content hash of every file the importer reads through it.
struct Recording<'a> {
    inner: &'a dyn Resolver,
    read: Mutex<Vec<(String, ContentHash)>>,
}

impl Resolver for Recording<'_> {
    fn read(&self, uri: &str) -> Result<Vec<u8>, IoError> {
        let bytes = self.inner.read(uri)?;
        let mut read = self.read.lock().map_err(|_| IoError::Internal("the dependency list was poisoned".into()))?;
        if !read.iter().any(|(seen, _)| seen == uri) {
            read.push((uri.to_owned(), ContentHash::of(&bytes)));
        }
        Ok(bytes)
    }
}

/// A file read by its importer, before any preparation: what [`prepare_imported`] prepares.
pub(crate) struct Read {
    /// The import's outcome: the scene and the importer's report.
    pub outcome: omuzaki_io_api::ImportOutcome,
    /// Every file read beside it, by URI, with its content hash.
    pub dependencies: Vec<(String, ContentHash)>,
    /// The importer's format id.
    pub format: String,
    /// The file's format, as the avatar node records it.
    pub source_format: SourceFormat,
    /// The source file's content hash.
    pub hash: ContentHash,
    /// How long the read took.
    pub took: Duration,
}

/// Reads `source` with its importer (the `Read` stage of D6), recording every file it reads beside it.
///
/// # Errors
/// `Unsupported` for a file no importer reads or a format that cannot be an avatar yet; `Invalid` for a file that is
/// malformed or over the limits.
pub(crate) fn read(source: &Source<'_>, options: &ImportOptions, clock: &dyn Clock) -> Result<Read, OpError> {
    let start = clock.now();
    let input = ImportInput::new(source.bytes).named(source.name);
    let importer = pick_importer(source.format, &input)?;
    let format = importer.info().id.to_owned();
    let Some(source_format) = source_format(&format) else {
        return Err(OpError::unsupported(format!("a {format} file can't be imported as an avatar yet"))
            .hint("import a VRM or a glTF (.glb) as an avatar"));
    };
    let recording = Recording { inner: source.resolver, read: Mutex::new(Vec::new()) };
    let ctx = ImportCtx::new(options).with_resolver(&recording).with_progress(source.progress);
    let outcome = run_import(&*importer, &input, &ctx).map_err(|e| from_io(e, source.field))?;
    let dependencies = recording.read.into_inner().map_err(|_| OpError::internal("the dependency list was poisoned"))?;
    let took = between(start, clock.now());
    Ok(Read { outcome, dependencies, format, source_format, hash: ContentHash::of(source.bytes), took })
}

/// Reads `source` and prepares it as an avatar with `def` (or a fresh node for the file when `None`), timing each
/// stage on `clock`; `doc` is read for an id the project doesn't use yet.
///
/// # Errors
/// As [`read`] and [`prepare_imported`].
pub(crate) fn prepare(
    source: &Source<'_>,
    def: Option<AvatarDef>,
    options: &ImportOptions,
    mtoon: MtoonSupport,
    clock: &dyn Clock,
    doc: &dyn DocRead,
) -> Result<Prepared, OpError> {
    let read = read(source, options, clock)?;
    prepare_imported(read, source, def, options, mtoon, clock, doc)
}

/// Prepares a file already [`read`] as an avatar (the `Prepare` and `Decode` stages), adds the file's rights rows
/// (D5) and seals it all with the confirm token.
///
/// # Errors
/// `Invalid` for a model that cannot become an avatar (the message names the report's errors).
#[allow(clippy::too_many_arguments)] // what was read, what it is prepared with, and where its time and id come from
pub(crate) fn prepare_imported(
    read: Read,
    source: &Source<'_>,
    def: Option<AvatarDef>,
    options: &ImportOptions,
    mtoon: MtoonSupport,
    clock: &dyn Clock,
    doc: &dyn DocRead,
) -> Result<Prepared, OpError> {
    let Read { outcome, dependencies, format, source_format, hash, took } = read;
    let def = def.unwrap_or_else(|| AvatarDef::new(AvatarSource::new(hash, source_format, source.name)));
    let (avatar, prepared, timings) =
        to_avatar_timed(&outcome.scene, &def, mtoon, clock).map_err(|e| from_io(e, source.field))?;
    let mut report = outcome.report;
    report.extend(prepared);
    for finding in outcome.scene.meta.perform_findings() {
        report.push(finding);
    }
    let provenance = outcome
        .scene
        .provenance
        .clone()
        .ok_or_else(|| OpError::internal("run_import left the scene without a provenance record"))?;
    let id = id_suggestion(source.name, &outcome.scene, doc);
    let summary = AvatarSummary::new(id, avatar.driven(), avatar.texture_bytes())
        .with_timing(Stage::Read, took)
        .with_timing(Stage::Prepare, timings.prepare)
        .with_timing(Stage::Decode, timings.decode);
    let confirm = confirm::token(&confirm::Inputs {
        source: hash,
        dependencies: &dependencies,
        def: &def,
        mtoon,
        strict: options.strict,
        importer: &format,
        report: &report,
        suggestion: summary.id_suggestion.as_str(),
    })?;
    Ok(Prepared { avatar, def, scene: outcome.scene, report, summary, confirm, provenance, format })
}

/// The importer `format` names, or the best one the sniff finds.
fn pick_importer(format: Option<&FormatId>, input: &ImportInput<'_>) -> Result<Box<dyn Importer>, OpError> {
    if let Some(format) = format {
        return find_importer(format.as_str())
            .ok_or_else(|| OpError::invalid("format", format!("no importer is registered for format {format}")));
    }
    let best = sniff(input)
        .into_iter()
        .next()
        .ok_or_else(|| OpError::unsupported("no registered importer recognises this file"))?;
    find_importer(best.format).ok_or_else(|| OpError::internal(format!("importer {} vanished after sniffing", best.format)))
}

/// The id a new character made from the file would take: the file's name (or the model's), as a slug, made unique
/// among the project's characters with `_2`, `_3`, …; `avatar` when neither has a usable letter.
pub(crate) fn id_suggestion(file: &str, scene: &IoScene, doc: &dyn DocRead) -> CharId {
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let base = [stem, scene.meta.name.as_str()]
        .into_iter()
        .map(crate::prepare::avatar::skeleton::slug_stem)
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| "avatar".to_owned());
    let mut taken = std::collections::BTreeSet::new();
    loop {
        let slug = crate::prepare::avatar::skeleton::unique_slug(&base, &mut taken);
        let id = CharId(slug);
        if doc.get(&paths::character(&id)).is_none() {
            return id;
        }
    }
}

/// How long `f` took on `clock`, with its result.
pub(crate) fn timed<T>(clock: &dyn Clock, f: impl FnOnce() -> T) -> (T, Duration) {
    let start = clock.now();
    let out = f();
    (out, between(start, clock.now()))
}

/// The format id of the importer that reads `format`.
fn importer_id(format: SourceFormat) -> Option<FormatId> {
    let id = match format {
        SourceFormat::Vrm1 => "vrm1",
        SourceFormat::Vrm0 => "vrm0",
        SourceFormat::Gltf => "glb",
        SourceFormat::Fbx => "fbx",
        _ => return None,
    };
    id.parse().ok()
}

/// The avatar node of the imported character `id`.
///
/// # Errors
/// `NotFound` for no such character; `Invalid` for a character that is not imported, or whose node does not read
/// (an unknown key, a later layout).
pub(crate) fn read_def(doc: &dyn DocRead, id: &CharId) -> Result<AvatarDef, OpError> {
    let character = paths::character(id);
    let Some(node) = doc.get(&character) else {
        return Err(OpError::not_found(format!("no character {id}")).hint("pass the id of a character in the project"));
    };
    let Some(avatar) = node.get("avatar") else {
        return Err(OpError::invalid("character", format!("{id} is built from parts, not imported"))
            .hint("the avatar ops work on characters imported with io.import as: avatar"));
    };
    let def: AvatarDef = serde_json::from_value(avatar.clone())
        .map_err(|e| OpError::invalid("character", format!("the avatar node of {id} doesn't read: {e}")))?;
    def.check_schema().map_err(|e| OpError::invalid("character", e.to_string()))?;
    Ok(def)
}

/// Serves the files an imported original refers to from the store, by the URIs its asset record lists.
struct StoredSidecars<'a> {
    store: &'a dyn Store,
    sidecars: Vec<(String, ContentHash)>,
}

impl Resolver for StoredSidecars<'_> {
    fn read(&self, uri: &str) -> Result<Vec<u8>, IoError> {
        let resource = |reason: String| IoError::Resource { uri: uri.to_owned(), reason };
        let (_, hash) = self
            .sidecars
            .iter()
            .find(|(recorded, _)| recorded == uri)
            .ok_or_else(|| resource("the stored model doesn't record this file".to_owned()))?;
        self.store.get(hash).map_err(|e| resource(e.to_string()))
    }
}

/// The sidecars the asset record of `original` lists, by URI.
fn recorded_sidecars(doc: &dyn DocRead, original: &ContentHash) -> Vec<(String, ContentHash)> {
    let path = omuzaki_doc_api::DocPath::root().join("assets").join(original.to_string()).join("sidecars");
    let list = doc.get(&path).and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
    list.iter()
        .filter_map(|s| Some((s.get("uri")?.as_str()?.to_owned(), s.get("hash")?.as_str()?.parse().ok()?)))
        .collect()
}

/// The stored original an avatar was imported from, ready to [`read`] again.
pub(crate) struct Stored<'a> {
    bytes: Vec<u8>,
    name: String,
    format: Option<FormatId>,
    sidecars: StoredSidecars<'a>,
}

impl Stored<'_> {
    /// The original as a [`Source`] to read.
    pub(crate) fn source(&self) -> Source<'_> {
        Source {
            name: &self.name,
            bytes: &self.bytes,
            format: self.format.as_ref(),
            resolver: &self.sidecars,
            progress: &omuzaki_io_api::NoProgress,
            field: "character",
        }
    }
}

/// The stored original `def` names, with the files it refers to.
///
/// # Errors
/// `NotFound` when the original is not in the store; no store service is an error too.
pub(crate) fn stored<'a>(ctx: &omuzaki_ops_api::OpCtx<'a>, def: &AvatarDef) -> Result<Stored<'a>, OpError> {
    stored_in(ctx.doc, ctx.service::<dyn Store>()?, def)
}

/// [`stored`] from a document and a store, outside an op.
fn stored_in<'a>(doc: &dyn DocRead, store: &'a dyn Store, def: &AvatarDef) -> Result<Stored<'a>, OpError> {
    let hash = def.source.hash;
    let bytes = store.get(&hash).map_err(|e| {
        OpError::not_found(format!("the original of this avatar ({hash}) isn't in the store: {e}"))
            .hint("import the model again")
    })?;
    let sidecars = StoredSidecars { store, sidecars: recorded_sidecars(doc, &hash) };
    Ok(Stored { bytes, name: def.source.file.clone(), format: importer_id(def.source.format), sidecars })
}

/// The avatar `def` describes, rebuilt from its stored original (D5: import is deterministic, so the avatar is
/// rebuilt on every load rather than stored).
///
/// # Errors
/// As [`stored`] and [`prepare`].
pub(crate) fn rebuild(ctx: &omuzaki_ops_api::OpCtx<'_>, def: AvatarDef) -> Result<Prepared, OpError> {
    let original = stored(ctx, &def)?;
    let mtoon = mtoon(ctx.service::<Capabilities>().ok());
    prepare(&original.source(), Some(def), &ImportOptions::default(), mtoon, ctx.clock, ctx.doc)
}

/// The imported character `id` as the stage runs it: its avatar node read from `doc` and the avatar rebuilt from the
/// original in `store` (D5: the avatar is rebuilt on every load, never stored). `mtoon` is what the renderer draws
/// ([`Capabilities::mtoon`]). This is how the studio, the CLI and the outputs put an imported character on a stage;
/// the findings of the rebuild were shown at import and are not repeated here.
///
/// # Errors
/// `NotFound` for no such character or an original missing from the store; `Invalid` for a character built from parts
/// or a model that no longer prepares.
pub fn load_avatar(
    doc: &dyn DocRead,
    store: &dyn Store,
    id: &CharId,
    mtoon: MtoonSupport,
    clock: &dyn Clock,
) -> Result<ImportedAvatar, OpError> {
    let def = read_def(doc, id)?;
    let original = stored_in(doc, store, &def)?;
    let prepared = prepare(&original.source(), Some(def), &ImportOptions::default(), mtoon, clock, doc)?;
    require_clean(&prepared.report, "character")?;
    Ok(prepared.avatar)
}

/// True if character `id` in `doc` was imported as an avatar rather than built from parts.
#[must_use]
pub fn is_imported(doc: &dyn DocRead, id: &CharId) -> bool {
    doc.get(&paths::character(id)).is_some_and(|node| node.get("avatar").is_some())
}

/// The patch that replaces the avatar node of `id` with `def`, or an empty patch when it is already exactly that.
///
/// # Errors
/// `Internal` when the node cannot be written as JSON.
pub(crate) fn def_patch(doc: &dyn DocRead, id: &CharId, def: &AvatarDef) -> Result<omuzaki_doc_api::Patch, OpError> {
    let path = paths::character(id).join("avatar");
    let after = serde_json::to_value(def).map_err(|e| OpError::internal(format!("writing the avatar node: {e}")))?;
    let before = doc.get(&path).cloned();
    if before.as_ref() == Some(&after) {
        return Ok(omuzaki_doc_api::Patch::new());
    }
    Ok(omuzaki_doc_api::Patch::new().with(path, before, Some(after)))
}

/// The rebuild's errors, if the report has any: an edit is refused when the avatar it leads to would not prepare.
///
/// # Errors
/// `Invalid` on `field`, naming the first error finding and its hint.
pub(crate) fn require_clean(report: &Report, field: &str) -> Result<(), OpError> {
    let Some(first) = report.errors().next() else { return Ok(()) };
    let mut error = OpError::invalid(field, format!("{}: {}", first.rule, first.message));
    if let Some(hint) = &first.hint {
        error = error.hint(hint.clone());
    }
    Err(error)
}
