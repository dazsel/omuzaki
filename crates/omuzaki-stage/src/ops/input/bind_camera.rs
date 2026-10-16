//! `input.bind_camera`: bind the camera the webcam source reads. Only the person may: it is a device binding.

use omuzaki_ops_api::{Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_track_api::camera::CameraId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{changed, host, op_error, person_only, persist, put_back_camera, retained};
use crate::perform::input::{InputCamera, InputError, SourceKind};

/// Bind the camera the webcam source reads, by an id from `input.state`; `null` unbinds it. A running preview moves to
/// the new camera. Only the person can do this, in the Omuzaki window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{ "camera": "fake:0" }, { "camera": null }]))]
pub struct BindCamera {
    /// The camera's id, as `input.state` lists it, or `null` to unbind.
    pub camera: Option<CameraId>,
    /// Used by undo; callers leave it unset. Starts the preview again after the change, as it ran before an unbind
    /// stopped it. Default `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume_preview: bool,
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
        // A resume needs a webcam source and a camera to start: refused before anything changes.
        if self.resume_preview {
            let state = host.state();
            if state.source != SourceKind::Webcam {
                return Err(op_error(InputError::NothingToCapture));
            }
            if self.camera.is_none() {
                return Err(op_error(InputError::NoCameraBound));
            }
        }
        // Checked first, so a camera that is not there is refused before anything changes.
        let before = host.bind_camera(self.camera.clone(), Mode::DryRun).map_err(op_error)?;
        let was_on = host.state().preview.is_on();
        let mut problems = Vec::new();
        if !ctx.dry_run {
            host.bind_camera(self.camera.clone(), Mode::Apply).map_err(op_error)?;
            if self.resume_preview
                && let Err(error) = host.preview(true, Mode::Apply)
            {
                match put_back_camera(host, before.as_ref(), was_on) {
                    Ok(()) => return Err(op_error(error)),
                    Err(left) => problems.push(format!("the preview did not start ({error}) and the change could not be taken back: {left}")),
                }
            }
        }
        let saved = match persist::<InputCamera>(ctx, &self.camera) {
            Ok(saved) => saved,
            Err(error) => {
                // The store refused: put the host back, so what runs is what is saved.
                match (ctx.dry_run, put_back_camera(host, before.as_ref(), was_on)) {
                    (true, _) | (false, Ok(())) => return Err(error),
                    (false, Err(left)) => {
                        problems.push(format!("{} and the change could not be taken back: {left}", error.message));
                        false
                    }
                }
            }
        };
        let mut answer = json!({ "camera": self.camera, "before": before, "saved": saved, "dry_run": ctx.dry_run });
        if !problems.is_empty() {
            answer["retained"] = retained(&problems);
        }
        let name = |camera: &Option<CameraId>| camera.as_ref().map_or_else(|| "none".to_owned(), |c| c.0.clone());
        let summary = format!("camera {} to {}", name(&before), name(&self.camera));
        let mut plan = changed(Self::meta().name, "camera", before, &self.camera, summary, answer)?;
        // Unbinding the camera a running preview reads stops the preview: undoing it brings the preview back too.
        if was_on
            && self.camera.is_none()
            && let Some(effect) = plan.effect.as_mut()
        {
            effect.undo.args["resume_preview"] = json!(true);
        }
        Ok(plan)
    }
}

omuzaki_ops_api::register_op!(BindCamera);
