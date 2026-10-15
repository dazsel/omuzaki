//! Stand-ins for the three ops the fixtures use. The real ones are in `omuzaki-char` and `omuzaki-face`, which a
//! mock may not depend on; these take the same arguments and write the same places.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub fn character() -> CharId {
    CharId::new("mira").unwrap()
}

/// The mock character stored at `characters/mira`.
pub fn doc() -> MemDoc {
    let def: Value = serde_json::from_str(omuzaki_char_mock::CHARACTER_JSON).unwrap();
    MemDoc::from_value(json!({ "characters": { "mira": def } }))
}

fn meta(name: &'static str) -> OpMeta {
    OpMeta { name, summary: "A stand-in.", kind: OpKind::Edit, scope: Scope::EditCharacter, undo: Undo::Auto }
}

fn set(ctx: &OpCtx<'_>, at: &DocPath, value: Value) -> Plan {
    Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(at).cloned(), Some(value)))
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PartAdd {
    character: CharId,
    kind: String,
    id: PartId,
    layer: Option<i32>,
}

impl Op for PartAdd {
    fn meta() -> OpMeta {
        meta("part.add")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = paths::character(&self.character).join("parts").join(self.id.as_str());
        if ctx.doc.get(&at).is_some() {
            return Err(OpError::invalid("id", "a part with this id exists"));
        }
        Ok(set(ctx, &at, json!({ "kind": self.kind, "layer": self.layer })))
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PaletteSet {
    character: CharId,
    slot: String,
    colour: String,
}

impl Op for PaletteSet {
    fn meta() -> OpMeta {
        meta("palette.set")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = paths::palette(&self.character).join(&self.slot);
        if ctx.doc.get(&at).is_none() {
            return Err(OpError::invalid("slot", "no such palette slot"));
        }
        Ok(set(ctx, &at, json!(self.colour)))
    }
}

#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetStyle {
    character: CharId,
    path: String,
    value: Value,
}

impl Op for SetStyle {
    fn meta() -> OpMeta {
        meta("face.set_style")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = paths::character(&self.character).join(self.path.as_str());
        Ok(set(ctx, &at, self.value.clone()))
    }
}

register_op!(PartAdd);
register_op!(PaletteSet);
register_op!(SetStyle);
