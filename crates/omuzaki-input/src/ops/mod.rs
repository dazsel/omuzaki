//! The `input.capture.*` and `input.binding.*` ops (design §4, §5, §13 IN-8). They reach the hub through the
//! `dyn InputControl` service the App registers, so no op ever sees a chord, an indicator or a key: the chord store,
//! the indicator host and the protected-list editor are handed to the person's surfaces directly, never to `Services`.
//!
//! | Op | Kind | Scope | Who |
//! |---|---|---|---|
//! | `input.capture.start` | edit | `input` | a person only (an agent or automation is `Denied`, never queued) |
//! | `input.capture.pause` | edit | `read` | anyone |
//! | `input.capture.stop` | edit | `read` | anyone |
//! | `input.capture.state` | query | `read` | anyone; the pause reason only with `process` |
//! | `input.capture.protect` | edit | `process` | a person, or an agent the person confirms |
//! | `input.binding.list` | query | `read` | anyone |
//! | `input.binding.set_action` | edit | `perform`, and every scope of the action's op | a person, or a confirmed agent |
//! | `input.binding.remove` | edit | `perform` | a person, or a confirmed agent |
//! | `input.binding.apply_preset` | edit | `perform`, and every scope of the preset's actions | a person, or a confirmed agent |
//!
//! Capture and bindings live outside the document, and none of these is undone through history: undoing a stop must
//! never start capture.

mod binding_apply_preset;
mod binding_list;
mod binding_remove;
mod binding_set_action;
mod capture_pause;
mod capture_protect;
mod capture_start;
mod capture_state;
mod capture_stop;

use omuzaki_input_api::binding::{BindingAction, BindingId};
use omuzaki_input_api::capture::InputError;
use omuzaki_input_api::feed::InputControl;
use omuzaki_ops_api::{OpCtx, OpError, Scope, find};
use serde_json::{Value, json};

/// The hub's control, as the App registered it.
fn control<'a>(ctx: &OpCtx<'a>) -> Result<&'a dyn InputControl, OpError> {
    ctx.service::<dyn InputControl>()
}

/// An `InputError` as the op's answer. None carries a key, a chord or a device.
fn error(error: &InputError) -> OpError {
    match error {
        InputError::NoIndicator => OpError::conflict(error.to_string())
            .hint("capture runs only while its indicator is on screen: open the Omuzaki window and start it there"),
        InputError::NotCapturing => OpError::conflict(error.to_string()).hint("start input capture first"),
        InputError::UnknownBinding(_) => OpError::not_found(error.to_string()).hint("input.binding.list lists them"),
        InputError::UnknownPreset(_) => OpError::invalid("preset", error.to_string()).hint("the presets: locomotion"),
        InputError::TooManyBindings => OpError::conflict(error.to_string()).hint("remove a binding first"),
        InputError::Unavailable(_) => OpError::unsupported(error.to_string()),
        _ => OpError::internal(error.to_string()),
    }
}

/// Refuses an action whose op the caller could not run itself: a binding must never let an actor make the person's
/// key press run an op beyond the actor's own grants (design §5). An op action must also name a registered op and
/// carry arguments it decodes; a held op must also take a `down` argument.
fn check_action(ctx: &OpCtx<'_>, field: &str, action: &BindingAction) -> Result<(), OpError> {
    let Some((op, args)) = action.op_call() else { return Ok(()) };
    let registration = find(op)
        .ok_or_else(|| OpError::invalid(field, format!("no op is named {op:?}")).hint("list the ops with tools/list"))?;
    let held = matches!(action, BindingAction::Hold { .. });
    if held && (registration.schema)().get("properties").and_then(|p| p.get("down")).is_none() {
        return Err(OpError::invalid(field, format!("{op} takes no `down` argument, so it cannot be held"))
            .hint("bind it as an op that runs on the press"));
    }
    let args = if held { crate::actions::with_down(args.clone(), true) } else { args.clone() };
    (registration.decode)(args).map_err(|e| OpError::invalid(field, format!("{op}: {}", e.message)))?;
    let scope = (registration.meta)().scope;
    if ctx.allows(scope) && ctx.allows(Scope::Read) {
        Ok(())
    } else {
        Err(OpError::denied(format!("a binding may not run {op}: it needs the {scope} scope, which this request lacks"))
            .hint(format!("ask for the {scope} scope, or bind an op this request may run")))
    }
}

/// Whether `id` is a binding now (a dry run checks this instead of changing anything).
fn known(control: &dyn InputControl, id: BindingId) -> Result<(), OpError> {
    if control.bindings().iter().any(|b| b.id == id) { Ok(()) } else { Err(error(&InputError::UnknownBinding(id))) }
}

/// An answer that says what changed.
fn answer(ctx: &OpCtx<'_>, fields: Value) -> Value {
    let mut out = json!({ "dry_run": ctx.dry_run });
    if let (Some(out), Value::Object(fields)) = (out.as_object_mut(), fields) {
        out.extend(fields);
    }
    out
}
