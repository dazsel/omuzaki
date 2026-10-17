//! `part.list_kinds`: the part kinds this build can make, so an agent knows what `part.add` accepts.
//!
//! The answer is whatever kinds the linker collected (`omuzaki-char-api` registers them by kind), so a new generator
//! shows up here without this file changing.

use omuzaki_char_api::{PartMeta, kinds};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Lists the registered part kinds, sorted by id: for each one what it builds, its category (body, head, hair, garment,
/// footwear, accessory) and the kinds a part of it needs built before it (hair needs a head, a shirt needs a body).
/// Takes no arguments. Use a kind's id as `kind` in `part.add`, and `part.describe` on a part to see its parameters.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {}
    ],
    "x-omuzaki-fixture" = "demo"
))]
pub struct PartListKinds {}

/// The facts of every registered kind, sorted by id. The registry holds them in link order, which differs between
/// builds, so every surface sorts before showing them.
pub(crate) fn registered() -> Vec<PartMeta> {
    let mut metas: Vec<PartMeta> = kinds().map(|k| (k.meta)()).collect();
    metas.sort_unstable_by_key(|m| m.id);
    metas
}

impl Op for PartListKinds {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.list_kinds",
            summary: "List the part kinds this build can make: id, what it builds, category and the kinds it needs.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let kinds: Vec<_> = registered()
            .into_iter()
            .map(|m| json!({"id": m.id, "summary": m.summary, "category": m.category, "requires": m.requires}))
            .collect();
        Ok(Plan::answer(json!({ "kinds": kinds })))
    }
}

register_op!(PartListKinds);
