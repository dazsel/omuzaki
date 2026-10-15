//! `face.set_rest_channels`: writes the `rest` bus layer, the channel values the painter reads on top of the neutral bus.

use std::collections::BTreeMap;

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::Patch;
use omuzaki_face_api::{FaceProfile, PaintParams};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value, json};

use crate::ops::mapping::repaint_report;
use crate::ops_shared::check::check_edit;
use crate::ops_shared::edit::{changed, diff, stored_face, working_copy};
use crate::ops_shared::path::{channel, template};

/// Sets channel values in a character's `rest` layer, the face's resting bus: a name of a channel and its number. Channels
/// that aren't named keep their values. Use `face.set_rest` for the resting face in paint terms instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetRestChannels {
    /// The character whose rest layer changes.
    pub character: CharId,
    /// Channel name (as on the animation bus, for example `mouthSmileLeft`) to its value.
    pub channels: BTreeMap<String, f64>,
}

impl Op for SetRestChannels {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.set_rest_channels",
            summary: "Set channel values in a character's rest layer (the resting bus), validate the whole face, and return \
                      the changed paths and the paint before and after",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let stored = stored_face(ctx, &self.character)?;
        let defaults = template()?;
        let mut working = working_copy(stored.as_ref());
        let Some(face) = working.as_object_mut() else {
            return Err(OpError::internal("a face is an object"));
        };
        let rest = face.entry("rest").or_insert_with(|| Value::Object(Map::new()));
        let Some(rest) = rest.as_object_mut() else {
            return Err(OpError::invalid(
                "face/rest",
                "face/rest is not a map of channels, so it can't be edited inside",
            )
            .hint("set face/rest as a whole map first"));
        };
        let unknown: Vec<OpError> = self.channels.keys().filter_map(|name| channel(name).err()).collect();
        if !unknown.is_empty() {
            return Err(all_unknown(&unknown));
        }
        for (name, value) in &self.channels {
            let number = Number::from_f64(*value).ok_or_else(|| {
                OpError::invalid(format!("face/rest/{name}"), "the value is not a finite number")
                    .hint("give the channel a number")
            })?;
            rest.insert(name.clone(), Value::Number(number));
        }
        check_edit(stored.as_ref(), &working, &defaults)?;

        let mut patch = Patch::new();
        diff(&paths::face(&self.character), stored.as_ref(), Some(&working), &mut patch);
        let mut plan = Plan::edit(patch);
        plan.output = Some(json!({
            "changed": changed(&plan.patch),
            "paint_with_rest_layer": repaint_report(stored.as_ref(), Some(&working), rest_layer_paint),
        }));
        Ok(plan)
    }
}

register_op!(SetRestChannels);

/// The paint of the neutral bus with the face's rest layer written on it, as the painter sees it.
fn rest_layer_paint(profile: &FaceProfile) -> PaintParams {
    profile.mapping.apply(&profile.rest_buf())
}

/// One refusal for every unknown channel name: the first names the field, the message lists each, and the hint gives each
/// one's nearest channel, so an agent fixes all the typos in one go.
fn all_unknown(errors: &[OpError]) -> OpError {
    let count = errors.len();
    let noun = if count == 1 { "field is" } else { "fields are" };
    let field = errors.first().and_then(|e| e.field.clone()).unwrap_or_default();
    let list =
        errors.iter().map(|e| format!("{}: {}", e.field.as_deref().unwrap_or_default(), e.message)).collect::<Vec<_>>();
    let hints = errors.iter().filter_map(|e| e.hint.clone()).collect::<Vec<_>>();
    OpError::invalid(field, format!("{count} {noun} invalid: {}", list.join("; "))).hint(hints.join("; "))
}
