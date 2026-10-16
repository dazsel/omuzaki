//! `input.select`: choose the kind of source that drives the face. Starts no capture.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{changed, host, mode, op_error, persist};
use crate::perform::input::{InputSource, SourceKind};

/// Choose what drives the face: `webcam`, `iphone` or `life_layer` (no capture; the life layer moves the face alone).
/// This only picks the kind: it binds no camera and starts no capture, and a running preview of another kind stops.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Select {
    /// `webcam`, `iphone` or `life_layer`.
    pub source: SourceKind,
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
        let before = host.select(self.source, mode(ctx)).map_err(op_error)?;
        let saved = persist::<InputSource>(ctx, &self.source)?;
        let answer = json!({ "source": self.source, "before": before, "saved": saved, "dry_run": ctx.dry_run });
        let summary = format!("tracking source {before:?} to {:?}", self.source);
        changed(Self::meta().name, "source", before, self.source, summary, answer)
    }
}

omuzaki_ops_api::register_op!(Select);
