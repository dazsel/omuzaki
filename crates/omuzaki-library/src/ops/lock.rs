//! `library.lock`: every pin in the project, and whether each still resolves.

use omuzaki_char_api::{CharId, paths};
use omuzaki_library_api::VersionReq;
use omuzaki_store_api::Store;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::apply::JoinPath;
use crate::common::{library, record_json};
use crate::records;

/// Lists every application in the project, or on one character (design D5, D9): its pin (`item`, `version`,
/// `hash`), whether that pin `resolves` (`ok`, from the library or from the copy the project kept when it was
/// applied; `mismatch` when the library holds the version with other content; `missing`), the newest version in the same major, the paths the person has changed since
/// (`overridden`) and whether anything it wrote remains (`stale` when nothing does, so `library.detach` may drop it).
/// The records are the lock: a project never changes until someone runs `library.update`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("x-omuzaki-fixture" = "character-library", "examples" = [{}, { "character": "kit" }]))]
pub struct LibraryLock {
    /// Only this character.
    #[serde(default)]
    pub character: Option<CharId>,
}

impl Op for LibraryLock {
    fn meta() -> OpMeta {
        OpMeta {
            name: "library.lock",
            summary: "List every library item the project pins, and whether each still resolves.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let library = library(ctx)?;
        let store = ctx.service::<dyn Store>().ok();
        let characters = match &self.character {
            Some(id) => vec![id.clone()],
            None => records::characters(ctx.doc),
        };
        let mut rows = Vec::new();
        for character in characters {
            let node = paths::character(&character);
            for (app, record) in records::of_character(ctx.doc, &character) {
                // The library's copy first, else the project's own (design D5): an item that left the library still
                // resolves from the copy apply stored.
                let copy = || store.is_some_and(|s| s.contains(&record.hash));
                let (resolves, from) = match library.resolve(&record.item, &VersionReq::Exact(record.version)) {
                    Ok(found) if found.hash == record.hash => ("ok", Some("library")),
                    Ok(_) => ("mismatch", copy().then_some("project")),
                    Err(_) if copy() => ("ok", Some("project")),
                    Err(_) => ("missing", None),
                };
                let newest = VersionReq::parse(&format!("^{}", record.version))
                    .ok()
                    .and_then(|req| library.resolve(&record.item, &req).ok())
                    .map(|found| found.manifest.version);
                let now = |path: &str| ctx.doc.get(&node.join_path(path)).cloned();
                let overridden: Vec<&String> =
                    record.wrote.iter().filter(|(path, value)| now(path).as_ref() != Some(value)).map(|(p, _)| p).collect();
                let stale = !record.wrote.is_empty() && overridden.len() == record.wrote.len();
                let mut shown = record_json(&character, &app, &record);
                shown["resolves"] = json!(resolves);
                shown["resolved_from"] = json!(from);
                shown["newest"] = json!(newest);
                shown["overridden"] = json!(overridden);
                shown["stale"] = json!(stale);
                rows.push(shown);
            }
        }
        let total = rows.len();
        Ok(Plan::answer(json!({ "applications": rows, "total": total })))
    }
}

register_op!(LibraryLock);

