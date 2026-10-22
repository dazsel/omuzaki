//! `settings.set`, `settings.reset` and `settings.reload`: the edits. Each is undone explicitly, by the inverse call
//! it records (ADR 0036): a set is undone by setting the value before, or by a reset when the value was the default;
//! a reset is undone by setting the value it removed.

use omuzaki_ops_api::{Actor, Effect, Mode, Op, OpCall, OpCtx, OpError, OpMeta, Plan, register_op};
use omuzaki_settings_api::event::Change;
use omuzaki_settings_api::service::{Layer, Source};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{edit, may_write, op_error, service};

/// Change one setting. The value must have the setting's type and pass its rules; settings.get gives its schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "key": "ui.scale", "value": 125 }, { "key": "ui.theme", "value": "light" }]))]
pub struct Set {
    /// The setting's key, such as `ui.scale`.
    pub key: String,
    /// The new value, of the setting's type.
    pub value: Value,
    /// Where to write: `auto` (the active profile for a key a profile may hold, else the base) or `base`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<Layer>,
}

/// Put one setting back to what it falls back to: the base value under a profile, else the default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "key": "ui.scale" }]))]
pub struct Reset {
    /// The setting's key, such as `ui.scale`.
    pub key: String,
    /// Which layer to remove the key from: `auto` (the active profile, else the base) or `base`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<Layer>,
}

/// Read the settings files again, to pick up hand edits now instead of at the next start. A person's call.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Reload {}

/// The run's mode for the service.
fn mode(ctx: &OpCtx<'_>) -> Mode {
    if ctx.dry_run { Mode::DryRun } else { Mode::Apply }
}

fn layer_name(layer: Layer) -> Value {
    serde_json::to_value(layer).unwrap_or_else(|_| json!("auto"))
}

/// The answer of a write: the change, whether it was applied, and whether it waits for a restart.
fn answer(ctx: &OpCtx<'_>, change: &Change) -> Value {
    json!({
        "key": change.key,
        "before": change.before,
        "after": change.after,
        "changed": change.before != change.after,
        "applied": !ctx.dry_run,
        "apply": change.apply,
    })
}

fn set_call(key: &str, value: &Value, layer: Layer) -> OpCall {
    OpCall { op: "settings.set".to_owned(), args: json!({ "key": key, "value": value, "layer": layer_name(layer) }) }
}

fn reset_call(key: &str, layer: Layer) -> OpCall {
    OpCall { op: "settings.reset".to_owned(), args: json!({ "key": key, "layer": layer_name(layer) }) }
}

impl Op for Set {
    fn meta() -> OpMeta {
        edit("settings.set", "Change one setting to a value of its type; who may change it follows the key's rule.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let settings = service(ctx)?;
        let before = settings.resolve(&self.key).map_err(|e| op_error(settings, &e))?;
        may_write(ctx, &before.meta, "change")?;
        let layer = self.layer.unwrap_or(Layer::Auto);
        let change = settings
            .set_raw(&self.key, self.value.clone(), layer, mode(ctx), ctx.actor)
            .map_err(|e| op_error(settings, &e))?;
        let undo = if before.source == Source::Default {
            reset_call(&self.key, layer)
        } else {
            set_call(&self.key, &change.before, layer)
        };
        let effect = (change.before != change.after).then(|| Effect {
            summary: format!("{} set to {}", self.key, change.after),
            undo,
            redo: set_call(&self.key, &change.after, layer),
        });
        Ok(Plan { effect, ..Plan::answer(answer(ctx, &change)) })
    }
}

impl Op for Reset {
    fn meta() -> OpMeta {
        edit(
            "settings.reset",
            "Put one setting back to its fallback: the base value under a profile, else its default.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let settings = service(ctx)?;
        let before = settings.resolve(&self.key).map_err(|e| op_error(settings, &e))?;
        may_write(ctx, &before.meta, "reset")?;
        let layer = self.layer.unwrap_or(Layer::Auto);
        let change = settings.reset(&self.key, layer, mode(ctx), ctx.actor).map_err(|e| op_error(settings, &e))?;
        let Some(change) = change else {
            let value = &before.value;
            return Ok(Plan::answer(json!({
                "key": self.key,
                "before": value,
                "after": value,
                "changed": false,
                "applied": !ctx.dry_run,
                "apply": before.meta.apply,
            })));
        };
        let effect = (change.before != change.after).then(|| Effect {
            summary: format!("{} reset to {}", self.key, change.after),
            undo: set_call(&self.key, &change.before, layer),
            redo: reset_call(&self.key, layer),
        });
        Ok(Plan { effect, ..Plan::answer(answer(ctx, &change)) })
    }
}

impl Op for Reload {
    fn meta() -> OpMeta {
        edit("settings.reload", "Read the settings files again now, to pick up changes made to them by hand.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !matches!(ctx.actor, Actor::Person { .. }) {
            return Err(OpError::person_only("only a person may reload the settings files"));
        }
        let settings = service(ctx)?;
        let report = settings.reload(mode(ctx), ctx.actor).map_err(|e| op_error(settings, &e))?;
        Ok(Plan::answer(json!({
            "changed": report.changed,
            "pending_restart": report.pending_restart,
            "applied": !ctx.dry_run,
        })))
    }
}

register_op!(Set);
register_op!(Reset);
register_op!(Reload);
