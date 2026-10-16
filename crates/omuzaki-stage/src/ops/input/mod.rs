//! The `input.*` ops (PF-01; `docs/design/first-run.md` §4, §5): choose the kind of source, bind the camera, start and
//! stop the live mirror, ask for the camera permission and read the input's state, all through the [`InputHost`]
//! service, so the Live workspace, the setup flow, MCP and agents share one path.
//!
//! | Op | Scope | Agent | Undo |
//! |---|---|---|---|
//! | `input.select {source}` | `edit:settings` | Confirm (queued for the person) | the inverse `input.select` |
//! | `input.bind_camera {camera}` | `edit:settings` | Never (`person_only`, refused, not queued) | the inverse `input.bind_camera` |
//! | `input.preview {on}` | `perform` | Confirm (it starts a capture) | the inverse `input.preview` |
//! | `input.request_camera` | `perform` | Never (the OS dialog is the person's) | none: the OS permission is the person's to revoke |
//! | `input.state` | `read` | free | none (a query) |
//!
//! The two choices are Machine settings (`input.source`, `input.camera`): when the App registers a
//! [`Settings`](omuzaki_settings_api::service::Settings) service, the ops write them there too, and their answer says
//! so in `saved`. The OS input capture ops of `docs/design/input-motion.md` share the namespace as `input.capture.*`
//! and `input.binding.*`; nothing here clashes with them.

use omuzaki_ops_api::{Actor, Effect, Mode, OpCall, OpCtx, OpError, Plan};
use omuzaki_settings_api::service::{Layer, Settings, SettingsExt};
use omuzaki_settings_api::setting::Setting;
use serde::Serialize;
use serde_json::Value;

use omuzaki_track_api::camera::CameraId;

use crate::perform::input::{InputError, InputHost, SourceKind};

pub mod bind_camera;
pub mod preview;
pub mod request_camera;
pub mod select;
pub mod state;

/// The input behind the ops, or `Unavailable` when the app did not wire one in.
fn host<'a>(ctx: &OpCtx<'a>) -> Result<&'a dyn InputHost, OpError> {
    ctx.service::<dyn InputHost>()
}

/// The host's mode for this run.
fn mode(ctx: &OpCtx<'_>) -> Mode {
    if ctx.dry_run { Mode::DryRun } else { Mode::Apply }
}

/// Refuses anyone but a person, with no confirmation possible (a device binding, the OS dialog).
fn person_only(ctx: &OpCtx<'_>, reason: &str) -> Result<(), OpError> {
    if matches!(ctx.actor, Actor::Person { .. }) { Ok(()) } else { Err(OpError::person_only(reason)) }
}

/// Writes `value` to the setting `S` when the App has a settings service, in the run's mode. Returns whether it was
/// saved: `false` when there is no service, so the choice holds until the app closes.
fn persist<S: Setting>(ctx: &OpCtx<'_>, value: &S::Value) -> Result<bool, OpError> {
    let Ok(settings) = ctx.service::<dyn Settings>() else { return Ok(false) };
    settings
        .set::<S>(value, Layer::Base, mode(ctx), ctx.actor)
        .map(|_| true)
        .map_err(|e| OpError::internal(format!("the setting `{}` could not be saved: {e}", S::META.key)))
}

/// Puts the camera binding back to `before` and, when a preview ran, starts it again: what an op does to undo its own
/// change when a later step of it fails. Says what could not be put back.
fn put_back_camera(host: &dyn InputHost, before: Option<&CameraId>, was_on: bool) -> Result<(), String> {
    host.bind_camera(before.cloned(), Mode::Apply).map_err(|e| e.to_string())?;
    if was_on {
        host.preview(true, Mode::Apply).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Puts the source back to `before` and, when a preview ran, starts it again (see [`put_back_camera`]).
fn put_back_source(host: &dyn InputHost, before: SourceKind, was_on: bool) -> Result<(), String> {
    host.select(before, Mode::Apply).map_err(|e| e.to_string())?;
    if was_on {
        host.preview(true, Mode::Apply).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// What the input holds, read back from the host: after a step failed partway this, not what was asked for, is what an
/// op must describe.
#[derive(Clone, Debug, PartialEq)]
struct Held {
    source: SourceKind,
    camera: Option<CameraId>,
    preview: bool,
}

/// What `host` holds now.
fn held(host: &dyn InputHost) -> Held {
    let state = host.state();
    Held { source: state.source, camera: state.camera, preview: state.preview.is_on() }
}

/// The plan of an op that ran, from what the host held before and holds after: the answer, and the pair of inverse calls
/// that undo and redo what actually changed. When `after` has the preview stopped where `before` had it running, the
/// undo starts it again too (`resume_preview`), or, when nothing else changed, is a `input.preview` call of its own.
fn settled(
    op: &str,
    field: &str,
    (before_value, after_value): (Value, Value),
    (before, after): (&Held, &Held),
    summary: String,
    answer: Value,
) -> Result<Plan, OpError> {
    let mut plan = changed(op, field, before_value, after_value, summary, answer)?;
    if before.preview && !after.preview {
        if let Some(effect) = plan.effect.as_mut() {
            effect.undo.args["resume_preview"] = serde_json::json!(true);
        } else {
            let call = |on: bool| OpCall { op: "input.preview".to_owned(), args: serde_json::json!({ "on": on }) };
            plan.effect = Some(Effect { summary: "camera preview stopped".to_owned(), undo: call(true), redo: call(false) });
        }
    }
    Ok(plan)
}

/// The answer `value` serialises to.
fn to_value(value: impl Serialize) -> Result<Value, OpError> {
    serde_json::to_value(value).map_err(|e| OpError::internal(e.to_string()))
}

/// The plan of an op that changed `before` to `after` outside the document: its answer, and, when something changed,
/// the pair of inverse calls (`op` with `field` set to each value) that undo and redo it (ADR 0036).
fn changed(
    op: &str,
    field: &str,
    before: impl Serialize,
    after: impl Serialize,
    summary: String,
    answer: Value,
) -> Result<Plan, OpError> {
    let (before, after) = (to_value(before)?, to_value(after)?);
    let call = |value: &Value| OpCall { op: op.to_owned(), args: serde_json::json!({ field: value }) };
    let effect = (before != after).then(|| Effect { summary, undo: call(&before), redo: call(&after) });
    Ok(Plan { effect, ..Plan::answer(answer) })
}

/// What each refusal means to a caller of an op.
#[allow(clippy::needless_pass_by_value)] // `map_err` hands the error over by value
fn op_error(error: InputError) -> OpError {
    use omuzaki_track_api::error::TrackError;
    let message = error.to_string();
    match error {
        InputError::NoCamera { .. } => {
            OpError::not_found(message).hint("plug the camera in, or pick one that `input.state` lists")
        }
        InputError::NoCameraBound => {
            OpError::conflict(message).hint("bind a camera first (`input.state` lists them), then start the preview")
        }
        InputError::NothingToCapture => {
            OpError::conflict(message).hint("select the webcam source with `input.select`, then start the preview")
        }
        InputError::Unsupported(_) => {
            OpError::unsupported(message).hint("use the webcam source, or the life layer alone")
        }
        InputError::Device(TrackError::PermissionDenied) => OpError::denied(message)
            .hint("the person allows the camera in the OS settings (`input.request_camera` asks)"),
        InputError::Device(TrackError::Busy(_)) => {
            OpError::conflict(message).hint("close the other app that uses the camera, then try again")
        }
        InputError::Device(TrackError::NoDevice(_) | TrackError::Lost) => {
            OpError::not_found(message).hint("plug the camera in, or bind another one")
        }
        InputError::Device(_) => OpError::internal(message).hint("try again, or bind another camera"),
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::ErrorCode;
    use omuzaki_track_api::camera::{CameraFormatRequest, CameraId};
    use omuzaki_track_api::error::TrackError;

    use super::*;
    use crate::perform::input::SourceKind;

    #[test]
    fn every_refusal_maps_to_a_code_with_a_hint() {
        let camera = CameraId("cam".to_owned());
        let cases = [
            (InputError::no_camera(camera.clone()), ErrorCode::NotFound),
            (InputError::NoCameraBound, ErrorCode::Conflict),
            (InputError::NothingToCapture, ErrorCode::Conflict),
            (InputError::Unsupported(SourceKind::Iphone), ErrorCode::Unsupported),
            (InputError::Device(TrackError::PermissionDenied), ErrorCode::Denied),
            (InputError::Device(TrackError::Busy(camera.clone())), ErrorCode::Conflict),
            (InputError::Device(TrackError::NoDevice(camera)), ErrorCode::NotFound),
            (InputError::Device(TrackError::Lost), ErrorCode::NotFound),
            (InputError::Device(TrackError::FormatUnsupported(CameraFormatRequest::default())), ErrorCode::Internal),
            (InputError::Device(TrackError::Backend("model".to_owned())), ErrorCode::Internal),
        ];
        for (error, code) in cases {
            let mapped = op_error(error.clone());
            assert_eq!(mapped.code, code, "{error:?}");
            assert!(mapped.hint.is_some(), "{error:?} has no hint");
        }
    }
}
