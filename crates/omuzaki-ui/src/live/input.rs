//! The `input.*` op surface the Camera section drives: the names, the arguments and the answer of `input.state`, as
//! the stage's input ops declare them (PR #2210, `omuzaki-stage::ops::input`).
//!
//! Those ops are not on `main` yet, so this module mirrors their surface, field for field, and nothing else: the panel
//! calls them only when the runner lists them ([`Ops`](super::model::Ops)), and until then the Camera section is
//! disabled with "not available yet". When they merge, [`InputView`] gives way to the stage's own `InputState`.
//!
//! | Op | Arguments | Who |
//! |---|---|---|
//! | `input.state` | `{}` | anyone (a query) |
//! | `input.select` | `{source: webcam \| iphone \| life_layer}` | the person confirms |
//! | `input.bind_camera` | `{camera: id \| null}` | the person only |
//! | `input.preview` | `{on: bool}` | the person starts; anyone stops |
//! | `input.request_camera` | `{}` | the person only |

use omuzaki_ops_api::{OpInfo, OpKind, OpMeta, Scope, Undo};
use omuzaki_stage::perform::Tracking;
use serde::{Deserialize, Serialize};

/// `input.state`: the source, the bound camera, the preview, the permission, the cameras and tracking.
pub const OP_STATE: &str = "input.state";
/// `input.select`: choose what drives the face.
pub const OP_SELECT: &str = "input.select";
/// `input.bind_camera`: bind the camera the webcam source reads.
pub const OP_BIND_CAMERA: &str = "input.bind_camera";
/// `input.preview`: start or stop the capture that drives the face.
pub const OP_PREVIEW: &str = "input.preview";
/// `input.request_camera`: show the OS camera permission prompt.
pub const OP_REQUEST_CAMERA: &str = "input.request_camera";

/// What drives the face (`input.select`'s `source`).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A camera on this computer, through the face tracker.
    Webcam,
    /// An iPhone's face tracking over the network.
    Iphone,
    /// No capture: the life layer moves the face alone.
    #[default]
    LifeLayer,
}

/// Whether the app may use the camera, as the OS says.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// Not asked yet, or the OS does not say until a camera opens.
    #[default]
    Unknown,
    /// The app may use the camera.
    Granted,
    /// The person refused; only they can change it, in the OS settings.
    Denied,
}

/// Whether the capture that drives the face runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Preview {
    /// No capture.
    #[default]
    Off,
    /// The tracker runs on `camera`.
    On {
        /// The camera it reads.
        camera: String,
    },
}

/// A camera the tracker can read.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Camera {
    /// What `input.bind_camera` takes.
    pub id: String,
    /// The name the OS shows.
    pub name: String,
    /// Whether it is the bound camera.
    pub bound: bool,
}

/// `input.state`'s answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct InputView {
    /// What drives the face.
    pub source: Source,
    /// The bound camera's id, if one is bound.
    pub camera: Option<String>,
    /// Whether the bound camera is plugged in now.
    pub camera_present: bool,
    /// Whether the capture runs.
    pub preview: Preview,
    /// The camera permission.
    pub permission: Permission,
    /// The cameras there are now.
    pub cameras: Vec<Camera>,
    /// Whether the performer sees a face.
    pub tracking: Tracking,
}

impl InputView {
    /// The bound camera as listed, if it is plugged in.
    pub fn bound(&self) -> Option<&Camera> {
        let id = self.camera.as_deref()?;
        self.cameras.iter().find(|camera| camera.id == id)
    }

    /// Whether the capture runs.
    pub fn capturing(&self) -> bool {
        matches!(self.preview, Preview::On { .. })
    }
}

/// The five ops as a runner lists them, with the kind, scope and undo PR #2210 declares: for a runner that stands in
/// for the surface until it merges (the gallery story, the tests). The schemas are open objects; the real ops carry
/// their own.
pub fn op_infos() -> Vec<OpInfo> {
    let info = |name, summary, kind, scope, undo| {
        OpInfo::new(OpMeta { name, summary, kind, scope, undo }, schemars::json_schema!({ "type": "object" }))
    };
    vec![
        info(OP_STATE, "Read the tracking input.", OpKind::Query, Scope::Read, Undo::None),
        info(OP_SELECT, "Choose what drives the face.", OpKind::Edit, Scope::EditSettings, Undo::Explicit),
        info(OP_BIND_CAMERA, "Bind the webcam source's camera.", OpKind::Edit, Scope::EditSettings, Undo::Explicit),
        info(OP_PREVIEW, "Start or stop the live camera preview.", OpKind::Edit, Scope::Perform, Undo::Explicit),
        info(OP_REQUEST_CAMERA, "Show the OS camera prompt.", OpKind::Edit, Scope::Perform, Undo::Explicit),
    ]
}
