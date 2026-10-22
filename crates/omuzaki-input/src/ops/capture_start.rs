//! `input.capture.start`: start or resume reading the OS input devices for the classes asked.

use omuzaki_input_api::event::{DeviceClass, DeviceMask};
use omuzaki_ops_api::{Actor, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{answer, control, error};

/// Start or resume input capture for these device classes (all when left out). Only a person can start it, and only
/// while the Omuzaki window shows the capture indicator; it is never restored after a restart.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "classes": ["keyboard", "mouse"] }]))]
pub struct CaptureStart {
    /// `keyboard`, `mouse`, `pen`, `gamepad`; every class when left out.
    #[serde(default)]
    pub classes: Option<Vec<DeviceClass>>,
}

impl Op for CaptureStart {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.capture.start",
            summary: "Start or resume reading keyboard, mouse, pen or gamepad for motion; a person only.",
            kind: OpKind::Edit,
            scope: Scope::Input,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        // Never queued for approval: an agent or an automation can't start reading another app's keystrokes (§4).
        if !matches!(ctx.actor, Actor::Person { .. }) {
            return Err(OpError::person_only("starting input capture"));
        }
        let control = control(ctx)?;
        let classes = self.classes.as_deref().map_or(DeviceMask::ALL, DeviceMask::of);
        if !ctx.dry_run {
            control.start(classes).map_err(|e| error(&e))?;
        }
        let state = control.report().state;
        Ok(Plan::answer(answer(ctx, json!({ "classes": classes, "state": state }))))
    }
}

register_op!(CaptureStart);
