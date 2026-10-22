//! `input.select`: choose the kind of source that drives the face. Starts no capture.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Held, held, host, mode, op_error, persist, put_back_source, settled};
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
                // The phone link has no binding to check here; starting it reports a missing link or a busy port.
                SourceKind::Iphone => {}
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
        let first = held(host);
        host.select(self.source, mode(ctx)).map_err(op_error)?;
        let mut problems = Vec::new();
        if self.resume_preview
            && !ctx.dry_run
            && let Err(error) = host.preview(true, mode(ctx))
        {
            match put_back_source(host, first.source, first.preview) {
                Ok(()) => return Err(op_error(error)),
                Err(left) => problems.push(format!("the preview did not start ({error}) and the change could not be taken back: {left}")),
            }
        }
        // Save only a change that stands: what is saved is what runs.
        let mut saved = false;
        if problems.is_empty() {
            match persist::<InputSource>(ctx, &self.source) {
                Ok(done) => saved = done,
                Err(error) => match (ctx.dry_run, put_back_source(host, first.source, first.preview)) {
                    (true, _) | (false, Ok(())) => return Err(error),
                    (false, Err(left)) => {
                        problems.push(format!("{} and the change could not be taken back: {left}", error.message));
                    }
                },
            }
        }
        // What the host holds now is what the answer and the undo describe, whatever was asked for.
        let after = if ctx.dry_run {
            Held { source: self.source, preview: first.preview && self.source == first.source, ..first.clone() }
        } else {
            held(host)
        };
        let mut answer = json!({ "source": after.source, "before": first.source, "saved": saved, "dry_run": ctx.dry_run });
        if after.source != self.source {
            answer["requested"] = json!(self.source);
        }
        if !problems.is_empty() {
            answer["retained"] = json!(problems);
        }
        let summary = format!("tracking source {:?} to {:?}", first.source, after.source);
        settled(
            Self::meta().name,
            "source",
            (json!(first.source), json!(after.source)),
            (&first, &after),
            summary,
            answer,
        )
    }
}

omuzaki_ops_api::register_op!(Select);
