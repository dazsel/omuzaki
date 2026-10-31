//! A scene's cameras (design D5, D8): `stage.add_camera`, `stage.set_camera`, `stage.remove_camera` and
//! `stage.set_active_camera`.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stage_api::{Anchor, CameraDef, CameraId, Framing, MAX_CAMERAS, SceneId};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::edit::{SceneEdit, free_id, unknown};

fn framing_name(framing: Framing) -> &'static str {
    match framing {
        Framing::Face => "face",
        Framing::Bust => "bust",
        Framing::Full => "full",
        Framing::Hands => "hands",
        Framing::Free => "free",
    }
}

/// The refusal for a free camera without its pose.
fn needs_pose(missing: &str) -> OpError {
    OpError::invalid(missing, format!("a free camera needs `position` and `target`, and `{missing}` is missing"))
        .hint("send the pose the viewport shows (position and target), or pick a preset framing")
}

/// The opening camera after `removed` leaves: unchanged unless it was the one, else the first remaining by id.
fn opening_after(current: Option<&CameraId>, removed: &CameraId, remaining: &[CameraId]) -> Option<CameraId> {
    match current {
        Some(c) if c == removed => remaining.first().cloned(),
        other => other.cloned(),
    }
}

/// Adds a camera to a scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {},
        {"id": "low", "framing": "full", "yaw": -25.0, "pitch": -8.0, "fov": 35.0},
        {"id": "dutch", "framing": "face", "roll": 10.0, "zoom": 1.2},
        {"scene": "night_stream", "framing": "free", "position": [0.0, 1.4, 2.5], "target": [0.0, 1.2, 0.0]}
    ],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct AddCamera {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// Its id; the framing's name (then `-2`, `-3`...) when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<CameraId>,
    /// `face`, `bust` (the default), `full`, `hands`, or `free` (which needs `position` and `target`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framing: Option<Framing>,
    /// Whom a preset frames: `subject` (the default) or `member:<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Anchor>,
    /// Degrees around the subject, -180 to 180: 0 is in front, more is toward screen right.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    /// Degrees, -90 to 90: more is from above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f32>,
    /// Degrees, -180 to 180, the picture turns about the lens (a Dutch angle): more turns it clockwise. 0 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roll: Option<f32>,
    /// Vertical field of view, degrees, between 0 and 180; 30 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fov: Option<f32>,
    /// Above 1 moves in from the preset's distance; 1 when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<f32>,
    /// Composition offset, as fractions of the frame's width and height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift: Option<[f32; 2]>,
    /// A free camera's frame of reference: `stage` (the default), `subject` or `member:<id>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// A free camera's position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// What a free camera looks at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<[f32; 3]>,
}

impl Op for AddCamera {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.add_camera",
            summary: "Add a camera to a scene: a preset framing (face, bust, full, hands) measured on the subject, or a \
                      free camera with a position and target.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let cameras = &mut edit.def.cameras;
        if cameras.len() >= MAX_CAMERAS {
            return Err(OpError::invalid(
                "framing",
                format!("the scene has {} cameras, the most a scene can have", cameras.len()),
            )
            .hint("remove one with stage.remove_camera, or change one with stage.set_camera"));
        }
        let framing = self.framing.unwrap_or(Framing::Bust);
        let free = framing == Framing::Free;
        if free {
            if self.position.is_none() {
                return Err(needs_pose("position"));
            }
            if self.target.is_none() {
                return Err(needs_pose("target"));
            }
        } else if let Some(field) = [
            ("anchor", self.anchor.is_some()),
            ("position", self.position.is_some()),
            ("target", self.target.is_some()),
        ]
        .into_iter()
        .find_map(|(field, given)| given.then_some(field))
        {
            return Err(OpError::invalid(field, format!("only a free camera takes `{field}`"))
                .hint("set framing to `free` to place the camera by position and target"));
        }
        let id = match &self.id {
            Some(id) if cameras.contains_key(id) => {
                return Err(OpError::conflict(format!("the scene already has a camera '{id}'"))
                    .hint("pick another id, or change that camera with stage.set_camera"));
            }
            Some(id) => id.clone(),
            None => {
                let free = free_id(framing_name(framing), &|s| cameras.keys().any(|k| k.as_str() == s));
                CameraId::new(&free).map_err(|e| OpError::internal(format!("camera id '{free}': {e}")))?
            }
        };
        let mut camera = CameraDef::preset(framing);
        if let Some(subject) = &self.subject {
            camera.subject = subject.clone();
        }
        camera.yaw = self.yaw.unwrap_or(camera.yaw);
        camera.pitch = self.pitch.unwrap_or(camera.pitch);
        camera.roll = self.roll.unwrap_or(camera.roll);
        camera.fov = self.fov.unwrap_or(camera.fov);
        camera.zoom = self.zoom.unwrap_or(camera.zoom);
        camera.shift = self.shift.unwrap_or(camera.shift);
        camera.anchor.clone_from(&self.anchor);
        camera.position = self.position;
        camera.target = self.target;
        cameras.insert(id.clone(), camera);
        let output = json!({ "scene": edit.id.as_str(), "camera": id.as_str() });
        edit.finish(ctx, output)
    }
}

register_op!(AddCamera);

/// Changes a camera of a scene: only the fields given.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"camera": "full", "yaw": 45.0},
        {"camera": "bust", "roll": -12.0},
        {"camera": "bust", "framing": "free", "position": [0.4, 1.5, 1.8], "target": [0.0, 1.35, 0.0]}
    ],
    "x-omuzaki-fixture" = "scenes"
))]
pub struct SetCamera {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The camera.
    pub camera: CameraId,
    /// Its framing; `free` needs a `position` and `target` (given here, or kept from before). Moving to a preset drops
    /// the free pose.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub framing: Option<Framing>,
    /// Whom a preset frames.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Anchor>,
    /// Degrees around the subject.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yaw: Option<f32>,
    /// Degrees from above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f32>,
    /// Degrees the picture turns about the lens (a Dutch angle): more turns it clockwise; 0 levels it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roll: Option<f32>,
    /// Vertical field of view, degrees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fov: Option<f32>,
    /// Above 1 moves in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<f32>,
    /// Composition offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shift: Option<[f32; 2]>,
    /// A free camera's frame of reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// A free camera's position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// What a free camera looks at.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<[f32; 3]>,
}

impl Op for SetCamera {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_camera",
            summary: "Change a camera of a scene: framing, subject, yaw, pitch, roll, fov, zoom, shift, or a free camera's \
                      position and target. Only the fields given change.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let cameras = &mut edit.def.cameras;
        let known: Vec<String> = cameras.keys().map(ToString::to_string).collect();
        let camera =
            cameras.get_mut(&self.camera).ok_or_else(|| unknown("camera", "camera", self.camera.as_str(), known))?;
        if let Some(framing) = self.framing {
            camera.framing = framing;
        }
        let free = camera.framing == Framing::Free;
        if free {
            camera.anchor = self.anchor.clone().or_else(|| camera.anchor.take());
            camera.position = self.position.or(camera.position);
            camera.target = self.target.or(camera.target);
            if camera.position.is_none() {
                return Err(needs_pose("position"));
            }
            if camera.target.is_none() {
                return Err(needs_pose("target"));
            }
        } else {
            if let Some(field) = [
                ("anchor", self.anchor.is_some()),
                ("position", self.position.is_some()),
                ("target", self.target.is_some()),
            ]
            .into_iter()
            .find_map(|(field, given)| given.then_some(field))
            {
                return Err(OpError::invalid(field, format!("'{}' isn't a free camera, so it has no `{field}`", self.camera))
                    .hint("set framing to `free` with it to place the camera by position and target"));
            }
            camera.anchor = None;
            camera.position = None;
            camera.target = None;
        }
        if let Some(subject) = &self.subject {
            camera.subject = subject.clone();
        }
        camera.yaw = self.yaw.unwrap_or(camera.yaw);
        camera.pitch = self.pitch.unwrap_or(camera.pitch);
        camera.roll = self.roll.unwrap_or(camera.roll);
        camera.fov = self.fov.unwrap_or(camera.fov);
        camera.zoom = self.zoom.unwrap_or(camera.zoom);
        camera.shift = self.shift.unwrap_or(camera.shift);
        let output = json!({ "scene": edit.id.as_str(), "camera": self.camera.as_str() });
        edit.finish(ctx, output)
    }
}

register_op!(SetCamera);

/// Removes a camera from a scene. If the scene opened with it, the first remaining camera becomes the opening one in
/// the same step.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"camera": "face"}, {"camera": "full"}], "x-omuzaki-fixture" = "scenes"))]
pub struct RemoveCamera {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The camera.
    pub camera: CameraId,
}

impl Op for RemoveCamera {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.remove_camera",
            summary: "Remove a camera from a scene; if the scene opened with it, the first remaining camera becomes the \
                      opening one.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        let known: Vec<String> = edit.def.cameras.keys().map(ToString::to_string).collect();
        edit.def
            .cameras
            .remove(&self.camera)
            .ok_or_else(|| unknown("camera", "camera", self.camera.as_str(), known))?;
        let remaining: Vec<CameraId> = edit.def.cameras.keys().cloned().collect();
        let opening = opening_after(edit.def.scene.camera.as_ref(), &self.camera, &remaining);
        edit.def.scene.camera.clone_from(&opening);
        let output = json!({ "scene": edit.id.as_str(), "removed": self.camera.as_str(), "opening_camera": opening });
        edit.finish(ctx, output)
    }
}

register_op!(RemoveCamera);

/// Picks the camera a scene opens with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"camera": "bust"}], "x-omuzaki-fixture" = "scenes"))]
pub struct SetActiveCamera {
    /// The scene; the one the project opens in when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// The camera.
    pub camera: CameraId,
}

impl Op for SetActiveCamera {
    fn meta() -> OpMeta {
        OpMeta {
            name: "stage.set_active_camera",
            summary: "Pick the camera a scene opens with (what the viewport, previews and outputs show first).",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut edit = SceneEdit::open(ctx.doc, self.scene.as_ref())?;
        if !edit.def.cameras.contains_key(&self.camera) {
            let known = edit.def.cameras.keys().map(ToString::to_string);
            return Err(unknown("camera", "camera", self.camera.as_str(), known));
        }
        edit.def.scene.camera = Some(self.camera.clone());
        let output = json!({ "scene": edit.id.as_str(), "camera": self.camera.as_str() });
        edit.finish(ctx, output)
    }
}

register_op!(SetActiveCamera);
