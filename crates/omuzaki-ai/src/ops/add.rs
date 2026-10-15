//! `model.add`: add a model file the person keeps, as a job (it copies, probes and smoke-tests in a sandboxed worker).

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use omuzaki_ai_api::{AddModel, DISCLAIMER, ModelManifest, ModelRegistry, Progress, RegistryError};
use omuzaki_ops_api::{Actor, FsMode, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::list::Listed;

/// Bytes copied per read while a picked file is brought in, between cancellation checks.
const CHUNK: usize = 1 << 20;

/// Adds a model file the person keeps (`.onnx`, a whisper `.bin`, ...) to a slot. The file is copied into the model
/// cache, so moving or editing it later changes nothing; it is then checked in a sandboxed worker: its format, the
/// slot's interface, and the slot's smoke test. A model that fails is listed as disabled with the reason. One that
/// passes waits for the person to accept its licence notice (`model.acknowledge`), unless the person accepted it here
/// with `accept_notice`, which only a person can do. The path must lie in a folder the person shared (`fs`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelAdd {
    /// The slot it is for, as `id@version`.
    pub slot: String,
    /// The model's main file.
    pub path: String,
    /// Its other files, for a model of several.
    #[serde(default)]
    pub more: Vec<String>,
    /// A `model.toml` that came with it; without one the app writes one (licence unknown).
    #[serde(default)]
    pub manifest: Option<String>,
    /// The person has read the licence notice and accepts it now. Only a person may set this.
    #[serde(default)]
    pub accept_notice: bool,
}

impl ModelAdd {
    /// Every file the op reads, as given.
    fn paths(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.path.as_str()).chain(self.more.iter().map(String::as_str)).chain(self.manifest.as_deref())
    }
}

/// [`Progress`] over a job: its stages reach the job's signal, and its cancellation stops the registry's work.
struct JobProgress<'a>(&'a JobCtx<'a>);

impl Progress for JobProgress<'_> {
    fn stage(&self, name: &str, fraction: f32) {
        self.0.progress(name, fraction);
    }

    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

impl Op for ModelAdd {
    fn meta() -> OpMeta {
        OpMeta {
            name: "model.add",
            summary: "Add a model file you keep to a slot: it is copied, checked in a sandbox and smoke-tested.",
            kind: OpKind::Job,
            scope: Scope::EditLibrary,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.accept_notice && !matches!(ctx.actor, Actor::Person { .. }) {
            return Err(OpError::person_only("accepting a model's licence notice"));
        }
        let registry = ctx.service::<dyn ModelRegistry>()?;
        super::slot("slot", &self.slot, registry)?;
        // Fails now, not after the job is queued, when the actor may not read a file.
        for path in self.paths() {
            ctx.fs_access(path, FsMode::Read)?;
        }
        // The model's files are known by their names, so two of the same name can't both be its files.
        let mut names: Vec<_> =
            std::iter::once(&self.path).chain(&self.more).map(|p| Path::new(p).file_name()).collect();
        names.sort();
        if names.windows(2).any(|w| w[0] == w[1]) {
            return Err(OpError::invalid("more", "two of the model's files have the same name")
                .hint("a model's files are known by their names: rename one"));
        }
        Ok(Plan::default())
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let registry = job.service::<dyn ModelRegistry>()?;
        let slot = super::slot("slot", &self.slot, registry)?;
        let acknowledged_by = match (self.accept_notice, job.actor()) {
            (false, _) => None,
            (true, Actor::Person { name }) => Some(name.clone()),
            (true, _) => return Err(OpError::person_only("accepting a model's licence notice")),
        };
        // The registry copies files by path; a checked path is opened only through its handle, so each file is
        // brought into a private folder first. The folder goes when the job ends.
        let staging = tempfile::tempdir().map_err(|e| OpError::internal(format!("a staging folder: {e}")))?;
        let mut files = Vec::with_capacity(1 + self.more.len());
        for path in std::iter::once(&self.path).chain(&self.more) {
            files.push(bring(job, path, staging.path())?);
        }
        let manifest = self.manifest.as_deref().map(|path| read_manifest(job, path)).transpose()?;
        let request = AddModel { slot, files, manifest, acknowledged_by };
        let added = registry.add(request, &JobProgress(job)).map_err(|e| match e {
            RegistryError::Cancelled => OpError::cancelled(job.id()),
            other => super::error(&other),
        })?;
        let mut answer = serde_json::to_value(Listed {
            key: super::full_name(&added.model),
            warning: added.imitates.as_ref().map(|_| crate::IMPERSONATION_WARNING.to_owned()),
            entry: added,
        })
        .map_err(|e| OpError::internal(format!("writing the added model: {e}")))?;
        if let Value::Object(fields) = &mut answer {
            fields.insert("notice".into(), DISCLAIMER.into());
        }
        job.checkpoint("done", 1.0)?;
        Ok(answer)
    }
}

/// Copies the file the person named at `path` into `staging` under its own name, through the checked handle.
fn bring(job: &JobCtx<'_>, path: &str, staging: &Path) -> Result<PathBuf, OpError> {
    let name =
        Path::new(path).file_name().ok_or_else(|| OpError::invalid("path", format!("{path:?} names no file")))?;
    let mut from = job.fs_access(path, FsMode::Read)?.open_read()?;
    let to_path = staging.join(name);
    let name = name.to_string_lossy();
    let mut to = File::create(&to_path).map_err(|e| OpError::internal(format!("staging \"{name}\": {e}")))?;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if job.is_cancelled() {
            return Err(OpError::cancelled(job.id()));
        }
        let n = from.read(&mut buf).map_err(|e| OpError::invalid("path", format!("reading \"{name}\": {e}")))?;
        if n == 0 {
            return Ok(to_path);
        }
        to.write_all(&buf[..n]).map_err(|e| OpError::internal(format!("staging \"{name}\": {e}")))?;
    }
}

/// Reads and parses the `model.toml` at `path`.
fn read_manifest(job: &JobCtx<'_>, path: &str) -> Result<ModelManifest, OpError> {
    let mut text = String::new();
    job.fs_access(path, FsMode::Read)?
        .open_read()?
        .read_to_string(&mut text)
        .map_err(|e| OpError::invalid("manifest", format!("reading it: {e}")))?;
    ModelManifest::from_toml(&text).map_err(|e| OpError::invalid("manifest", e.to_string()))
}

register_op!(ModelAdd);
