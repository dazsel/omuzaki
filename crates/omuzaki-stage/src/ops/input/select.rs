//! `input.select`: choose the kind of source that drives the face. Starts no capture.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{changed, host, mode, op_error, persist, put_back_source, retained};
use crate::perform::input::{InputError, InputSource, SourceKind};

/// Choose what drives the face: `webcam`, `iphone` or `life_layer` (no capture; the life layer moves the face alone).
/// This only picks the kind: it binds no camera and starts no capture, and a running preview of another kind stops.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{ "source": "webcam" }, { "source": "life_layer" }]))]
pub struct Select {
    /// `webcam`, `iphone` or `life_layer`.
    pub source: SourceKind,
    /// Used by undo; callers leave it unset. Starts the preview again after the change, as it ran before a change of
    /// source stopped it. Default `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume_preview: bool,
}

impl Op for Select {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.select",
            summary: "Choose what drives the face (webcam, iphone or life_layer); starts no capture.",
            kind: OpKind::Edit,
            scope: Scope::EditSettings,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.require_person("choosing the tracking source changes what the camera is used for")?;
        let host = host(ctx)?;
        // A resume needs something to capture from the requested source: refused before anything changes, in a dry
        // run too.
        if self.resume_preview {
            match self.source {
                SourceKind::LifeLayer => return Err(op_error(InputError::NothingToCapture)),
                SourceKind::Iphone => return Err(op_error(InputError::Unsupported(SourceKind::Iphone))),
                SourceKind::Webcam => {
                    let state = host.state();
                    match state.camera {
                        None => return Err(op_error(InputError::NoCameraBound)),
                        Some(camera) if !state.camera_present => return Err(op_error(InputError::no_camera(camera))),
                        Some(_) => {}
                    }
                }
            }
        }
        let was_on = host.state().preview.is_on();
        let before = host.select(self.source, mode(ctx)).map_err(op_error)?;
        let mut problems = Vec::new();
        if self.resume_preview
            && !ctx.dry_run
            && let Err(error) = host.preview(true, mode(ctx))
        {
            match put_back_source(host, before, was_on) {
                Ok(()) => return Err(op_error(error)),
                Err(left) => problems.push(format!("the preview did not start ({error}) and the change could not be taken back: {left}")),
            }
        }
        let saved = match persist::<InputSource>(ctx, &self.source) {
            Ok(saved) => saved,
            Err(error) => {
                // The store refused: put the host back, so what runs is what is saved.
                match (ctx.dry_run, put_back_source(host, before, was_on)) {
                    (true, _) | (false, Ok(())) => return Err(error),
                    (false, Err(left)) => {
                        problems.push(format!("{} and the change could not be taken back: {left}", error.message));
                        false
                    }
                }
            }
        };
        let mut answer = json!({ "source": self.source, "before": before, "saved": saved, "dry_run": ctx.dry_run });
        if !problems.is_empty() {
            answer["retained"] = retained(&problems);
        }
        let summary = format!("tracking source {before:?} to {:?}", self.source);
        let mut plan = changed(Self::meta().name, "source", before, self.source, summary, answer)?;
        // A change of source stops a running preview: undoing it brings the preview back too.
        if was_on
            && before != self.source
            && let Some(effect) = plan.effect.as_mut()
        {
            effect.undo.args["resume_preview"] = json!(true);
        }
        Ok(plan)
    }
}

omuzaki_ops_api::register_op!(Select);
