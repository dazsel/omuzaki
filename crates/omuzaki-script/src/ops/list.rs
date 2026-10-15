//! `script.list`: the saved scripts and macros, in the project and in My scripts.

use omuzaki_doc_api::Value;
use omuzaki_ops_api::{ErrorCode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::saved::{Item, Place, project_nodes, store};

/// Lists what is saved: the project's scripts and macros (the document nodes under `scripts`) and, when the app wired a
/// store, My scripts. The answer is `{ "scripts": [..] }`, sorted by place and id; each entry has `id`, `name`, `kind`
/// (`script` or `macro`), `place` (`project` or `mine`), `hash` (the content hash) and, for project items, `author` (who
/// saved it last). An entry that can't be read has a `problem` instead of a kind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptList {}

impl Op for ScriptList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "script.list",
            summary: "List the saved scripts and macros, in the project and in My scripts.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut entries: Vec<(Place, String, Value)> = Vec::new();
        for (id, node) in project_nodes(ctx.doc) {
            entries.push((Place::Project, id.clone(), entry(&id, Place::Project, Item::from_node(&id, &node))));
        }
        match store(ctx) {
            Ok(store) => {
                for stored in store.list()? {
                    let id = stored.id.clone();
                    entries.push((Place::Mine, id.clone(), entry(&id, Place::Mine, Item::from_stored(&stored))));
                }
            }
            // No store wired is not an error for a listing: there is just nothing in My scripts to show.
            Err(e) if e.code == ErrorCode::Unavailable => {}
            Err(e) => return Err(e),
        }
        entries.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        let scripts: Vec<Value> = entries.into_iter().map(|(_, _, entry)| entry).collect();
        Ok(Plan::answer(json!({ "scripts": scripts })))
    }
}

fn entry(id: &str, place: Place, item: Result<Item, String>) -> Value {
    match item {
        Ok(item) => json!({
            "id": id,
            "name": item.name,
            "kind": item.body.kind(),
            "place": place,
            "hash": item.hash,
            "author": item.author,
        }),
        Err(problem) => json!({ "id": id, "place": place, "problem": problem }),
    }
}

register_op!(ScriptList);
