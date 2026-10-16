//! `input.bind_camera`: bind the camera the webcam source reads. Only the person may: it is a device binding.

use omuzaki_ops_api::{Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_track_api::camera::CameraId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{changed, host, op_error, person_only, persist};
use crate::perform::input::InputCamera;

/// Bind the camera the webcam source reads, by an id from `input.state`; `null` unbinds it. A running preview moves to
/// the new camera. Only the person can do this, in the Omuzaki window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BindCamera {
    /// The camera's id, as `input.state` lists it, or `null` to unbind.
    pub camera: Option<CameraId>,
}

impl Op for BindCamera {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.bind_camera",
            summary: "Bind the camera the webcam source reads (person only); a running preview moves to it.",
            kind: OpKind::Edit,
            scope: Scope::EditSettings,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if let Some(camera) = self.camera.as_ref().filter(|camera| camera.0.trim().is_empty()) {
            return Err(OpError::invalid("camera", format!("'{}' is not a camera id", camera.0))
                .hint("use an id that `input.state` lists, or null to unbind"));
        }
        person_only(ctx, "which physical camera runs is the person's choice")?;
        let host = host(ctx)?;
        // Checked first, so a camera that is not there is refused before anything is saved.
        let before = host.bind_camera(self.camera.clone(), Mode::DryRun).map_err(op_error)?;
        let saved = persist::<InputCamera>(ctx, &self.camera)?;
        if !ctx.dry_run {
            host.bind_camera(self.camera.clone(), Mode::Apply).map_err(op_error)?;
        }
        let answer = json!({ "camera": self.camera, "before": before, "saved": saved, "dry_run": ctx.dry_run });
        let name = |camera: &Option<CameraId>| camera.as_ref().map_or_else(|| "none".to_owned(), |c| c.0.clone());
        let summary = format!("camera {} to {}", name(&before), name(&self.camera));
        changed(Self::meta().name, "camera", before, &self.camera, summary, answer)
    }
}

omuzaki_ops_api::register_op!(BindCamera);
