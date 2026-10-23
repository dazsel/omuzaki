//! `library.uninstall`: remove an item from the person's own library.

use omuzaki_library_api::{ItemScope, Library, VersionReq};
use omuzaki_ops_api::{Effect, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::common::{failed, host, item_id, version};
use crate::ops::install::call;

/// Removes an item from your library (design D9). Projects keep their pins and what the item made: a project never
/// reads the library to build. A person's decision: an agent's call asks the person first. Undo installs it again.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "item": "signer/hair/ribbon-bow", "version": "1.0.0" }]))]
pub struct LibraryUninstall {
    /// The item id.
    pub item: String,
    /// The version; the newest in your library without it.
    #[serde(default)]
    pub version: Option<String>,
}

impl Op for LibraryUninstall {
    fn meta() -> OpMeta {
        OpMeta {
            name: "library.uninstall",
            summary: "Remove an item from your own library.",
            kind: OpKind::Edit,
            scope: Scope::EditLibrary,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let id = item_id("item", &self.item)?;
        let library = host(ctx)?;
        let wanted = if let Some(v) = &self.version {
            version("version", v)?
        } else {
            let mine = library
                .search(&omuzaki_library_api::Query {
                    scope: Some(ItemScope::Mine),
                    text: Some(id.to_string()),
                    limit: 100,
                    ..Default::default()
                })
                .map_err(|e| failed(&e))?;
            mine.items.into_iter().find(|i| i.id == id).map(|i| i.version).ok_or_else(|| {
                OpError::not_found(format!("{id} is not in your library")).hint("list it with library.list {\"scope\": \"mine\"}")
            })?
        };
        let held = library.resolve(&id, &VersionReq::Exact(wanted)).map_err(|e| failed(&e))?;
        if held.scope != ItemScope::Mine {
            return Err(OpError::invalid("item", format!("{id}@{wanted} isn't in your library; it comes with Omuzaki")));
        }
        ctx.require_person(format!("remove {id}@{wanted} from your library"))?;
        let manifest = if ctx.dry_run { held.manifest } else { library.uninstall(&id, &wanted).map_err(|e| failed(&e))? };
        let text = toml::to_string(&manifest).map_err(|e| OpError::internal(e.to_string()))?;
        let effect = Effect {
            summary: format!("removed {id}@{wanted} from your library"),
            undo: call("library.install", json!({ "manifest": text })),
            redo: call("library.uninstall", json!({ "item": id, "version": wanted.to_string() })),
        };
        let output = json!({ "item": id, "version": wanted.to_string() });
        Ok(Plan { output: Some(output), effect: Some(effect), ..Plan::default() })
    }
}

register_op!(LibraryUninstall);
