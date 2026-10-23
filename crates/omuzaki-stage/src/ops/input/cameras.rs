//! `input.cameras`: the cameras the OS reports, for a picker or an agent choosing one. Listing opens none.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_track_api::camera::CameraId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{host, to_value};
use crate::perform::input::PreviewState;

/// List the cameras the computer reports: each one's id (what `input.bind_camera` takes), its name, whether it is the
/// bound camera and whether Omuzaki is reading it now. Opens no camera.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Cameras {}

/// One camera in the answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CameraRow {
    /// What `input.bind_camera` takes.
    pub id: CameraId,
    /// The name the OS shows.
    pub name: String,
    /// Whether it is the bound camera.
    pub bound: bool,
    /// Whether Omuzaki's capture reads it now.
    pub in_use: bool,
}

impl Op for Cameras {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.cameras",
            summary: "List the cameras the computer reports, without opening any.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let state = host(ctx)?.state();
        let reading = match &state.preview {
            PreviewState::On { camera } => Some(camera),
            PreviewState::Off => None,
        };
        let rows: Vec<CameraRow> = state
            .cameras
            .iter()
            .map(|c| CameraRow {
                id: c.id.clone(),
                name: c.name.clone(),
                bound: c.bound,
                in_use: reading == Some(&c.id),
            })
            .collect();
        Ok(Plan::answer(to_value(serde_json::json!({ "cameras": rows }))?))
    }
}

omuzaki_ops_api::register_op!(Cameras);
