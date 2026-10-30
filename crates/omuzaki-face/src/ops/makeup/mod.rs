//! The makeup ops (#2472): `face.makeup.list_presets` lists the makeup looks and `face.makeup.preset` gives a character
//! one. A look sets every makeup field (eyeshadow, lip colour, blush, contour, the nose's shade and highlight) in one
//! undoable edit, so nothing of an earlier look is left behind; each field stays editable on its own with
//! `face.set_style`. The looks are in [`crate::makeup_presets`].

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::makeup_presets::{preset_makeup, presets, set};
use crate::ops_shared::check::check_edit;
use crate::ops_shared::edit::{changed_output, diff, stored_face, working_copy};
use crate::ops_shared::path::template;

/// Lists the makeup presets: each one's `id` and a line on what it looks like.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct ListPresets {}

impl Op for ListPresets {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.makeup.list_presets",
            summary: "List the makeup looks (none, natural, soft-glam, idol, gothic, ...) with a line on each",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        let list: Vec<Value> = presets()
            .map_err(OpError::internal)?
            .into_iter()
            .map(|p| json!({ "id": p.id, "summary": p.summary }))
            .collect();
        Ok(Plan::answer(json!({ "presets": list })))
    }
}

register_op!(ListPresets);

/// Gives a character a makeup look (`face.makeup.list_presets` lists them): eyeshadow, lip colour, blush style and
/// strength, contour and the nose's shade and highlight all become the look's; `none` takes the makeup off.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "character": "kit", "preset": "natural" }, { "character": "kit", "preset": "none" }],
    "x-omuzaki-fixture" = "character"
))]
pub struct SetPreset {
    /// The character whose makeup changes.
    pub character: CharId,
    /// The preset's id.
    pub preset: String,
}

impl Op for SetPreset {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.makeup.preset",
            summary: "Give a character a makeup look (eyeshadow, lip colour, blush, contour, nose shade and highlight), \
                      or take it off with `none`, and return the changed paths",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let fields = preset_makeup(&self.preset).map_err(|e| {
            OpError::invalid("preset", e).hint("`face.makeup.list_presets` lists the presets")
        })?;
        let stored = stored_face(ctx, &self.character)?;
        let defaults = template()?;
        let mut working = working_copy(stored.as_ref());
        for (path, value) in fields {
            set(&mut working, path, value);
        }
        check_edit(stored.as_ref(), &working, &defaults)?;
        let mut patch = Patch::new();
        diff(&paths::face(&self.character), stored.as_ref(), Some(&working), &mut patch);
        let mut plan = Plan::edit(patch);
        let mut output = changed_output(&plan.patch, None);
        if let Value::Object(map) = &mut output {
            map.insert("preset".to_owned(), json!(self.preset));
        }
        plan.output = Some(output);
        Ok(plan)
    }
}

register_op!(SetPreset);
