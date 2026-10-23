//! `library.apply`: put an item on a character, as one undoable step.

use std::collections::BTreeMap;

use omuzaki_char_api::{CharId, PartId};
use omuzaki_library_api::{CompatStatus, LibraryError, ResolvedItem, VersionReq};
use omuzaki_store_api::Store;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::apply::{Request, Work, apply};
use crate::common::{facts, item_id, library, rights_note, version};
use crate::ops::update::{LibraryUpdate, Prefer};
use crate::records;

/// Applies a library item to a character (design D4 to D8): plans every step of its recipe on a scratch copy and
/// lands them as **one undoable step**, with a source row for each part it creates and the application record
/// (`characters/<id>/applied/<app>`), which pins the item's exact version and is the project's lock.
///
/// Without `version` it takes the version already pinned for that item on the character, else the newest that fits.
/// An item with a slot (a hairstyle, a colourway) replaces the slot's current item in the same step; applying the
/// same item again is `library.update`. A part of the same id that isn't from the library is kept (the new part gets
/// a new id) unless you name it in `replace`. A dry-run shows everything: the parts it creates, what it replaces, the
/// rights, and any warning.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "item": "omuzaki/colourways/night-iris", "character": "mia" },
    { "item": "omuzaki/hair/twintails", "character": "mia", "params": { "tail_length": 2.6 }, "replace": ["hair"] }
]))]
pub struct LibraryApply {
    /// The item id, `publisher/name`.
    pub item: String,
    /// The exact version (`1.2.0`); see above for the default.
    #[serde(default)]
    pub version: Option<String>,
    /// The character to apply it to.
    pub character: CharId,
    /// The existing part an item that changes a part (`$target`) works on.
    #[serde(default)]
    pub target: Option<PartId>,
    /// The name of the member an item adds (`$name`), instead of the item's own.
    #[serde(default, rename = "as")]
    pub name: Option<String>,
    /// Parameter values, by the names `library.describe` lists; the others take their defaults.
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
    /// Hand-made parts to remove in the same step, for an item that creates one like them.
    #[serde(default)]
    pub replace: Vec<PartId>,
}

impl Op for LibraryApply {
    fn meta() -> OpMeta {
        OpMeta {
            name: "library.apply",
            summary: "Apply a library item to a character as one undoable step.",
            kind: OpKind::Edit,
            scope: Scope::EditLibrary,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let id = item_id("item", &self.item)?;
        let facts = facts(ctx, &self.character)?;
        let library = library(ctx)?;
        // A version pinned on this character for the item wins over the newest (design D5).
        let pinned =
            records::of_character(ctx.doc, &self.character).into_iter().find(|(_, r)| r.item == id).map(|(_, r)| r);
        let req = match (&self.version, &pinned) {
            (Some(v), _) => VersionReq::Exact(version("version", v)?),
            (None, Some(record)) => VersionReq::Exact(record.version),
            (None, None) => VersionReq::Any,
        };
        let item = library.resolve(&id, &req).map_err(|e| crate::apply::library_error("item", &e))?;
        let mut warnings = Vec::new();
        match facts.status(&item.manifest.compat) {
            CompatStatus::Incompatible { reason } => {
                return Err(OpError::invalid("character", format!("{id} doesn't fit {}: {reason}", self.character))
                    .hint("search for items that fit with library.search {\"compatible_with\": {\"character\": ...}}"));
            }
            CompatStatus::StyleMismatch { reason } => warnings.push(reason),
            CompatStatus::Compatible => {}
        }
        // D6: the same item again in its slot is an update to the new parameters, at the pinned version unless one
        // is given (design D10).
        let again = records::of_character(ctx.doc, &self.character)
            .into_iter()
            .find(|(_, r)| r.item == id && r.slot.is_some() && r.slot == item.manifest.slot);
        if let Some((app, record)) = again {
            let update = LibraryUpdate {
                character: self.character.clone(),
                application: app,
                to_version: Some(self.version.clone().unwrap_or_else(|| record.version.to_string())),
                params: self.params.clone(),
                prefer: Prefer::Mine,
            };
            let mut plan = update.plan(ctx)?;
            if let Some(Value::Object(output)) = &mut plan.output {
                output.insert("updated".to_owned(), json!(true));
            }
            return Ok(plan);
        }
        let detail = library.describe(&id, Some(&item.manifest.item_version().map_err(|e| OpError::internal(e.to_string()))?));
        let publish_ok = detail.as_ref().is_ok_and(|d| d.info.rights.publish_ok);
        if let Ok(Some(why)) = detail.as_ref().map(|d| d.info.unavailable.clone()) {
            return Err(OpError::unsupported(format!("{id} can't be applied in this build: {why}")));
        }

        let resolve = |id: &_, req: &_| -> Result<ResolvedItem, LibraryError> { library.resolve(id, req) };
        let mut work = Work::of(ctx.doc);
        let request = Request {
            item: &item,
            character: &self.character,
            target: self.target.clone(),
            name: self.name.clone(),
            params: self.params.clone(),
            replace: self.replace.clone(),
        };
        let applied = apply(ctx, &resolve, &mut work, &request)?;
        warnings.extend(applied.warnings);
        let record = &applied.record;
        // The project keeps the exact manifest it pinned, so it still resolves after the item leaves the library
        // (design D5). Content-addressed, so applying again stores nothing new.
        if !ctx.dry_run
            && let Ok(store) = ctx.service::<dyn Store>()
        {
            let provenance = item.manifest.rights.to_provenance(&record.item, &record.version);
            if let Err(e) = store.put(&item.manifest.canonical_bytes(), provenance) {
                warnings.push(format!("the project's copy of {id} wasn't stored: {e}"));
            }
        }
        let output = json!({
            "item": record.item,
            "version": record.version.to_string(),
            "hash": record.hash.to_string(),
            "character": self.character,
            "application": applied.app,
            "created": applied.created,
            "replaced": applied.replaced,
            "params": record.params,
            "changed": record.wrote.len(),
            "rights": rights_note(publish_ok, &record.rights.licence),
            "warnings": warnings,
        });
        Ok(Plan { patch: work.combined, output: Some(output), ..Plan::default() })
    }
}

register_op!(LibraryApply);
