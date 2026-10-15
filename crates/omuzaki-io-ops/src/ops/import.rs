//! `io.import`: sniff, `run_import`, store the original bytes and images with their provenance, and record the asset in
//! the document. A job, so it reports progress and can be cancelled.
//!
//! **Three calls** (the job contract in `omuzaki-ops-api`):
//! - `plan` validates the arguments and the access, under the lock, and changes nothing;
//! - `work` runs off the lock: it reads the file, imports it, then stores the original and its images;
//! - `finish` runs under the lock and returns the patch that records the asset, so the document changes once.
//!
//! **What is stored.** The original file's bytes, unchanged, with the provenance `run_import` derived from the file's
//! rights (io-api D5: the conservative mapping, `Unknown` wherever the file does not settle it), and each image of the
//! scene under that same record, so an image never carries more rights than the model it came from. The store is
//! content addressed, so importing the same bytes again stores nothing new.
//!
//! **Cancellation.** The import is cancellable at every checkpoint the importer reaches. The store phase is not
//! interrupted: the last check comes before the first `put`, so a cancelled job has stored nothing.
//!
//! **Not here.** Importing as an editable part is CB-44 and answers `Unsupported`. Files a `.gltf` refers to are not
//! read (the resolver needs the plan-time context): import a `.glb`, which carries its buffers and images.

use std::borrow::Cow;
use std::io::Read;

use omuzaki_doc_api::{DocPath, DocRead, Patch, Value};
use omuzaki_io_api::{
    FormatId, ImportCtx, ImportInput, ImportOptions, Importer, IoError, NoResolver, Progress, Report, SceneSummary,
    find_importer, formats, run_import, sniff,
};
use omuzaki_ops_api::{FsMode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store, StoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
    /// `Unsupported` when no importer recognises the bytes; `Invalid` for a `format` nobody registered, and for files
    /// that are malformed, over `options.limits`, or rejected by the scene checks; `Conflict` when cancelled;
    /// `Internal` when the store fails.
    pub fn import_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        options: &ImportOptions,
        store: &dyn Store,
        progress: &dyn Progress,
    ) -> Result<Imported, OpError> {
        let input = ImportInput::new(bytes).named(name);
        let importer = self.pick_importer(&input)?;
        let ctx = ImportCtx::new(options).with_resolver(&NoResolver).with_progress(progress);
        let outcome = run_import(&*importer, &input, &ctx).map_err(import_error)?;
        // The last chance to stop: past this point the objects are written without further checks.
        progress.checkpoint("storing", 0.9).map_err(|e| from_io(e, "path"))?;
        let provenance = outcome
            .scene
            .provenance
            .clone()
            .ok_or_else(|| OpError::internal("run_import left the scene without a provenance record"))?;
        let mut objects = vec![(ContentHash::of(bytes), bytes, provenance.clone())];
        for (ix, image) in outcome.scene.images.iter().enumerate() {
            let record = Provenance {
                source: Source::Imported { file: format!("{name}#image{ix}"), origin: None },
                ..provenance.clone()
            };
            objects.push((ContentHash::of(image.bytes.as_slice()), image.bytes.as_slice(), record));
        }
        // Checked for every object before the first write: `put` keeps the record an object was first stored with, so
        // an image that is already there under wider rights would carry more than this model grants.
        for (hash, _, record) in &objects {
            refuse_wider_rights(store, hash, record, name)?;
        }
        let mut images = Vec::new();
        for (hash, bytes, record) in objects {
            let stored = store
                .put(bytes, record)
                .map_err(|e: StoreError| OpError::internal(format!("storing {name}: {e}")))?;
            debug_assert_eq!(stored, hash, "the store names an object by the hash of its bytes");
            images.push(stored);
        }
        let original = images.remove(0);
        let mut seen = Vec::new();
        images.retain(|hash| {
            let first = !seen.contains(hash);
            seen.push(*hash);
            first
        });
        progress.stage("done", 1.0);
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

    /// The file's name: `name`, else the last part of `path`, else [`DEFAULT_NAME`].
    fn file_name(&self) -> String {
        let from_path = self.path.as_deref().and_then(|p| std::path::Path::new(p).file_name()?.to_str());
        self.name.clone().or_else(|| from_path.map(str::to_owned)).unwrap_or_else(|| DEFAULT_NAME.to_owned())
    }

    /// The read: `path` through the job's file access, refusing a file over the input limit before reading it.
    fn read_source(&self, job: &JobCtx<'_>, max_bytes: u64) -> Result<Cow<'_, [u8]>, OpError> {
        let Some(path) = &self.path else {
            let bytes = self.bytes.as_deref();
            return bytes.map(Cow::Borrowed).ok_or_else(|| OpError::internal("plan accepted an import with no source"));
        };
        let file = job.fs_access(path, FsMode::Read)?.open_read()?;
        let size = file.metadata().map_err(|e| OpError::invalid("path", format!("couldn't read the file: {e}")))?.len();
        check_size(size, max_bytes)?;
        // The read still stops at the limit: the file may grow after the size was taken.
        let mut bytes = Vec::new();
        file.take(max_bytes.saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|e| OpError::invalid("path", format!("couldn't read the file: {e}")))?;
        check_size(bytes.len() as u64, max_bytes)?;
        Ok(Cow::Owned(bytes))
    }
}

/// The op error for a failed import. The job reads no other files, so a file the model refers to is a feature this op
/// lacks, not a missing file.
fn import_error(error: IoError) -> OpError {
    match error {
        IoError::Resource { uri, .. } => OpError::unsupported(format!(
            "the model refers to another file ({uri}), and io.import reads a single file"
        ))
        .hint("import a .glb, which carries its buffers and images"),
        other => from_io(other, "path"),
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

/// How much a record lets people do with an object: higher grants more.
fn permissiveness(record: &Provenance) -> (u8, bool, bool) {
    let commercial = match record.commercial {
        Commercial::No => 0,
        Commercial::Unknown => 1,
        Commercial::Ok => 2,
    };
    // Private test material is the stricter record; a missing credit line asks less of the user than a required one.
    (commercial, !record.private_test, record.attribution.is_none())
}

/// Refuses an import whose object is already stored under a record that grants more than `wanted`. `Store::put` keeps the
/// first record and the store offers no way to join two, so the import fails closed; an equal or stricter stored record
/// is fine.
fn refuse_wider_rights(store: &dyn Store, hash: &ContentHash, wanted: &Provenance, name: &str) -> Result<(), OpError> {
    if !store.contains(hash) {
        return Ok(());
    }
    let existing = store
        .provenance(hash)
        .map_err(|e| OpError::internal(format!("reading the record of {hash}: {e}")))?;
    let (have, want) = (permissiveness(&existing), permissiveness(wanted));
    if have.0 > want.0 || (have.1 && !want.1) || (have.2 && !want.2) {
        return Err(OpError::conflict(format!(
            "{name} holds an object ({hash}) that the store already has under wider rights: stored as {} ({:?}), \
             but this model is {} ({:?})",
            existing.licence, existing.commercial, wanted.licence, wanted.commercial,
        ))
        .hint("import the models in the order of their rights, or record the shared object's rights by hand first"));
    }
    Ok(())
}

/// `Invalid` over the input limit, the same error `run_import` gives, so a hostile file is refused before it is held.
fn check_size(got: u64, max_bytes: u64) -> Result<(), OpError> {
    if got > max_bytes {
        return Err(from_io(IoError::Limit { what: "input size in bytes", limit: max_bytes, got }, "path"));
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

/// The patch that records `asset`: a new entry under `assets/<original>`, creating `assets` when the document has none
/// (a change needs its parent to exist). Importing the same file again with nothing new to say is an empty patch.
fn record_patch(doc: &dyn DocRead, asset: &AssetRecord) -> Result<Patch, OpError> {
    let value = serde_json::to_value(asset).map_err(|e| OpError::internal(format!("serialising the asset: {e}")))?;
    let table = DocPath::root().join(ASSETS);
    let entry = table.join(asset.original.to_string());
    let patch = match doc.get(&table) {
        None => Patch::new().with(table, None, Some(Value::Object(std::iter::once((asset.original.to_string(), value)).collect()))),
        Some(Value::Object(_)) => Patch::new().with(entry.clone(), doc.get(&entry).cloned(), Some(value)),
        Some(_) => {
            return Err(OpError::conflict(format!("{ASSETS} in the document is not a table"))
                .hint("the document is damaged; restore it from history"));
        }
    };
    Ok(if patch.is_empty() { Patch::new() } else { patch })
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
                check_size(bytes.len() as u64, max_bytes).map_err(|mut e| {
                    e.field = Some("bytes".to_owned());
                    e
                })?;
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
            let bytes = self.read_source(job, options.limits.max_input_bytes)?;
            self.import_bytes(&self.file_name(), &bytes, &options, store, &JobProgress(job))
        });
        // A cancelled import surfaces from the importer as a plain conflict; the job reports it as cancelled.
        let imported = outcome.map_err(|e| if job.is_cancelled() { OpError::cancelled(job.id()) } else { e })?;
        let mut result = serde_json::to_value(imported)
            .map_err(|e| OpError::internal(format!("serialising the import: {e}")))?;
        strip_nulls(&mut result);
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
