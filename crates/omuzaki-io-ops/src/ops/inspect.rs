//! `io.inspect`: what is in this model file, without importing it.
//!
//! The op reads one model, from a file (`path`) or from the asset store (`asset`), asks the registered importers which
//! format it is, imports it **in memory** with [`run_import`] (so the limits, scene invariants and rights notes of a
//! real import all apply) and returns the [`SceneSummary`] and the findings [`Report`]. Nothing is stored and no
//! document changes: it is the dry run of `io.import`, and the first thing an agent should call on a file it was handed.
//!
//! **Access.** The op's scope is `read`. A `path` goes through [`OpCtx::fs_access`]: inside the project folder `read`
//! alone is enough; anywhere else the actor needs `fs` and a root that covers the path (ops-api.md §5). The file is
//! opened through the returned [`CanonicalPath`](omuzaki_ops_api::CanonicalPath), so a link swapped in after the check
//! cannot redirect the read. An `asset` is read from the store service and needs no file access at all.
//!
//! **Sidecars.** The files a `.gltf` refers to are read through a [`DirResolver`], confined to the folder of the file
//! the person named, so a `path` read resolves them. An `asset` has no folder, so its sidecars are refused with
//! [`NoResolver`].
//!
//! **As an avatar** (`as: avatar`, `docs/design/byom.md` D6): the whole preparation runs in memory as well
//! ([`prepare::avatar::to_avatar`](crate::prepare::avatar::to_avatar)), and the answer adds what the confirm screen
//! shows: the [`AvatarSummary`] (the id it suggests, the channels the model shows, the time each stage took, its
//! texture memory), the one report with every step's findings and the file's rights rows, and the `confirm` token
//! `io.import {as: avatar}` must be given. Still nothing is stored and no document changes.

use std::io::Read;
use std::path::Path;

use omuzaki_io_api::{
    Detection, FormatId, ImportCtx, ImportInput, ImportOptions, Importer, IoError, NoResolver, Report, Resolver,
    SceneSummary, find_importer, formats, run_import, sniff,
};
use omuzaki_ops_api::{FsMode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::{ContentHash, Store, StoreError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::from_io;
use super::avatar::{self, Source};
use super::sidecar::DirResolver;
use omuzaki_avatar_api::summary::AvatarSummary;
use omuzaki_io_api::NoProgress;
use omuzaki_render_api::Capabilities;

/// Reads a model file and reports what is in it, without importing it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Inspect {
    /// The model file to read. Paths outside the project folder need the `fs` grant and a root covering them.
    /// Pass exactly one of `path` and `asset`.
    #[serde(default)]
    pub path: Option<String>,
    /// The stored asset to read, by content hash (64 hex characters). Pass exactly one of `path` and `asset`.
    #[serde(default)]
    pub asset: Option<ContentHash>,
    /// The format to read it as (see `io.list_formats`). Detected from the file's contents when omitted.
    #[serde(default)]
    pub format: Option<FormatId>,
    /// Treat warnings as errors, as a strict import would.
    #[serde(default)]
    pub strict: bool,
    /// What to read the file as: `scene` reports what is in it; `avatar` also prepares it as an avatar, in memory, and
    /// returns the summary, the whole report and the `confirm` token that `io.import` with `as: avatar` needs.
    #[serde(default, rename = "as")]
    pub as_: InspectAs,
}

/// What `io.inspect` reads a file as.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InspectAs {
    /// A model file: what is in it.
    #[default]
    Scene,
    /// An avatar to perform as: the scene, plus the prepared avatar's summary, report and confirm token.
    Avatar,
}

/// What `io.inspect {as: avatar}` adds: what the confirm screen shows, and the token that confirms it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AvatarInspection {
    /// The prepared avatar at a glance.
    pub summary: AvatarSummary,
    /// The token `io.import {as: avatar}` must be given, so the import is the one the person saw.
    pub confirm: String,
}

/// What `io.inspect` returns.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Inspection {
    /// Where the bytes came from: `"path"` or `"asset"`.
    pub source: &'static str,
    /// The file's name, without directories; for an asset, its hash.
    pub file: String,
    /// Its size in bytes.
    pub bytes: u64,
    /// The format it was read as.
    pub format: String,
    /// Every importer that recognised the file, most sure first.
    pub detected: Vec<Detection>,
    /// Counts of what the file holds.
    pub summary: SceneSummary,
    /// What was noticed on the way: warnings, dropped extensions, the rights notes; as an avatar, also every finding
    /// of the preparation and the rights rows of performing as it.
    pub report: Report,
    /// The prepared avatar, when inspected `as: avatar`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<AvatarInspection>,
}

impl Inspect {
    /// Inspects `bytes` as if they were the file called `name`. The file-reading half of the op calls this; it is
    /// separate so the sniff, import and summary can be driven without a disk.
    ///
    /// # Errors
    /// `Unsupported` when no importer recognises the bytes; `Invalid` for a `format` nobody registered, and for files
    /// that are malformed, over the import limits, or rejected by the scene checks.
    pub fn inspect_bytes(&self, name: &str, bytes: &[u8]) -> Result<Inspection, OpError> {
        self.inspect_bytes_with(name, bytes, &NoResolver)
    }

    /// [`inspect_bytes`](Self::inspect_bytes) with a `resolver` for the files the model refers to (a `.gltf`'s buffers
    /// and images).
    ///
    /// # Errors
    /// As `inspect_bytes`; a reference the resolver can't read is `NotFound`.
    pub fn inspect_bytes_with(&self, name: &str, bytes: &[u8], resolver: &dyn Resolver) -> Result<Inspection, OpError> {
        self.inspect_source("path", name, bytes, resolver)
    }

    /// The shared body: `field` is the argument the bytes came from (`path` or `asset`), which import errors blame.
    fn inspect_source(
        &self,
        field: &str,
        name: &str,
        bytes: &[u8],
        resolver: &dyn Resolver,
    ) -> Result<Inspection, OpError> {
        let input = ImportInput::new(bytes).named(name);
        let detected = sniff(&input);
        let importer = self.pick_importer(&detected)?;
        let options = if self.strict { ImportOptions::default().strict() } else { ImportOptions::default() };
        let ctx = ImportCtx::new(&options).with_resolver(resolver);
        let outcome = run_import(&*importer, &input, &ctx).map_err(|e| from_io(e, field))?;
        Ok(Inspection {
            source: "path",
            file: name.to_owned(),
            bytes: bytes.len() as u64,
            format: importer.info().id.to_owned(),
            detected,
            summary: outcome.scene.summary(),
            report: outcome.report,
            avatar: None,
        })
    }

    /// Inspects `bytes`, read from the argument `field` as the file `name`, as an avatar (see the module docs).
    fn inspect_avatar(
        &self,
        ctx: &OpCtx<'_>,
        field: &'static str,
        name: &str,
        bytes: &[u8],
        resolver: &dyn Resolver,
    ) -> Result<Inspection, OpError> {
        let options = if self.strict { ImportOptions::default().strict() } else { ImportOptions::default() };
        let source =
            Source { name, bytes, format: self.format.as_ref(), resolver, progress: &NoProgress, field };
        let mtoon = avatar::mtoon(ctx.service::<Capabilities>().ok());
        let prepared = avatar::prepare(&source, None, &options, mtoon, ctx.clock, ctx.doc)?;
        Ok(Inspection {
            source: field,
            file: name.to_owned(),
            bytes: bytes.len() as u64,
            format: prepared.format,
            detected: sniff(&ImportInput::new(bytes).named(name)),
            summary: prepared.scene.summary(),
            report: prepared.report,
            avatar: Some(AvatarInspection { summary: prepared.summary, confirm: prepared.confirm }),
        })
    }

    /// The importer to use: the one named by `format`, otherwise the best of `detected`.
    fn pick_importer(&self, detected: &[Detection]) -> Result<Box<dyn Importer>, OpError> {
        if let Some(format) = &self.format {
            return find_importer(format.as_str()).ok_or_else(|| {
                OpError::invalid("format", format!("no importer is registered for format {format}"))
                    .hint(importable_formats())
            });
        }
        let best = detected.first().ok_or_else(|| {
            OpError::unsupported("no registered importer recognises this file")
                .hint(format!("{}; if it is one of them it may be damaged or truncated", importable_formats()))
        })?;
        find_importer(best.format)
            .ok_or_else(|| OpError::internal(format!("importer {} vanished after sniffing", best.format)))
    }
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

/// Reads `file` whole, refusing anything over `max_bytes`. The read stops one byte past the limit, so a huge or
/// endless file is never held in memory.
fn read_limited(file: impl Read, max_bytes: u64) -> Result<Vec<u8>, OpError> {
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| OpError::invalid("path", format!("couldn't read the file: {e}")))?;
    check_size(bytes.len() as u64, max_bytes, "path")?;
    Ok(bytes)
}

fn check_size(got: u64, max_bytes: u64, field: &str) -> Result<(), OpError> {
    if got > max_bytes {
        return Err(from_io(IoError::Limit { what: "input size in bytes", limit: max_bytes, got }, field));
    }
    Ok(())
}

/// The bytes of the stored asset `hash`. The store has no size query, so the limit
/// (`ImportOptions::default().limits.max_input_bytes`) is checked after the read.
fn read_asset(ctx: &OpCtx<'_>, hash: &ContentHash, max_bytes: u64) -> Result<Vec<u8>, OpError> {
    let store = ctx.service::<dyn Store>()?;
    let bytes = store.get(hash).map_err(|e| match e {
        StoreError::Missing(_) => {
            let mut error = OpError::not_found(format!("no asset {hash} in the store"))
                .hint("pass the hash of an asset that was added to the project");
            error.field = Some("asset".to_owned());
            error
        }
        other => OpError::internal(format!("reading asset {hash} failed: {other}")),
    })?;
    check_size(bytes.len() as u64, max_bytes, "asset")?;
    Ok(bytes)
}

impl Op for Inspect {
    fn meta() -> OpMeta {
        OpMeta {
            name: "io.inspect",
            summary: "Read a model file (glTF, VRM) or a stored asset and report what is in it, without importing it; \
                      with `as: avatar`, also prepare it as an avatar and return the summary, report and confirm token.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let max_bytes = ImportOptions::default().limits.max_input_bytes;
        let inspection = match (&self.path, &self.asset) {
            (Some(path), None) => {
                if path.is_empty() {
                    return Err(OpError::invalid("path", "the path is empty").hint("pass the path of a model file"));
                }
                let file = ctx.fs_access(path, FsMode::Read)?;
                let bytes = read_limited(file.open_read()?, max_bytes)?;
                if bytes.is_empty() {
                    return Err(OpError::invalid("path", format!("{path} is empty")));
                }
                let name = Path::new(path).file_name().and_then(|name| name.to_str()).unwrap_or(path);
                let sidecars = DirResolver::new(ctx, &file, path, max_bytes);
                match self.as_ {
                    InspectAs::Scene => self.inspect_bytes_with(name, &bytes, &sidecars)?,
                    InspectAs::Avatar => self.inspect_avatar(ctx, "path", name, &bytes, &sidecars)?,
                }
            }
            (None, Some(hash)) => {
                let bytes = read_asset(ctx, hash, max_bytes)?;
                let name = hash.to_string();
                match self.as_ {
                    InspectAs::Scene => {
                        Inspection { source: "asset", ..self.inspect_source("asset", &name, &bytes, &NoResolver)? }
                    }
                    InspectAs::Avatar => self.inspect_avatar(ctx, "asset", &name, &bytes, &NoResolver)?,
                }
            }
            _ => {
                return Err(OpError::invalid("path", "pass exactly one of path or asset")
                    .hint("path reads a file; asset reads a stored asset by its hash"));
            }
        };
        let output = serde_json::to_value(inspection)
            .map_err(|e| OpError::internal(format!("serialising the inspection failed: {e}")))?;
        Ok(Plan::answer(output))
    }
}

register_op!(Inspect);

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::ErrorCode;

    use super::*;

    #[test]
    fn input_over_the_limit_is_refused_without_reading_it_all() {
        let error = read_limited(&[0u8; 100][..], 99).unwrap_err();
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("path")));
        assert!(error.message.contains("input size in bytes is over the limit: 100 > 99"), "{}", error.message);
        assert_eq!(read_limited(&[0u8; 100][..], 100).unwrap().len(), 100, "input exactly at the limit is fine");
        let endless = std::io::repeat(0);
        assert!(read_limited(endless, 10).is_err(), "an endless reader stops at the limit");
    }
}
