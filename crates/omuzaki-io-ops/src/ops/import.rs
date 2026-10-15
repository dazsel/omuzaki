//! `io.import`: sniff, `run_import`, store the original bytes and images with their provenance, and record the asset in
//! the document. A job, so it reports progress and can be cancelled.
//!
//! **Three calls** (the job contract in `omuzaki-ops-api`):
//! - `plan` validates the arguments and the access, under the lock, and changes nothing. For a `.gltf` read from a
//!   `path` it also reads the files the model names (buffers, images) through the confined [`DirResolver`] that
//!   `io.inspect` uses, and hands them to `work` as the plan's output;
//! - `work` runs off the lock: it reads the file, imports it (the files `plan` resolved are its only sidecars, so the
//!   job reads no file the plan did not), then stores the original and its images;
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
//! **Sidecars.** Only a `path` has a folder, so only a `path` import resolves a `.gltf`'s external files; `bytes` of a
//! model that names one are `Unsupported` (import a `.glb`, or pass the `path`). The input limit counts the file and
//! every sidecar together. `plan` hands the files to `work` hex encoded in its output, which is the only channel a job
//! has from `plan`.
//!
//! **Not here.** Importing as an editable part is CB-44 and answers `Unsupported`.

use std::borrow::Cow;
use std::io::Read;

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

use super::sidecar::DirResolver;
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
    /// The file's name.
    file: String,
    /// The format it was read as.
    format: String,
    /// The character it is a reference for, if one was named. Absent otherwise: the document cannot hold a `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    character: Option<String>,
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

    /// [`import_bytes`](Self::import_bytes) with `sidecars`, the files the model refers to, which are the only ones it
    /// can read. A reference not among them is `NotFound`.
    ///
    /// # Errors
    /// As `import_bytes`.
    pub fn import_bytes_with(
        &self,
        name: &str,
        bytes: &[u8],
        sidecars: &[(String, Vec<u8>)],
        options: &ImportOptions,
        store: &dyn Store,
        progress: &dyn Progress,
    ) -> Result<Imported, OpError> {
        self.import_with(name, bytes, options, store, progress, Some(sidecars))
    }

    /// The shared body; `sidecars` is `None` when the source has no folder to read them from.
    fn import_with(
        &self,
        name: &str,
        bytes: &[u8],
        options: &ImportOptions,
        store: &dyn Store,
        progress: &dyn Progress,
        sidecars: Option<&[(String, Vec<u8>)]>,
    ) -> Result<Imported, OpError> {
        let field = self.source_field();
        let input = ImportInput::new(bytes).named(name);
        let importer = self.pick_importer(&input)?;
        let planned = PlannedFiles(sidecars.unwrap_or_default());
        let resolver: &dyn Resolver = if sidecars.is_some() { &planned } else { &NoResolver };
        let ctx = ImportCtx::new(options).with_resolver(resolver).with_progress(progress);
        let outcome = run_import(&*importer, &input, &ctx).map_err(|e| import_error(e, field, sidecars.is_some()))?;
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
        let images = stored;
        Ok(Imported {
            asset: AssetRecord {
                original,
                images,
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

    /// The read: `path` through the job's file access, refusing a file over the input limit (`max_bytes`, what is left
    /// after the sidecars) before reading it.
    fn read_source(&self, job: &JobCtx<'_>, max_bytes: u64) -> Result<Cow<'_, [u8]>, OpError> {
        let Some(path) = &self.path else {
            let bytes = self.bytes.as_deref();
            return bytes.map(Cow::Borrowed).ok_or_else(|| OpError::internal("plan accepted an import with no source"));
        };
        let file = job.fs_access(path, FsMode::Read)?.open_read()?;
        let size = file.metadata().map_err(|e| OpError::invalid("path", format!("couldn't read the file: {e}")))?.len();
        check_size(size, max_bytes, "path")?;
        // The read still stops at the limit: the file may grow after the size was taken.
        let mut bytes = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| OpError::invalid("path", format!("couldn't read the file: {e}")))?;
        check_size(bytes.len() as u64, max_bytes, "path")?;
        Ok(Cow::Owned(bytes))
    }
}

/// The op error for a failed import. Bytes have no folder, so a file the model refers to is a feature that source
/// lacks, not a missing file; for a `path` (`planned`), a reference the plan did not resolve is an ordinary
/// `NotFound`.
fn import_error(error: IoError, field: &str, planned: bool) -> OpError {
    match error {
        IoError::Resource { uri, .. } if !planned => OpError::unsupported(format!(
            "the model refers to another file ({uri}), and bytes have no folder to read it from"
        ))
        .hint("import a .glb, which carries its buffers and images, or pass the model as a path"),
        other => from_io(other, field),
    }
}

/// The files `plan` resolved, served to the importer by the URI the model names. Nothing else is readable.
struct PlannedFiles<'a>(&'a [(String, Vec<u8>)]);

impl Resolver for PlannedFiles<'_> {
    fn read(&self, uri: &str) -> Result<Vec<u8>, IoError> {
        let found = self.0.iter().find(|(planned, _)| planned == uri);
        found.map(|(_, bytes)| bytes.clone()).ok_or_else(|| IoError::Resource {
            uri: uri.to_owned(),
            reason: "the file was not resolved when the import was planned".to_owned(),
        })
    }
}

/// A glTF's sidecars: the `uri` the model names and the bytes read for it.
pub type SidecarFiles = Vec<(String, Vec<u8>)>;

/// The key of the sidecars in `plan`'s output.
const SIDECARS: &str = "sidecars";

/// The `uri`s of a glTF's buffers and images that name a file: `data:` URIs are decoded by the importer itself.
/// Percent-decoded as the importer does before it asks the resolver, in file order, each once. A `uri` that is not
/// valid after decoding is left out; the importer then reports it.
fn sidecar_uris(gltf: &Value) -> Vec<String> {
    let mut uris: Vec<String> = Vec::new();
    for key in ["buffers", "images"] {
        let items = gltf.get(key).and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
        for uri in items.iter().filter_map(|item| item.get("uri")?.as_str()) {
            let is_data = uri.get(..5).is_some_and(|scheme| scheme.eq_ignore_ascii_case("data:"));
            if let Some(decoded) = (!is_data).then(|| percent_decode(uri)).flatten()
                && !uris.contains(&decoded)
            {
                uris.push(decoded);
            }
        }
    }
    uris
}

/// `text` with its `%XX` escapes decoded, or `None` when an escape is malformed or the result is not UTF-8.
fn percent_decode(text: &str) -> Option<String> {
    let raw = text.as_bytes();
    let mut out = Vec::with_capacity(raw.len());
    let mut at = 0;
    while at < raw.len() {
        if raw[at] == b'%' {
            let pair = text.get(at + 1..at + 3)?;
            out.push(u8::from_str_radix(pair, 16).ok()?);
            at += 3;
        } else {
            out.push(raw[at]);
            at += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// How many bytes of the file are read to tell JSON from a `.glb` or another binary format.
const PEEK_BYTES: u64 = 64;

/// Whether `head` starts like a JSON object: after an optional byte order mark and white space, a `{`.
fn looks_like_json(head: &[u8]) -> bool {
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(head);
    head.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'{')
}

/// Reads the files the `.gltf` at `main` names, through the same confined resolver `io.inspect` uses, each counted
/// with the file itself against `max_bytes`. `None` when the file is not a `.gltf` or names none.
///
/// # Errors
/// `Invalid` for a file over the limit; `NotFound` for a sidecar that is missing, outside the file's folder or over
/// what is left of the limit.
fn resolve_sidecars(
    ctx: &OpCtx<'_>,
    main: &CanonicalPath,
    typed: &str,
    max_bytes: u64,
) -> Result<Option<SidecarFiles>, OpError> {
    let mut file = main.open_read()?;
    let couldnt = |e: std::io::Error| OpError::invalid("path", format!("couldn't read the file: {e}"));
    let mut bytes = Vec::new();
    file.by_ref().take(PEEK_BYTES).read_to_end(&mut bytes).map_err(couldnt)?;
    if !looks_like_json(&bytes) {
        return Ok(None);
    }
    file.take(max_bytes.saturating_add(1)).read_to_end(&mut bytes).map_err(couldnt)?;
    check_size(bytes.len() as u64, max_bytes, "path")?;
    // A file that is not valid JSON is left for the importer to report.
    let Ok(gltf) = serde_json::from_slice::<Value>(&bytes) else { return Ok(None) };
    let mut used = bytes.len() as u64;
    let mut files = Vec::new();
    for uri in sidecar_uris(&gltf) {
        let read = DirResolver::new(ctx, main, typed, max_bytes - used).read(&uri).map_err(|e| from_io(e, "path"))?;
        used += read.len() as u64;
        files.push((uri, read));
    }
    Ok((!files.is_empty()).then_some(files))
}

/// One sidecar as `plan`'s output carries it. Hex rather than a number per byte: a JSON number costs a few dozen bytes
/// in memory, a hex digit pair costs two characters.
#[derive(Serialize, Deserialize)]
struct SidecarJson {
    uri: String,
    hex: String,
}

fn encode_sidecars(files: &[(String, Vec<u8>)]) -> Result<Value, OpError> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let list: Vec<SidecarJson> = files
        .iter()
        .map(|(uri, bytes)| SidecarJson {
            uri: uri.clone(),
            hex: bytes.iter().flat_map(|b| [DIGITS[usize::from(b >> 4)], DIGITS[usize::from(b & 15)]]).map(char::from).collect(),
        })
        .collect();
    serde_json::to_value(list).map_err(|e| OpError::internal(format!("serialising the sidecars: {e}")))
}

/// The sidecars `plan` resolved, from the job's plan output; empty when it resolved none.
fn planned_sidecars(job: &JobCtx<'_>) -> Result<SidecarFiles, OpError> {
    let malformed = |why: String| OpError::internal(format!("the planned sidecars are malformed: {why}"));
    let Some(listed) = job.plan_output().and_then(|output| output.get(SIDECARS)) else { return Ok(Vec::new()) };
    let list: Vec<SidecarJson> = serde_json::from_value(listed.clone()).map_err(|e| malformed(e.to_string()))?;
    list.into_iter()
        .map(|SidecarJson { uri, hex }| {
            let digits = hex.as_bytes();
            let bytes = digits
                .chunks(2)
                .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
                .collect::<Option<Vec<u8>>>()
                .filter(|_| digits.len() % 2 == 0)
                .ok_or_else(|| malformed(format!("{uri} is not hex")))?;
            Ok((uri, bytes))
        })
        .collect()
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
        let mut sidecars = None;
        match (&self.path, &self.bytes) {
            (Some(path), None) => {
                if path.is_empty() {
                    return Err(OpError::invalid("path", "the path is empty").hint("pass the path of a model file"));
                }
                // Fails now, not after the job is queued, when the actor may not read the file.
                let main = ctx.fs_access(path, FsMode::Read)?;
                sidecars = resolve_sidecars(ctx, &main, path, max_bytes)?;
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
        let output = sidecars
            .map(|files| encode_sidecars(&files).map(|list| serde_json::json!({ SIDECARS: list })))
            .transpose()?;
        Ok(Plan { output, ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let store = job.service::<dyn Store>()?;
        let options = ImportOptions::default();
        let outcome = job.checkpoint("reading", 0.0).and_then(|()| {
            let sidecars = planned_sidecars(job)?;
            // The limit counts the file and its sidecars together.
            let sidecar_bytes: u64 = sidecars.iter().map(|(_, bytes)| bytes.len() as u64).sum();
            let bytes = self.read_source(job, options.limits.max_input_bytes.saturating_sub(sidecar_bytes))?;
            let (name, progress) = (self.file_name(), JobProgress(job));
            if self.path.is_some() {
                self.import_bytes_with(&name, &bytes, &sidecars, &options, store, &progress)
            } else {
                self.import_bytes(&name, &bytes, &options, store, &progress)
            }
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

    #[test]
    fn sidecar_uris_are_the_decoded_file_references_once_each() {
        let gltf = serde_json::json!({
            "buffers": [{ "uri": "a%20b.bin" }, { "uri": "data:application/octet-stream;base64,AA==" }, { "uri": "a b.bin" }],
            "images": [{ "uri": "tex/x.png" }, { "bufferView": 0 }, { "uri": "DATA:image/png;base64,AA==" }, { "uri": "%zz" }],
        });
        assert_eq!(sidecar_uris(&gltf), ["a b.bin", "tex/x.png"]);
    }

    #[test]
    fn json_is_told_from_a_glb_by_its_first_byte() {
        assert!(looks_like_json(b"  \n{\"asset\""));
        assert!(looks_like_json(b"\xEF\xBB\xBF{"));
        assert!(!looks_like_json(b"glTF\x02\0\0\0"));
        assert!(!looks_like_json(b""));
    }

    #[test]
    #[allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]
    fn sidecars_count_against_the_input_limit_with_the_file() {
        use omuzaki_core::time::FakeClock;
        use omuzaki_ops_api::{Actor, Grants, ProjectDir};
        let dir = tempfile::tempdir().unwrap();
        let gltf = br#"{"buffers":[{"uri":"a.bin"}],"images":[{"uri":"b.png"}]}"#;
        std::fs::write(dir.path().join("m.gltf"), gltf).unwrap();
        std::fs::write(dir.path().join("a.bin"), [0_u8; 10]).unwrap();
        std::fs::write(dir.path().join("b.png"), [0_u8; 10]).unwrap();
        let (doc, actor, clock) = (omuzaki_doc_mock::MemDoc::new(), Actor::Person { name: "t".into() }, FakeClock::default());
        let (grants, project) = (Grants::person(), ProjectDir::open(dir.path()).unwrap());
        let ctx = OpCtx::new(&doc, &actor, &clock).with_grants(&grants).with_project_dir(&project);
        let main = ctx.fs_access("m.gltf", FsMode::Read).unwrap();
        let total = gltf.len() as u64 + 20;
        let files = resolve_sidecars(&ctx, &main, "m.gltf", total).unwrap().unwrap();
        assert_eq!(files.iter().map(|(uri, _)| uri.as_str()).collect::<Vec<_>>(), ["a.bin", "b.png"]);
        let error = resolve_sidecars(&ctx, &main, "m.gltf", total - 1).unwrap_err();
        assert!(error.message.contains("b.png") && error.message.contains("byte limit"), "{}", error.message);
        let error = resolve_sidecars(&ctx, &main, "m.gltf", gltf.len() as u64 - 1).unwrap_err();
        assert_eq!(error.code, omuzaki_ops_api::ErrorCode::Invalid, "the file alone is over the limit");
    }

    #[test]
    fn the_same_asset_recorded_twice_is_an_empty_patch() {
        let asset = AssetRecord {
            original: ContentHash::of(b"a"),
            images: vec![ContentHash::of(b"b")],
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
