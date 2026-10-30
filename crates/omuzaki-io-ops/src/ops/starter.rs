//! The starter characters: `io.starter.list` and `io.starter.fetch`.
//!
//! A starter is a finished, rigged VRM character whose licence lets anyone perform as it, edit it and export it, so a
//! person (or an agent) can begin from something that already looks right instead of an empty project. The catalogue
//! is data (`data/starter-characters.toml`); each licence was read at its primary source, and the files themselves
//! stay out of the repository (ADR 0003): `io.starter.fetch` downloads one on first use through the app's verified
//! cache ([`FileSource`], pinned by size and SHA-256), takes the model out of its archive when it comes in one, checks
//! the model against its own pin, and puts it in the asset store under the record the catalogue gives.
//!
//! Becoming a character is then the ordinary avatar import of a stored asset, so the person still sees the report
//! and confirms it: `io.inspect {asset, as: avatar}`, `io.import {asset, as: avatar, confirm}`. The import reads the
//! file's own `meta` for the avatar's terms, and a file's meta can say less than its licence (pixiv's CC0 samples
//! carry a `VRoid` Hub `Other` licence that reads as "no modification, no redistribution"), so the last step,
//! `licence.set_source` on the avatar, records the terms the licence grants. The fetch's result spells all three out.

use std::io::Read;
use std::sync::OnceLock;

use omuzaki_ai_api::{FileSource, PinnedFile, Progress as FetchProgress, Sha256};
use omuzaki_io_api::UseTerms;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Digest as _;

use super::avatar::import::AVATAR_PART;
use super::licence::source::PartSource;
use super::progress::JobProgress;

/// The catalogue, as shipped.
const CATALOGUE: &str = include_str!("../../data/starter-characters.toml");

/// Bytes read from an archive member between cancellation checks.
const CHUNK: usize = 1 << 20;

/// One starter character, as the catalogue lists it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Starter {
    /// What `io.starter.fetch` takes.
    pub id: String,
    /// The character's name, as its makers give it.
    pub name: String,
    /// One line on how it looks.
    pub about: String,
    /// SPDX licence or `LicenseRef-…`.
    pub licence: String,
    /// Who made it, as the licence names them.
    pub author: String,
    /// The credit line, when the licence requires one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    /// Where the licence was read.
    pub source_url: String,
    /// What the licence grants, in the VRM `meta` vocabulary.
    pub terms: UseTerms,
    /// The pinned download.
    pub download: Download,
}

/// Where a starter's file comes from, pinned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Download {
    /// `https://` URLs of the same bytes, tried in order.
    pub urls: Vec<String>,
    /// The SHA-256 of the downloaded file.
    pub sha256: Sha256,
    /// Its length in bytes.
    pub size: u64,
    /// For a zip archive: the model's name inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member: Option<String>,
    /// For a zip archive: the SHA-256 of the model inside it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_sha256: Option<Sha256>,
    /// For a zip archive: the model's length in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub member_size: Option<u64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogueFile {
    starter: Vec<Starter>,
}

/// Parses a catalogue and checks every entry: ids unique and well formed, `https://` URLs, an archive's member fully
/// pinned, and a licence that allows commercial use, modification and redistribution.
///
/// # Errors
/// What is wrong, naming the entry.
pub fn parse_catalogue(text: &str) -> Result<Vec<Starter>, String> {
    let file: CatalogueFile = toml::from_str(text).map_err(|e| format!("the starter catalogue does not parse: {e}"))?;
    let mut seen = std::collections::BTreeSet::new();
    for starter in &file.starter {
        let id = &starter.id;
        let well_formed = !id.is_empty()
            && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !id.starts_with('-');
        if !well_formed {
            return Err(format!("starter id `{id}`: use lowercase letters, digits and `-`"));
        }
        if !seen.insert(id.clone()) {
            return Err(format!("starter id `{id}` is listed twice"));
        }
        let d = &starter.download;
        if d.urls.is_empty() || d.urls.iter().any(|u| !u.starts_with("https://")) {
            return Err(format!("starter `{id}`: every download URL must be https"));
        }
        if d.member.is_some() != d.member_sha256.is_some() || d.member.is_some() != d.member_size.is_some() {
            return Err(format!("starter `{id}`: an archive member needs `member`, `member_sha256` and `member_size`"));
        }
        let t = &starter.terms;
        let open = t.allow_redistribution
            && t.modification == omuzaki_io_api::Modification::AllowModificationRedistribution
            && t.commercial_usage == omuzaki_io_api::CommercialUsage::Corporation
            && t.avatar_permission == omuzaki_io_api::AvatarPermission::Everyone;
        if !open {
            return Err(format!(
                "starter `{id}`: a starter's licence must allow everyone to perform as it, corporate commercial use, \
                 modification and redistribution"
            ));
        }
        let credit = t.credit_notation == omuzaki_io_api::CreditNotation::Required;
        if credit != starter.attribution.is_some() {
            return Err(format!("starter `{id}`: give `attribution` exactly when the terms require credit"));
        }
    }
    Ok(file.starter)
}

/// The shipped catalogue, parsed once.
///
/// # Errors
/// `Internal` if the shipped file is malformed (a test holds it well formed).
pub fn catalogue() -> Result<&'static [Starter], OpError> {
    static PARSED: OnceLock<Result<Vec<Starter>, String>> = OnceLock::new();
    PARSED.get_or_init(|| parse_catalogue(CATALOGUE)).as_deref().map_err(|e| OpError::internal(e.clone()))
}

impl Starter {
    /// The model's file name: the archive member's, or the last segment of the first URL.
    pub fn file_name(&self) -> &str {
        match &self.download.member {
            Some(member) => member,
            None => self.download.urls.first().and_then(|u| u.rsplit('/').next()).unwrap_or(&self.id),
        }
    }

    /// The record the model is stored under: imported from its file, with the catalogue's licence, credit and origin,
    /// and commercial use cleared (the catalogue holds only such licences).
    pub fn provenance(&self) -> Provenance {
        Provenance {
            source: Source::Imported { file: self.file_name().to_owned(), origin: Some(self.source_url.clone()) },
            author: Some(self.author.clone()),
            licence: self.licence.clone(),
            attribution: self.attribution.clone(),
            commercial: Commercial::Ok,
            consent: None,
            private_test: false,
        }
    }

    /// The avatar's source row the licence grants: [`Starter::provenance`] with the catalogue's terms.
    pub fn source_row(&self) -> PartSource {
        let mut row = PartSource::new(self.provenance());
        row.terms = Some(self.terms.clone());
        row
    }

    /// The pin of the file to download.
    pub fn pin(&self) -> PinnedFile {
        PinnedFile::new(self.download.urls.iter().cloned(), self.download.sha256, self.download.size)
    }
}

/// Lists the starter characters: finished VRM characters that anyone may perform as, edit and export.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct StarterList {}

/// One row of `io.starter.list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StarterRow {
    /// What `io.starter.fetch` takes.
    pub id: String,
    /// The character's name.
    pub name: String,
    /// How it looks.
    pub about: String,
    /// Its licence (SPDX or `LicenseRef-…`).
    pub licence: String,
    /// The credit line to show with it, when its licence requires one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
    /// Who made it.
    pub author: String,
    /// The download, in bytes.
    pub download_bytes: u64,
}

impl Op for StarterList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "io.starter.list",
            summary: "List the starter characters: finished, rigged VRM characters that anyone may perform as, edit and \
                      export, each with its licence and credit. Fetch one with io.starter.fetch.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let rows: Vec<StarterRow> = catalogue()?
            .iter()
            .map(|s| StarterRow {
                id: s.id.clone(),
                name: s.name.clone(),
                about: s.about.clone(),
                licence: s.licence.clone(),
                credit: s.attribution.clone(),
                author: s.author.clone(),
                download_bytes: s.download.size,
            })
            .collect();
        Ok(Plan::answer(json!({ "starters": rows })))
    }
}

register_op!(StarterList);

/// Downloads a starter character (once; later calls use the cache), checks it against its pin and stores it as an
/// asset under its licence record. The result names the asset and the three steps that make it a character:
/// `io.inspect {asset, as: avatar}`, `io.import {asset, as: avatar, confirm}` and `licence.set_source` with the row it
/// gives. The project's document does not change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "id": "vroid-vita" }, { "id": "seed-san", "character": "seed" }]))]
pub struct StarterFetch {
    /// The starter, as `io.starter.list` names it.
    pub id: String,
    /// The id the character will have, used in the steps the result gives; the starter's id with `-` as `_` when
    /// left out.
    #[serde(default)]
    pub character: Option<String>,
}

/// What `io.starter.fetch` returns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Fetched {
    /// The starter.
    pub id: String,
    /// The character's name.
    pub name: String,
    /// The stored model, for `io.inspect` and `io.import`.
    pub asset: ContentHash,
    /// The model's file name.
    pub file: String,
    /// The record the store holds for it. The store keeps the first record it was given, so a model that was already
    /// stored from a file of the person's keeps that file's record; `record_kept` says so.
    pub provenance: Provenance,
    /// True when the store already held the model under another record.
    pub record_kept: bool,
    /// The credit line to show with it, when its licence requires one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credit: Option<String>,
    /// The steps that make it a character, in order, each an op and its arguments. `io.import` needs the `confirm`
    /// token `io.inspect` returns.
    pub next: Vec<Value>,
}

impl FetchProgress for JobProgress<'_, '_> {
    fn stage(&self, name: &str, fraction: f32) {
        self.0.progress(name, fraction);
    }

    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

fn find(id: &str) -> Result<&'static Starter, OpError> {
    let all = catalogue()?;
    all.iter().find(|s| s.id == id).ok_or_else(|| {
        let ids: Vec<&str> = all.iter().map(|s| s.id.as_str()).collect();
        let mut error = OpError::not_found(format!("no starter character `{id}`"));
        error.field = Some("id".to_owned());
        error.hint(format!("the starters are: {}", ids.join(", ")))
    })
}

fn sha256(bytes: &[u8]) -> Sha256 {
    Sha256(sha2::Sha256::digest(bytes).into())
}

/// The model's bytes from the downloaded file: the file itself, or its pinned archive member, checked against its pin.
///
/// # Errors
/// `Invalid` naming `id` when the archive or the member is not what the catalogue pins; `Internal` on a read failure;
/// `Cancelled` through `progress`.
#[allow(clippy::disallowed_methods, reason = "the file is the verified cache's, not one the person named")]
pub fn model_bytes(
    starter: &Starter,
    path: &std::path::Path,
    progress: &dyn FetchProgress,
) -> Result<Vec<u8>, OpError> {
    let read_error = |e: &dyn std::fmt::Display| OpError::internal(format!("reading the downloaded starter: {e}"));
    let d = &starter.download;
    let (Some(member), Some(want), Some(size)) = (&d.member, d.member_sha256, d.member_size) else {
        return std::fs::read(path).map_err(|e| read_error(&e));
    };
    let file = std::fs::File::open(path).map_err(|e| read_error(&e))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| read_error(&e))?;
    let mut entry = archive.by_name(member).map_err(|e| {
        OpError::invalid("id", format!("the archive of `{}` has no `{member}`: {e}", starter.id))
            .hint("the download no longer matches the catalogue: report it")
    })?;
    if entry.size() != size {
        return Err(OpError::invalid(
            "id",
            format!("`{member}` is {} bytes, the catalogue pins {size}", entry.size()),
        ));
    }
    let capacity = usize::try_from(size).map_err(|_| OpError::internal("the model is too large for this machine"))?;
    let mut bytes = Vec::with_capacity(capacity);
    let mut chunk = vec![0; CHUNK];
    loop {
        if progress.is_cancelled() {
            return Err(OpError::conflict("cancelled while unpacking the starter"));
        }
        let n = entry.read(&mut chunk).map_err(|e| read_error(&e))?;
        if n == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..n]);
        if bytes.len() as u64 > size {
            return Err(OpError::invalid("id", format!("`{member}` is longer than the pinned {size} bytes")));
        }
        #[allow(clippy::cast_precision_loss, reason = "a progress fraction")]
        progress.stage("unpacking", bytes.len() as f32 / size.max(1) as f32);
    }
    let got = sha256(&bytes);
    if got != want {
        return Err(OpError::invalid("id", format!("`{member}` has SHA-256 {got}, the catalogue pins {want}"))
            .hint("the download no longer matches the catalogue: report it"));
    }
    Ok(bytes)
}

/// Downloads `starter` through `files`, takes its model out and stores it in `store` under its record.
///
/// # Errors
/// `Unavailable` (with the reason) when the download fails, as [`model_bytes`], and `Internal` for a store failure.
pub fn fetch(
    starter: &Starter,
    character: &str,
    files: &dyn FileSource,
    store: &dyn Store,
    progress: &dyn FetchProgress,
) -> Result<Fetched, OpError> {
    let path = files.get(&starter.pin(), progress).map_err(|e| {
        OpError::unavailable("the starter download")
            .hint(format!("{e}; check the connection and try again, or import a VRM file of your own"))
    })?;
    let bytes = model_bytes(starter, &path, progress)?;
    let wanted = starter.provenance();
    let asset = store.put(&bytes, wanted.clone()).map_err(|e| OpError::internal(format!("storing the starter: {e}")))?;
    let provenance = store.provenance(&asset).map_err(|e| OpError::internal(format!("reading its record: {e}")))?;
    let record_kept = provenance != wanted;
    let row = serde_json::to_value(starter.source_row())
        .map_err(|e| OpError::internal(format!("serialising the source row: {e}")))?;
    let next = vec![
        json!({ "op": "io.inspect", "args": { "asset": asset, "as": "avatar" } }),
        json!({ "op": "io.import", "args": {
            "asset": asset, "as": "avatar", "id": character,
            "confirm": "<the confirm token io.inspect returned>"
        } }),
        json!({ "op": "licence.set_source", "args": { "character": character, "part": AVATAR_PART, "source": row } }),
    ];
    Ok(Fetched {
        id: starter.id.clone(),
        name: starter.name.clone(),
        asset,
        file: starter.file_name().to_owned(),
        provenance,
        record_kept,
        credit: starter.attribution.clone(),
        next,
    })
}

impl StarterFetch {
    fn character(&self) -> String {
        self.character.clone().unwrap_or_else(|| self.id.replace('-', "_"))
    }
}

impl Op for StarterFetch {
    fn meta() -> OpMeta {
        OpMeta {
            name: "io.starter.fetch",
            summary: "Download a starter character (io.starter.list), check it against its pin and store it as an asset \
                      under its licence record; returns the asset and the io.inspect, io.import and \
                      licence.set_source steps that make it a character. The document does not change.",
            kind: OpKind::Job,
            scope: Scope::EditCharacter,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let starter = find(&self.id)?;
        if let Err(e) = omuzaki_char_api::CharId::new(self.character()) {
            return Err(OpError::invalid("character", e.to_string()).hint("pass a character id: lowercase, `_`"));
        }
        // The store and the download service are `work`'s; a dry run only says what would be fetched.
        Ok(Plan::answer(json!({ "id": starter.id, "download_bytes": starter.download.size })))
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let starter = find(&self.id)?;
        let store = job.service::<dyn Store>()?;
        let files = job.service::<dyn FileSource>().map_err(|e| {
            e.hint("downloads need a host with the verified model cache (the studio); or import a VRM file of your own")
        })?;
        job.checkpoint("downloading", 0.0)?;
        let fetched = fetch(starter, &self.character(), files, store, &JobProgress(job))
            .map_err(|e| if job.is_cancelled() { OpError::cancelled(job.id()) } else { e })?;
        let value = serde_json::to_value(fetched).map_err(|e| OpError::internal(format!("serialising: {e}")))?;
        job.checkpoint("done", 1.0)?;
        Ok(value)
    }
}

register_op!(StarterFetch);
