//! `library.describe`: one item in full.

use omuzaki_library_api::Step;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::common::{failed, item_id, library, rights_note, row, version};

/// Describes one item (design D9): its row, every installed version, its parameters, what it creates, its recipe
/// step by step, and its rights explained.
#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "item": "omuzaki/colourways/night-iris" }]))]
pub struct LibraryDescribe {
    /// The item id, `publisher/name`.
    pub item: String,
    /// One version; the newest without it.
    #[serde(default)]
    pub version: Option<String>,
}

impl Op for LibraryDescribe {
    fn meta() -> OpMeta {
        OpMeta {
            name: "library.describe",
            summary: "Describe a library item: versions, parameters, recipe and rights.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let id = item_id("item", &self.item)?;
        let wanted = self.version.as_deref().map(|v| version("version", v)).transpose()?;
        let detail = library(ctx)?.describe(&id, wanted.as_ref()).map_err(|e| failed(&e))?;
        let m = &detail.item.manifest;
        let steps: Vec<_> = m
            .steps
            .iter()
            .map(|step| match step {
                Step::Op(op) => json!({ "op": op.op, "args": op.args }),
                Step::Item(item) => json!({ "item": item.item, "version": item.version, "params": item.params }),
            })
            .collect();
        let versions: Vec<String> = detail.versions.iter().map(ToString::to_string).collect();
        Ok(Plan::answer(json!({
            "row": row(&detail.info),
            "versions": versions,
            "slot": m.slot,
            "params": m.params,
            "creates": m.creates,
            "compat": m.compat,
            "steps": steps,
            "rights": m.rights,
            "rights_note": rights_note(detail.info.rights.publish_ok, &m.rights.licence),
            "publisher": format!("{:?}", detail.item.publisher),
        })))
    }
}

register_op!(LibraryDescribe);
