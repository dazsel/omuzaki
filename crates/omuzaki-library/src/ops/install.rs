//! `library.install`: add items to the person's own library.

use std::io::Read as _;
use std::time::{SystemTime, UNIX_EPOCH};

use omuzaki_library_api::{ItemManifest, Origin, PublisherCheck};
use omuzaki_ops_api::{
    Actor, Effect, EntryKind, FsMode, Op, OpCall, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op,
};
use omuzaki_store_api::{ContentHash, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::Acknowledgement;
use crate::common::{failed, host};

/// What the person accepts by installing (design D8), shown once per publisher and item.
const DISCLAIMER: &str = "The publisher, not Omuzaki, declares this item's licence and rights. Check them before you \
                          publish what you make with it.";

/// Adds items to your library, across projects (design D9, D11): a manifest file (`from` naming a `.toml`), a folder
/// of manifests with their binaries in `assets/<hash>` (`from` naming the folder), or one manifest's text
/// (`manifest`). Every manifest is checked and its publisher verified: an unsigned item, or one whose signature fails,
/// installs as an unverified publisher whatever name it gives. A version the same publisher already holds with other
/// content is refused, and `omuzaki/` ids come only with Omuzaki. Installing is a person's decision, so an agent's
/// call asks the person, and the disclaimer is acknowledged once per publisher and item. Undo uninstalls them.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "from": "downloads/ribbon-bow.toml" }, { "from": "downloads/ribbon-pack" }]))]
pub struct LibraryInstall {
    /// A manifest file (`.toml`) or a folder of them, absolute or relative to the project.
    #[serde(default)]
    pub from: Option<String>,
    /// One manifest's text, instead of a file.
    #[serde(default)]
    pub manifest: Option<String>,
    /// Several manifests' text (what undoing an uninstall passes).
    #[serde(default)]
    pub manifests: Vec<String>,
}

pub(crate) fn call(op: &str, args: Value) -> OpCall {
    OpCall { op: op.to_owned(), args }
}

/// The asset files of a folder: each file name (a hash) and its bytes.
type AssetFiles = Vec<(String, Vec<u8>)>;

/// Whether `name` is a manifest file name.
fn is_toml(name: &str) -> bool {
    std::path::Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("toml"))
}

/// One manifest read for installing, with its text.
struct Found {
    text: String,
    manifest: ItemManifest,
}

fn read_text(ctx: &OpCtx<'_>, path: &str) -> Result<String, OpError> {
    let mut text = String::new();
    ctx.fs_access(path, FsMode::Read)?
        .open_read()?
        .read_to_string(&mut text)
        .map_err(|e| OpError::invalid("from", format!("{path}: {e}")))?;
    Ok(text)
}

fn parse(text: String, what: &str) -> Result<Found, OpError> {
    let manifest = ItemManifest::parse(&text).map_err(|e| OpError::invalid("manifest", format!("{what}: {e}")))?;
    Ok(Found { text, manifest })
}

/// The manifests and asset files `from` names: a `.toml` file, or a folder's `*.toml` files and `assets/*`.
fn read_from(ctx: &OpCtx<'_>, from: &str) -> Result<(Vec<Found>, AssetFiles), OpError> {
    let path = from.trim_end_matches(['/', '\\']);
    if is_toml(path) {
        return Ok((vec![parse(read_text(ctx, path)?, path)?], Vec::new()));
    }
    let mut found = Vec::new();
    let mut assets = Vec::new();
    for entry in ctx.fs_access(path, FsMode::Read)?.read_dir()? {
        let at = format!("{path}/{}", entry.name);
        match entry.kind {
            EntryKind::File if is_toml(&entry.name) => found.push(parse(read_text(ctx, &at)?, &at)?),
            EntryKind::Folder if entry.name == "assets" => {
                for asset in ctx.fs_access(&at, FsMode::Read)?.read_dir()? {
                    if asset.kind == EntryKind::File {
                        let mut bytes = Vec::new();
                        ctx.fs_access(&format!("{at}/{}", asset.name), FsMode::Read)?
                            .open_read()?
                            .read_to_end(&mut bytes)
                            .map_err(|e| OpError::invalid("from", format!("{at}/{}: {e}", asset.name)))?;
                        assets.push((asset.name, bytes));
                    }
                }
            }
            _ => {}
        }
    }
    if found.is_empty() {
        return Err(OpError::invalid("from", format!("{path} holds no item manifests (`*.toml`)")));
    }
    Ok((found, assets))
}

/// Checks a manifest the way installing needs, before anything is written.
fn check(found: &Found) -> Result<PublisherCheck, OpError> {
    let m = &found.manifest;
    let id = m.item_id().map_err(|e| OpError::invalid("manifest", e.to_string()))?;
    if id.is_first_party() {
        return Err(OpError::invalid("manifest", format!("{id}: omuzaki/ items come with Omuzaki and aren't installed")));
    }
    m.item_version().map_err(|e| OpError::invalid("manifest", format!("{id}: {e}")))?;
    let problems = m.check();
    if !problems.is_empty() {
        let all: Vec<String> = problems.iter().map(ToString::to_string).collect();
        return Err(OpError::invalid("manifest", format!("{id}: {}", all.join("; "))));
    }
    match m.verify(Origin::Installed) {
        PublisherCheck::Refused { reason } => Err(OpError::invalid("manifest", format!("{id}: {reason}"))),
        check => Ok(check),
    }
}

/// The publisher as the person is shown it.
fn publisher_json(check: &PublisherCheck) -> Value {
    match check {
        PublisherCheck::Verified { name, fingerprint } => json!({ "verified": true, "name": name, "key": fingerprint }),
        PublisherCheck::Unverified { reason } => json!({ "verified": false, "name": "unverified publisher", "reason": reason }),
        PublisherCheck::FirstParty | PublisherCheck::Refused { .. } => json!({ "verified": false }),
    }
}

fn who(ctx: &OpCtx<'_>) -> String {
    match ctx.confirmed_by().unwrap_or(ctx.actor) {
        Actor::Person { name } => name.clone(),
        other => format!("{other:?}"),
    }
}

/// Every asset each manifest names, from the folder (checked against its hash) or already in the store.
fn assets_for<'a>(
    found: &[Found],
    files: &'a [(String, Vec<u8>)],
    store: Option<&dyn Store>,
) -> Result<Vec<(usize, &'a [u8])>, OpError> {
    let mut wanted = Vec::new();
    for (index, f) in found.iter().enumerate() {
        for (name, hash) in &f.manifest.assets {
            let file = files.iter().find(|(file, _)| *file == hash.to_string());
            match file {
                Some((_, bytes)) if ContentHash::of(bytes) == *hash => wanted.push((index, bytes.as_slice())),
                Some(_) => {
                    return Err(OpError::invalid("from", format!("assets/{hash} doesn't match its hash ({name})")));
                }
                None if store.is_some_and(|s| s.contains(hash)) => {}
                None => {
                    return Err(OpError::invalid(
                        "from",
                        format!("{}: asset `{name}` ({hash}) isn't in assets/ or in your store", f.manifest.id),
                    ));
                }
            }
        }
    }
    Ok(wanted)
}

impl Op for LibraryInstall {
    fn meta() -> OpMeta {
        OpMeta {
            name: "library.install",
            summary: "Install library items into your own library.",
            kind: OpKind::Edit,
            scope: Scope::EditLibrary,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (found, files) = match (&self.from, &self.manifest, self.manifests.is_empty()) {
            (Some(path), None, true) => read_from(ctx, path)?,
            (None, Some(text), true) => (vec![parse(text.clone(), "manifest")?], Vec::new()),
            (None, None, false) => {
                let found = self.manifests.iter().map(|text| parse(text.clone(), "manifests")).collect::<Result<_, _>>()?;
                (found, Vec::new())
            }
            _ => {
                return Err(OpError::invalid(
                    "from",
                    "give a manifest file or folder in `from`, or the text in `manifest` (or `manifests`)",
                ));
            }
        };
        let checks = found.iter().map(check).collect::<Result<Vec<_>, _>>()?;
        let names: Vec<String> = found
            .iter()
            .map(|f| format!("{}@{} ({})", f.manifest.id, f.manifest.version, f.manifest.rights.licence))
            .collect();
        ctx.require_person(format!("install {} into your library", names.join(", ")))?;
        let library = host(ctx)?;
        let store = ctx.service::<dyn Store>().ok();
        let assets = assets_for(&found, &files, store)?;
        if !assets.is_empty() && store.is_none() {
            return Err(OpError::unavailable("the asset store").hint("these items carry binaries; open a data folder"));
        }

        let at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default();
        let mut installed = Vec::new();
        let mut pins = Vec::new();
        for (index, (f, publisher)) in found.iter().zip(&checks).enumerate() {
            let m = &f.manifest;
            let (id, version) = (m.item_id(), m.item_version());
            let (Ok(id), Ok(version)) = (id, version) else {
                return Err(OpError::internal("a checked manifest lost its id or version"));
            };
            let new = if ctx.dry_run { true } else { library.install(m).map_err(|e| failed(&e))? };
            if !ctx.dry_run {
                if let Some(store) = store {
                    for (_, bytes) in assets.iter().filter(|(i, _)| *i == index) {
                        store
                            .put(bytes, m.rights.to_provenance(&id, &version))
                            .map_err(|e| OpError::unavailable("the asset store").hint(e.to_string()))?;
                    }
                }
                let key = m.publisher.as_ref().filter(|_| matches!(publisher, PublisherCheck::Verified { .. }));
                library
                    .acknowledge(Acknowledgement {
                        publisher: key.map_or_else(|| "unverified".to_owned(), |p| p.key.clone()),
                        hash: m.hash().to_string(),
                        item: format!("{}@{}", m.id, m.version),
                        licence: m.rights.licence.clone(),
                        by: who(ctx),
                        at,
                    })
                    .map_err(|e| failed(&e))?;
            }
            if new {
                pins.push(json!({ "item": m.id, "version": m.version }));
            }
            installed.push(json!({
                "item": m.id,
                "version": m.version,
                "hash": m.hash().to_string(),
                "licence": m.rights.licence,
                "publisher": publisher_json(publisher),
                "new": new,
            }));
        }
        let redo = match &self.from {
            Some(path) => json!({ "from": path }),
            None => json!({ "manifests": found.iter().map(|f| f.text.clone()).collect::<Vec<_>>() }),
        };
        let effect = Effect {
            summary: format!("installed {} in your library", names.join(", ")),
            undo: call("library.uninstall", json!({ "items": pins })),
            redo: call("library.install", redo),
        };
        let output = json!({ "installed": installed, "disclaimer": DISCLAIMER });
        Ok(Plan { output: Some(output), effect: (!pins.is_empty()).then_some(effect), ..Plan::default() })
    }
}

register_op!(LibraryInstall);
