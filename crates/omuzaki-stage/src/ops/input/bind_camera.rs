//! `input.bind_camera`: bind the camera the webcam source reads. Only the person may: it is a device binding.

use omuzaki_ops_api::{Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_track_api::camera::CameraId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Held, held, host, op_error, person_only, persist, put_back_camera, settled};
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
            if host.state().source != SourceKind::Webcam {
                return Err(op_error(InputError::NothingToCapture));
            }
            if self.camera.is_none() {
                return Err(op_error(InputError::NoCameraBound));
            }
        }
        // Checked first, so a camera that is not there is refused before anything changes.
        host.bind_camera(self.camera.clone(), Mode::DryRun).map_err(op_error)?;
        let first = held(host);
        let mut problems = Vec::new();
        if !ctx.dry_run {
            if let Err(error) = host.bind_camera(self.camera.clone(), Mode::Apply) {
                // The host may have stopped the preview on the way (the old camera did not restart either).
                if held(host) == first {
                    return Err(op_error(error));
                }
                problems.push(format!("{error}; the old camera did not start again, so the preview is stopped"));
            } else if self.resume_preview
                && let Err(error) = host.preview(true, Mode::Apply)
            {
                match put_back_camera(host, first.camera.as_ref(), first.preview) {
                    Ok(()) => return Err(op_error(error)),
                    Err(left) => problems.push(format!("the preview did not start ({error}) and the change could not be taken back: {left}")),
                }
            }
        }
        // Save only a change that stands: what is saved is what runs.
        let mut saved = false;
        if problems.is_empty() {
            match persist::<InputCamera>(ctx, &self.camera) {
                Ok(done) => saved = done,
                Err(error) => match (ctx.dry_run, put_back_camera(host, first.camera.as_ref(), first.preview)) {
                    (true, _) | (false, Ok(())) => return Err(error),
                    (false, Err(left)) => {
                        problems.push(format!("{} and the change could not be taken back: {left}", error.message));
                    }
                },
            }
        }
        // What the host holds now is what the answer and the undo describe, whatever was asked for.
        let after = if ctx.dry_run {
            Held { camera: self.camera.clone(), preview: first.preview && self.camera.is_some(), ..first.clone() }
        } else {
            held(host)
        };
        let name = |camera: &Option<CameraId>| camera.as_ref().map_or_else(|| "none".to_owned(), |c| c.0.clone());
        let mut answer = json!({
            "camera": after.camera, "before": first.camera, "saved": saved, "dry_run": ctx.dry_run,
        });
        if after.camera != self.camera {
            answer["requested"] = json!(self.camera);
        }
        if !problems.is_empty() {
            answer["retained"] = json!(problems);
        }
        let summary = format!("camera {} to {}", name(&first.camera), name(&after.camera));
        settled(
            Self::meta().name,
            "camera",
            (json!(first.camera), json!(after.camera)),
            (&first, &after),
            summary,
            answer,
        )
    }
}

omuzaki_ops_api::register_op!(BindCamera);
