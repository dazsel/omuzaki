//! `input.open_video`: track from a video file instead of a camera (#2585).

use std::path::Path;

use omuzaki_ops_api::{Effect, Mode, Op, OpCall, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_track_api::camera::CameraId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{Held, held, host, mode, op_error, put_back_source, to_value};
use crate::perform::input::{InputHost, SourceKind};

/// Track the face, head and (with `input.track_body`) the body from a recorded video, a webcam clip or a phone video
/// of the person, as if it were a camera: it is bound as the webcam source's camera, and the preview (`input.preview`)
/// plays it at the file's own rate, looping, with calibration, the doctor and recording working as they do live.
/// `.y4m` is read directly; MP4, MKV, MOV, WebM and the rest through the person's own ffmpeg, on this machine only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{ "path": "/home/me/Videos/clip.mp4" }]))]
pub struct OpenVideo {
    /// The video's absolute path on this machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Used by undo; callers leave it unset. Puts back the source and camera held before a file was opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<Restore>,
    /// Used by undo; callers leave it unset. Starts the preview again after the change, as it ran before. Default
    /// `false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resume_preview: bool,
}

/// What `input.open_video` replaced, for its undo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Restore {
    /// The source selected before.
    pub source: SourceKind,
    /// The camera bound before, or `null`.
    pub camera: Option<CameraId>,
}

impl Op for OpenVideo {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.open_video",
            summary: "Track from a video file (path) as if it were the webcam; the preview plays it at its own rate.",
            kind: OpKind::Edit,
            scope: Scope::EditSettings,
            undo: Undo::Explicit,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        ctx.require_person("tracking from a video file reads a recording of the person")?;
        let host = host(ctx)?;
        let before = held(host);
        let (camera, after_source, info) = match (&self.path, &self.restore) {
            (Some(path), None) => {
                let path = Path::new(path.trim());
                if !path.is_absolute() {
                    return Err(OpError::invalid("path", format!("'{}' is not an absolute path", path.display()))
                        .hint("give the video's full path, starting from the root of the drive"));
                }
                let info = host.add_video(path, mode(ctx)).map_err(op_error)?;
                (info.as_ref().map(|i| i.id.clone()), SourceKind::Webcam, info)
            }
            (None, Some(restore)) => (restore.camera.clone(), restore.source, None),
            _ => {
                return Err(OpError::invalid("path", "give a video's path (and leave `restore` unset)"));
            }
        };
        let answer_camera = info.as_ref().map(|i| {
            let format = i.formats.first();
            json!({
                "id": i.id,
                "name": i.name,
                "width": format.map(|f| f.width),
                "height": format.map(|f| f.height),
                "fps": format.map(|f| f.fps),
            })
        });
        if ctx.dry_run {
            let answer = json!({ "camera": answer_camera, "source": after_source, "dry_run": true });
            return Ok(Plan::answer(answer));
        }
        apply(host, &before, after_source, camera.as_ref(), self.resume_preview)?;
        let after = held(host);
        let restore = Restore { source: before.source, camera: before.camera.clone() };
        let answer = json!({
            "camera": answer_camera,
            "source": after.source,
            "before": to_value(&restore)?,
            "preview": after.preview,
            "dry_run": false,
        });
        let changed = (before.source, &before.camera) != (after.source, &after.camera);
        let effect = (changed && self.path.is_some())
            .then(|| -> Result<Effect, OpError> {
                let call = |args: &OpenVideo| -> Result<OpCall, OpError> {
                    Ok(OpCall { op: Self::meta().name.to_owned(), args: to_value(args)? })
                };
                let undo = OpenVideo {
                    path: None,
                    restore: Some(restore),
                    resume_preview: before.preview && !after.preview,
                };
                let name = info.as_ref().map_or("a video", |i| i.name.as_str());
                Ok(Effect { summary: format!("tracking from {name}"), undo: call(&undo)?, redo: call(self)? })
            })
            .transpose()?;
        Ok(Plan { effect, ..Plan::answer(answer) })
    }
}

/// Selects `source` and binds `camera`; on a failure, puts the source back as it was and returns the error.
fn apply(
    host: &dyn InputHost,
    before: &Held,
    source: SourceKind,
    camera: Option<&CameraId>,
    resume_preview: bool,
) -> Result<(), OpError> {
    host.select(source, Mode::Apply).map_err(op_error)?;
    let bound = host.bind_camera(camera.cloned(), Mode::Apply);
    let resumed = bound.and_then(|_| if resume_preview { host.preview(true, Mode::Apply).map(drop) } else { Ok(()) });
    if let Err(error) = resumed {
        let error = op_error(error);
        return match put_back_source(host, before.source, false)
            .and_then(|()| super::put_back_camera(host, before.camera.as_ref(), before.preview))
        {
            Ok(()) => Err(error),
            Err(left) => Err(error.hint(format!("the change could not be taken back: {left}"))),
        };
    }
    Ok(())
}

omuzaki_ops_api::register_op!(OpenVideo);
