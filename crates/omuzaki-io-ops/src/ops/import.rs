//! `io.import`: sniff, `run_import`, store the original bytes and images with their provenance, and record the asset in
//! the document. A job, so it reports progress and can be cancelled.
//!
//! **Three calls** (the job contract in `omuzaki-ops-api`):
//! - `plan` validates the arguments and the access, under the lock, and changes nothing;
//! - `work` runs off the lock: it reads the file, imports it, then stores the original, its images and the files it
//!   refers to;
//! - `finish` runs under the lock and returns the patch that records the asset, so the document changes once.
//!
//! **What is stored.** The original file's bytes, unchanged, with the provenance `run_import` derived from the file's
//! rights (io-api D5: the conservative mapping, `Unknown` wherever the file does not settle it), and each image of the
//! scene under that same record, so an image never carries more rights than the model it came from. The store is
//! content addressed, so importing the same bytes again stores nothing new.
//! An asset already recorded is never overwritten: a re-import without a character, or with the same one, changes
//! nothing, and one with a different character is a `Conflict` (several characters per asset is #1792).
//! An object is shared with another import only under an identical record; the check runs before the first write and again
//! after each `put`, and any difference is a `Conflict` that names both records (joining records is #1773).
//!
//! **Cancellation.** The import is cancellable at every checkpoint the importer reaches. The store phase is not
//! interrupted: the last check comes before the first `put`. The guarantee:
//! - a cancel seen before the first `put` stores nothing;
//! - a cancel requested at any point before `work` returns, including while the objects are written, makes `work` return
//!   `Cancelled`, so no asset record is written: the record exists only in the patch `finish` returns;
//! - the objects already put are whole, because `put` is atomic and content addressed;
//! - those objects are referenced by nothing, so garbage collection removes them (the `StoreGc` contract, ADR 0021).
//!
//! **Sidecars.** The files a `.gltf` refers to (`.bin`, images) are read by `work`, when the importer asks for them,
//! through the job's file access and confined to the folder of the file the person named, with the rules of
//! `io.inspect`'s resolver (`ops::sidecar`): no `..`, no absolute path, no link out of the folder. Only a `path` has a
//! folder; `bytes` of a model that names a file are `Unsupported`. The input limit counts the file and every sidecar
//! together. Each sidecar is stored like an image, under the same record, and the asset records its `uri` and hash, so
//! nothing the import used is lost. (Reloading such an asset through `io.inspect {asset}` needs that op to serve the
//! stored sidecars; it does not yet.)
//!
//! **Not here.** Importing as an editable part is CB-44 and answers `Unsupported`.

use std::borrow::Cow;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use omuzaki_doc_api::{DocPath, DocRead, Patch, Value};
use omuzaki_io_api::{
    FormatId, ImportCtx, ImportInput, ImportOptions, Importer, IoError, NoResolver, Progress, Report, Resolver,
    SceneSummary, find_importer, formats, run_import, sniff,
};
use omuzaki_ops_api::{
    CanonicalPath, FsMode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op,
};
use omuzaki_store_api::{ContentHash, Provenance, Source, Store, StoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::sidecar::clean_relative;
use crate::error::from_io;

/// The name a byte import is given when the caller names none.
const DEFAULT_NAME: &str = "model";

/// Where the document records imported assets, one entry per original, keyed by its content hash.
const ASSETS: &str = "assets";

/// What the imported file becomes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportAs {
    /// Kept as something to look at and trace: the document records the asset, no part is created.
    #[default]
    Reference,
    /// An editable part of a character (CB-44, not built yet).
    Part,
}

/// Imports a model file and records it as an asset of the project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Import {
    /// The model file to read. Paths outside the project folder need the `fs` grant and a root covering them.
    /// Pass exactly one of `path` and `bytes`.
    #[serde(default)]
    pub path: Option<String>,
    /// The file's bytes, for a caller that has no file. Pass exactly one of `path` and `bytes`.
    #[serde(default)]
    pub bytes: Option<Vec<u8>>,
    /// The file's name, used to recognise the format and to record where the asset came from. Defaults to the name in
    /// `path`.
    #[serde(default)]
    pub name: Option<String>,
    /// The format to read it as (see `io.list_formats`). Detected from the file's contents when omitted.
    #[serde(default)]
    pub format: Option<FormatId>,
    /// The character the asset is a reference for. It must exist.
    #[serde(default)]
    pub character: Option<String>,
    /// What the file becomes. Only `reference` is available.
    #[serde(default, rename = "as")]
    pub as_: ImportAs,
}

/// What the document keeps about one imported original.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct AssetRecord {
    /// The original file's content hash.
    original: ContentHash,
    /// The hashes of its images, in order, each once.
    images: Vec<ContentHash>,
    /// The files the model refers to, as it names them, each with the hash of its stored bytes. Absent when none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sidecars: Vec<SidecarRecord>,
    /// The file's name.
    file: String,
    /// The format it was read as.
    format: String,
    /// The character it is a reference for, if one was named. Absent otherwise: the document cannot hold a `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    character: Option<String>,
}

/// One file a model refers to, as the document records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SidecarRecord {
    /// The URI as the model names it (percent-decoded), relative to the model's folder.
    uri: String,
    /// The content hash of the file's stored bytes.
    hash: ContentHash,
}

/// What a finished import reports.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Imported {
    /// The stored asset, as the document records it.
    asset: AssetRecord,
    /// Counts of what the file holds.
    pub summary: SceneSummary,
    /// What was noticed on the way, including the rights notes.
    pub report: Report,
    /// The record the original was stored with.
    pub provenance: Provenance,
}

impl Imported {
    /// The content hash of the stored original.
    pub fn original(&self) -> ContentHash {
        self.asset.original
    }

    /// The content hashes of the stored images, in scene order, each once.
    pub fn images(&self) -> &[ContentHash] {
        &self.asset.images
    }
}

/// Reports an import's stages to the job.
struct JobProgress<'a, 'j>(&'a JobCtx<'j>);

impl Progress for JobProgress<'_, '_> {
    fn stage(&self, name: &str, fraction: f32) {
        self.0.progress(name, fraction);
    }

    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl Import {
    /// Imports `bytes` as the file called `name`, then stores the original and its images in `store`.
    ///
    /// Nothing is stored unless the import succeeds and `progress` has not been cancelled by the time storing starts.
    ///
    /// # Errors
    /// - `Unsupported` when no importer recognises the bytes, and when the model refers to other files;
    /// - `Invalid` for a `format` nobody registered, and for files that are malformed, over `options.limits`, or
    ///   rejected by the scene checks (the field blames `path` or `bytes`, whichever the file came from);
    /// - `Conflict` when `progress` reports a cancel, and when an object is already stored under a record that differs
    ///   from this model's, checked before the first write and again after each `put`;
    /// - `Internal` when the store fails or `run_import` leaves the scene without a record.
    pub fn import_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        options: &ImportOptions,
        store: &dyn Store,
        progress: &dyn Progress,
    ) -> Result<Imported, OpError> {
        self.import_with(name, bytes, options, store, progress, None)
    }

    /// The shared body; `folder` serves the model's sidecars, and is `None` when the source has no folder.
    fn import_with(
        &self,
        name: &str,
        bytes: &[u8],
        options: &ImportOptions,
        store: &dyn Store,
        progress: &dyn Progress,
        folder: Option<&FolderFiles<'_, '_>>,
    ) -> Result<Imported, OpError> {
        let field = self.source_field();
        let input = ImportInput::new(bytes).named(name);
        let importer = self.pick_importer(&input)?;
        let resolver: &dyn Resolver = match folder {
            Some(folder) => folder,
            None => &NoResolver,
        };
        let ctx = ImportCtx::new(options).with_resolver(resolver).with_progress(progress);
        let outcome = run_import(&*importer, &input, &ctx).map_err(|e| import_error(e, field, folder.is_some()))?;
        let served = folder.map(FolderFiles::served).transpose()?.unwrap_or_default();
        // The last chance to stop: past this point the objects are written without further checks.
        progress.checkpoint("storing", 0.9).map_err(|e| from_io(e, field))?;
        let provenance = outcome
            .scene
            .provenance
            .clone()
            .ok_or_else(|| OpError::internal("run_import left the scene without a provenance record"))?;
        let mut objects = vec![(ContentHash::of(bytes), bytes, provenance.clone())];
        for (ix, image) in outcome.scene.images.iter().enumerate() {
            let hash = ContentHash::of(image.bytes.as_slice());
            // The same picture used twice in one model is one object, recorded by its first use.
            if objects.iter().any(|(seen, ..)| *seen == hash) {
                continue;
            }
            let record = Provenance {
                source: Source::Imported { file: format!("{name}#image{ix}"), origin: None },
                ..provenance.clone()
            };
            objects.push((hash, image.bytes.as_slice(), record));
        }
        let image_objects = objects.len() - 1;
        let mut sidecars = Vec::with_capacity(served.len());
        for (uri, file) in &served {
            let hash = ContentHash::of(file);
            sidecars.push(SidecarRecord { uri: uri.clone(), hash });
            // A file the model also uses as an image is already an object, recorded by its first use.
            if objects.iter().all(|(seen, ..)| *seen != hash) {
                let record = Provenance {
                    source: Source::Imported { file: format!("{name}#{uri}"), origin: None },
                    ..provenance.clone()
                };
                objects.push((hash, file.as_slice(), record));
            }
        }
        // Checked for every object before the first write, so a refusal stores nothing.
        for (hash, _, record) in &objects {
            if store.contains(hash) {
                require_same_record(store, hash, record, name)?;
            }
        }
        let mut stored = Vec::with_capacity(objects.len());
        for (hash, bytes, record) in objects {
            let put = store
                .put(bytes, record.clone())
                .map_err(|e: StoreError| OpError::internal(format!("storing {name}: {e}")))?;
            debug_assert_eq!(put, hash, "the store names an object by the hash of its bytes");
            // `put` keeps the first record, and another import may have stored the object since the check above.
            require_same_record(store, &put, &record, name)?;
            stored.push(put);
        }
        let original = stored.remove(0);
        stored.truncate(image_objects);
        let images = stored;
        Ok(Imported {
            asset: AssetRecord {
                original,
                images,
                sidecars,
                file: name.to_owned(),
                format: importer.info().id.to_owned(),
                character: self.character.clone(),
            },
            summary: outcome.scene.summary(),
            report: outcome.report,
            provenance,
        })
    }

    /// The importer to use: the one named by `format`, otherwise the best the sniff finds.
    fn pick_importer(&self, input: &ImportInput<'_>) -> Result<Box<dyn Importer>, OpError> {
        if let Some(format) = &self.format {
            return find_importer(format.as_str()).ok_or_else(|| {
                OpError::invalid("format", format!("no importer is registered for format {format}"))
                    .hint(importable_formats())
            });
        }
        let best = sniff(input).into_iter().next().ok_or_else(|| {
            OpError::unsupported("no registered importer recognises this file")
                .hint(format!("{}; if it is one of them it may be damaged or truncated", importable_formats()))
        })?;
        find_importer(best.format)
            .ok_or_else(|| OpError::internal(format!("importer {} vanished after sniffing", best.format)))
    }

    /// The argument the bytes came from, which errors about the file blame.
    fn source_field(&self) -> &'static str {
        if self.path.is_some() { "path" } else { "bytes" }
    }

    /// The file's name: `name`, else the last part of `path`, else [`DEFAULT_NAME`].
    fn file_name(&self) -> String {
        let from_path = self.path.as_deref().and_then(|p| std::path::Path::new(p).file_name()?.to_str());
        self.name.clone().or_else(|| from_path.map(str::to_owned)).unwrap_or_else(|| DEFAULT_NAME.to_owned())
    }

    /// The read: `path` through the job's file access, refusing a file over the input limit before reading it. Also
    /// returns where the file really is, for a `path`, so its sidecars can be confined to its folder.
    fn read_source(&self, job: &JobCtx<'_>, max_bytes: u64) -> Result<(Cow<'_, [u8]>, Option<CanonicalPath>), OpError> {
        let Some(path) = &self.path else {
            let bytes = self.bytes.as_deref();
            let bytes = bytes.map(Cow::Borrowed).ok_or_else(|| OpError::internal("plan accepted an import with no source"))?;
            return Ok((bytes, None));
        };
        let main = job.fs_access(path, FsMode::Read)?;
        let file = main.open_read()?;
        let size = file.metadata().map_err(|e| OpError::invalid("path", format!("couldn't read the file: {e}")))?.len();
        check_size(size, max_bytes, "path")?;
        // The read still stops at the limit: the file may grow after the size was taken.
        let mut bytes = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| OpError::invalid("path", format!("couldn't read the file: {e}")))?;
        check_size(bytes.len() as u64, max_bytes, "path")?;
        Ok((Cow::Owned(bytes), Some(main)))
    }
}

/// The op error for a failed import. Bytes have no folder, so a file the model refers to is a feature that source
/// lacks, not a missing file; for a `path` (`has_folder`), an unreadable reference is an ordinary `NotFound`.
fn import_error(error: IoError, field: &str, has_folder: bool) -> OpError {
    match error {
        IoError::Resource { uri, .. } if !has_folder => OpError::unsupported(format!(
            "the model refers to another file ({uri}), and bytes have no folder to read it from"
        ))
        .hint("import a .glb, which carries its buffers and images, or pass the model as a path"),
        other => from_io(other, field),
    }
}

/// The files read for one import, with the bytes of each, in the order the importer first asked for them.
type Served = Vec<(String, Vec<u8>)>;

/// Serves a `.gltf`'s sidecars to the importer from the folder of the file the person named, through the job's file
/// access, and remembers what it served so the import can store it.
///
/// The confinement is that of `ops::sidecar::DirResolver`, which needs a plan-time `OpCtx` and so cannot run here: the
/// URI must be a plain relative path ([`clean_relative`]), is joined onto the folder as the person typed it, goes
/// through `fs_access` (grants, roots, no links), and must really lie inside the file's real folder.
struct FolderFiles<'a, 'j> {
    job: &'a JobCtx<'j>,
    /// The folder as the person typed it (empty for the project folder itself).
    folder_text: String,
    /// The file's real folder; every sidecar's real place must start with it.
    folder: PathBuf,
    /// The input limit, shared by the file and all its sidecars.
    max_bytes: u64,
    /// What has been served and the bytes counted against the limit (the file itself counts from the start).
    state: Mutex<(Served, u64)>,
}

impl<'a, 'j> FolderFiles<'a, 'j> {
    /// The sidecars of `main`, which the person typed as `typed`. `file_bytes` is its size, already counted.
    fn new(job: &'a JobCtx<'j>, main: &CanonicalPath, typed: &str, max_bytes: u64, file_bytes: u64) -> Self {
        let folder_text = Path::new(typed).parent().and_then(Path::to_str).unwrap_or("").to_owned();
        // A file always has a parent; if one ever did not, the file itself is the fallback, so no sidecar lies inside
        // it and every read is refused rather than widened.
        let real = PathBuf::from(main.to_string());
        let folder = real.parent().map_or_else(|| real.clone(), Path::to_path_buf);
        Self { job, folder_text, folder, max_bytes, state: Mutex::new((Vec::new(), file_bytes)) }
    }

    /// What was served, taking it from the resolver.
    fn served(&self) -> Result<Served, OpError> {
        let mut state = self.state.lock().map_err(|_| OpError::internal("the sidecar list was poisoned"))?;
        Ok(std::mem::take(&mut state.0))
    }
}

/// A refusal of the form or the place of `uri`, worded as `DirResolver`'s.
fn refused(uri: &str, why: &str) -> IoError {
    IoError::Resource { uri: uri.to_owned(), reason: format!("refused: {why}") }
}

impl Resolver for FolderFiles<'_, '_> {
    fn read(&self, uri: &str) -> Result<Vec<u8>, IoError> {
        let poisoned = || IoError::Internal("the sidecar list was poisoned".to_owned());
        // Held for the whole read: the importer may ask for files from several threads, and the limit is one total that
        // each read must see the result of the one before it.
        let mut state = self.state.lock().map_err(|_| poisoned())?;
        if let Some((_, bytes)) = state.0.iter().find(|(served, _)| served == uri) {
            return Ok(bytes.clone());
        }
        let used = state.1;
        let relative = clean_relative(uri).map_err(|why| refused(uri, &why))?;
        let joined = Path::new(&self.folder_text).join(relative);
        let joined = joined.to_str().ok_or_else(|| refused(uri, "the path is not text this machine can name"))?;
        let resource = |reason: String| IoError::Resource { uri: uri.to_owned(), reason };
        let sidecar = self.job.fs_access(joined, FsMode::Read).map_err(|e| resource(e.message))?;
        if !Path::new(&sidecar.to_string()).starts_with(&self.folder) {
            return Err(refused(uri, "the file is not in the folder of the importing file"));
        }
        let file = sidecar.open_read().map_err(|e| resource(e.message))?;
        // One byte past what is left tells an over-long file from one that fits, and stops the read there.
        let left = self.max_bytes.saturating_sub(used);
        let mut bytes = Vec::new();
        file.take(left.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| resource(e.to_string()))?;
        let total = used.saturating_add(bytes.len() as u64);
        if total > self.max_bytes {
            return Err(IoError::Limit { what: "input size in bytes", limit: self.max_bytes, got: total });
        }
        state.1 = total;
        state.0.push((uri.to_owned(), bytes.clone()));
        Ok(bytes)
    }
}

/// Removes every `null` from `value`: the result is shown in history and a stored document cannot hold one.
fn strip_nulls(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.retain(|_, v| !v.is_null());
            map.values_mut().for_each(strip_nulls);
        }
        Value::Array(items) => items.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

/// One record on a line, for the error that names two of them.
fn describe_record(record: &Provenance) -> String {
    format!(
        "licence {}, commercial {:?}, attribution {:?}, private test {}, author {:?}, consent {:?}, source {:?}",
        record.licence,
        record.commercial,
        record.attribution,
        record.private_test,
        record.author,
        record.consent,
        record.source
    )
}

/// Requires the store to hold `hash` under exactly `wanted`. `Store::put` keeps the first record and the store offers
/// no way to join two, so an object is shared between imports only when their records are identical; anything else
/// (wider, stricter, another licence or credit line) fails closed. Joining records is follow-up #1773.
fn require_same_record(store: &dyn Store, hash: &ContentHash, wanted: &Provenance, name: &str) -> Result<(), OpError> {
    let stored = store
        .provenance(hash)
        .map_err(|e| OpError::internal(format!("reading the record of {hash}: {e}")))?;
    if &stored == wanted {
        return Ok(());
    }
    Err(OpError::conflict(format!(
        "{name} holds an object ({hash}) that the store has under a different record. Stored: {}. This model: {}",
        describe_record(&stored),
        describe_record(wanted),
    ))
    .hint("an object is shared between imports only under identical rights; sharing across different rights is not built yet (#1773)"))
}

/// `Invalid` over the input limit, the same error `run_import` gives, so a hostile file is refused before it is held.
fn check_size(got: u64, max_bytes: u64, field: &str) -> Result<(), OpError> {
    if got > max_bytes {
        return Err(from_io(IoError::Limit { what: "input size in bytes", limit: max_bytes, got }, field));
    }
    Ok(())
}

/// "Importable formats: glb, vrm1", for hints.
fn importable_formats() -> String {
    let ids: Vec<&str> = formats().iter().filter(|f| f.import).map(|f| f.info.id).collect();
    if ids.is_empty() {
        "this build has no importers".to_owned()
    } else {
        format!("importable formats: {}", ids.join(", "))
    }
}

/// The character `id` must exist in the document.
fn require_character(doc: &dyn DocRead, id: &str) -> Result<(), OpError> {
    let path = DocPath::root().join("characters").join(id);
    if doc.get(&path).is_some() {
        return Ok(());
    }
    Err(OpError::not_found(format!("no character {id}")).hint("pass the id of a character in the project"))
}

/// The patch that records `asset` under `assets/<original>`, creating `assets` when the document has none (a change
/// needs its parent to exist).
///
/// An entry that already exists is never overwritten, so a re-import cannot drop what it recorded. With no `character`,
/// or the same one, the patch is empty; a `character` on an entry that has none is added; a different `character` is a
/// `Conflict` naming the existing one (several characters per asset is #1792).
fn record_patch(doc: &dyn DocRead, asset: &AssetRecord) -> Result<Patch, OpError> {
    let value = serde_json::to_value(asset).map_err(|e| OpError::internal(format!("serialising the asset: {e}")))?;
    let table = DocPath::root().join(ASSETS);
    let entry = table.join(asset.original.to_string());
    match doc.get(&table) {
        None => {
            let created = Value::Object(std::iter::once((asset.original.to_string(), value)).collect());
            Ok(Patch::new().with(table, None, Some(created)))
        }
        Some(Value::Object(_)) => match doc.get(&entry) {
            None => Ok(Patch::new().with(entry, None, Some(value))),
            Some(existing) => keep_existing(existing, asset, entry),
        },
        Some(_) => Err(OpError::conflict(format!("{ASSETS} in the document is not a table"))
            .hint("the document is damaged; restore it from history")),
    }
}

/// The patch for an asset the document already records: see [`record_patch`].
fn keep_existing(existing: &Value, asset: &AssetRecord, entry: DocPath) -> Result<Patch, OpError> {
    let Value::Object(fields) = existing else {
        return Err(OpError::conflict(format!("the record at {entry} is not an object"))
            .hint("the document is damaged; restore it from history"));
    };
    // The same original with other files behind it (a sidecar edited since) is not the recorded asset: keeping the old
    // record would leave the new objects referenced by nothing.
    let files = |value: Option<&Value>| value.cloned().filter(|v| v.as_array().is_none_or(|a| !a.is_empty()));
    for (field, wanted) in [("images", serde_json::to_value(&asset.images)), ("sidecars", serde_json::to_value(&asset.sidecars))] {
        let wanted = wanted.map_err(|e| OpError::internal(format!("serialising the asset: {e}")))?;
        if files(fields.get(field)) != files(Some(&wanted)) {
            return Err(OpError::conflict(format!(
                "this file is already recorded with different {field} ({entry}), so a file it refers to changed since it was imported"
            ))
            .hint("an asset keeps one version of the files behind it for now; restore the files as they were, or remove the recorded asset first"));
        }
    }
    let recorded = fields.get("character").and_then(Value::as_str);
    match (recorded, asset.character.as_deref()) {
        (_, None) => Ok(Patch::new()),
        (Some(have), Some(want)) if have == want => Ok(Patch::new()),
        (Some(have), Some(want)) => Err(OpError::conflict(format!(
            "this asset is already recorded as a reference for character {have}, so it cannot also be one for {want}"
        ))
        .hint("an asset belongs to one character for now (#1792); import it without a character to keep the record")),
        (None, Some(want)) => {
            let mut updated = fields.clone();
            updated.insert("character".to_owned(), Value::String(want.to_owned()));
            Ok(Patch::new().with(entry, Some(existing.clone()), Some(Value::Object(updated))))
        }
    }
}

impl Op for Import {
    fn meta() -> OpMeta {
        OpMeta {
            name: "io.import",
            summary: "Import a model file (glTF, VRM): store the original and its images with their licence record, \
                      and record the asset in the project.",
            kind: OpKind::Job,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.as_ == ImportAs::Part {
            return Err(OpError::unsupported("importing as an editable part is not available yet")
                .hint("import it as a reference"));
        }
        let max_bytes = ImportOptions::default().limits.max_input_bytes;
        match (&self.path, &self.bytes) {
            (Some(path), None) => {
                if path.is_empty() {
                    return Err(OpError::invalid("path", "the path is empty").hint("pass the path of a model file"));
                }
                // Fails now, not after the job is queued, when the actor may not read the file.
                ctx.fs_access(path, FsMode::Read)?;
            }
            (None, Some(bytes)) => {
                if bytes.is_empty() {
                    return Err(OpError::invalid("bytes", "no bytes were given"));
                }
                check_size(bytes.len() as u64, max_bytes, "bytes")?;
            }
            _ => {
                return Err(OpError::invalid("path", "pass exactly one of path or bytes")
                    .hint("path reads a file; bytes carries the file's content"));
            }
        }
        if let Some(format) = &self.format
            && find_importer(format.as_str()).is_none()
        {
            return Err(OpError::invalid("format", format!("no importer is registered for format {format}"))
                .hint(importable_formats()));
        }
        if let Some(id) = &self.character {
            require_character(ctx.doc, id)?;
        }
        // The store is needed by `work`; its absence is better reported here.
        ctx.service::<dyn Store>()?;
        Ok(Plan::default())
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let store = job.service::<dyn Store>()?;
        let options = ImportOptions::default();
        let outcome = job.checkpoint("reading", 0.0).and_then(|()| {
            let max_bytes = options.limits.max_input_bytes;
            let (bytes, main) = self.read_source(job, max_bytes)?;
            let (name, progress) = (self.file_name(), JobProgress(job));
            let typed = self.path.as_deref().unwrap_or_default();
            let folder = main.map(|main| FolderFiles::new(job, &main, typed, max_bytes, bytes.len() as u64));
            self.import_with(&name, &bytes, &options, store, &progress, folder.as_ref())
        });
        // A cancelled import surfaces from the importer as a plain conflict; the job reports it as cancelled.
        let imported = outcome.map_err(|e| if job.is_cancelled() { OpError::cancelled(job.id()) } else { e })?;
        let mut result = serde_json::to_value(imported)
            .map_err(|e| OpError::internal(format!("serialising the import: {e}")))?;
        strip_nulls(&mut result);
        // The manager applies `finish` for work that succeeded even after a cancel request, so the cancel check is the
        // last step before the result is returned: nothing may run between it and the return.
        job.checkpoint("done", 1.0)?;
        Ok(result)
    }

    fn finish(&self, ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        let asset = result
            .get("asset")
            .cloned()
            .map(serde_json::from_value::<AssetRecord>)
            .transpose()
            .map_err(|e| OpError::internal(format!("the import's result is malformed: {e}")))?
            .ok_or_else(|| OpError::internal("the import's result has no asset"))?;
        // The character may have been removed while the job ran.
        if let Some(id) = &asset.character {
            require_character(ctx.doc, id)?;
        }
        let patch = record_patch(ctx.doc, &asset)?;
        Ok(Plan { patch, output: Some(result), ..Plan::default() })
    }
}

register_op!(Import);

#[cfg(test)]
mod tests {
    use omuzaki_doc_api::Change;

    use super::*;

    /// A job with nothing to report to and no document.
    struct Quiet(omuzaki_ops_api::CancelFlag);

    impl omuzaki_ops_api::JobSignal for Quiet {
        fn progress(&self, _stage: &str, _fraction: f32) {}

        fn is_cancelled(&self) -> bool {
            false
        }

        fn cancel_flag(&self) -> omuzaki_ops_api::CancelFlag {
            self.0.clone()
        }

        fn spend(&self, _usd_micros: u64) -> Result<(), OpError> {
            Ok(())
        }
    }

    struct NoDoc;

    impl omuzaki_ops_api::JobDoc for NoDoc {
        fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
            f(&omuzaki_doc_mock::MemDoc::new());
        }
    }

    #[test]
    #[allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]
    fn sidecars_count_against_the_input_limit_with_the_file() {
        use omuzaki_ops_api::{Actor, Grants, JobId, ProjectDir};
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("m.gltf"), [0_u8; 5]).unwrap();
        std::fs::write(dir.path().join("a.bin"), [1_u8; 10]).unwrap();
        std::fs::write(dir.path().join("b.png"), [2_u8; 10]).unwrap();
        let (actor, signal, doc) = (Actor::Person { name: "t".into() }, Quiet(omuzaki_ops_api::CancelFlag::new()), NoDoc);
        let (grants, project) = (Grants::person(), ProjectDir::open(dir.path()).unwrap());
        let job = JobCtx::new(JobId(1), &actor, &doc, &signal).with_grants(&grants).with_project_dir(&project);
        let main = job.fs_access("m.gltf", FsMode::Read).unwrap();

        // The file (5) and both sidecars (10 each) fit exactly in 25 bytes; the same file is served again for free.
        let folder = FolderFiles::new(&job, &main, "m.gltf", 25, 5);
        assert_eq!(folder.read("a.bin").unwrap(), [1_u8; 10]);
        assert_eq!(folder.read("b.png").unwrap(), [2_u8; 10]);
        assert_eq!(folder.read("a.bin").unwrap(), [1_u8; 10]);
        assert_eq!(folder.served().unwrap().len(), 2);

        // One byte less and the second sidecar is the one that tips it over.
        let folder = FolderFiles::new(&job, &main, "m.gltf", 24, 5);
        folder.read("a.bin").unwrap();
        let error = from_io(folder.read("b.png").unwrap_err(), "path");
        assert_eq!(error.code, omuzaki_ops_api::ErrorCode::Invalid, "{}", error.message);
        assert!(error.message.contains("input size in bytes"), "{}", error.message);
    }

    #[test]
    fn the_same_asset_recorded_twice_is_an_empty_patch() {
        let asset = AssetRecord {
            original: ContentHash::of(b"a"),
            images: vec![ContentHash::of(b"b")],
            sidecars: Vec::new(),
            file: "a.glb".into(),
            format: "glb".into(),
            character: None,
        };
        let doc = omuzaki_doc_mock::MemDoc::new();
        let first = record_patch(&doc, &asset).unwrap();
        assert_eq!(first.changes.len(), 1, "the table and its first entry are one change");
        let mut doc = omuzaki_doc_mock::MemDoc::from_value(serde_json::json!({ "assets": {} }));
        let entry = DocPath::root().join(ASSETS).join(asset.original.to_string());
        let Change { after, .. } = record_patch(&doc, &asset).unwrap().changes.remove(0);
        assert_eq!(after, Some(serde_json::to_value(&asset).unwrap()));
        omuzaki_doc_api::DocWrite::apply(&mut doc, &Patch::new().with(entry, None, after)).unwrap();
        assert_eq!(record_patch(&doc, &asset).unwrap(), Patch::new());
    }
}
