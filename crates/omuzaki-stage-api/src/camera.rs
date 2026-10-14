//! Cameras (design D5): framing presets measured on the subject, plus free cameras. `cameras/<id>.toml`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::anchor::Anchor;

/// How a camera is placed.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Framing {
    /// Close-up: chin to crown fills the frame.
    Face,
    /// Medium: chest to crown.
    Bust,
    /// The whole character.
    #[default]
    Full,
    /// The hands' bounds (pose previews).
    Hands,
    /// An explicit `position` and `target`.
    Free,
}

fn subject() -> Anchor {
    Anchor::Subject
}

fn thirty() -> f32 {
    30.0
}

fn one() -> f32 {
    1.0
}

// `skip_serializing_if` hands the field by reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn no_shift(shift: &[f32; 2]) -> bool {
    *shift == [0.0, 0.0]
}

// `skip_serializing_if` hands the field by reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

/// One camera of a scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CameraDef {
    /// Which placement.
    #[serde(default)]
    pub framing: Framing,
    /// Whom a preset frames: `subject` or `member:<id>`.
    #[serde(default = "subject")]
    pub subject: Anchor,
    /// Degrees around the subject: 0 = in front, + = toward screen right. `-180..=180`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub yaw: f32,
    /// Degrees: + = from above. `-90..=90`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pitch: f32,
    /// Vertical field of view in degrees, in `(0, 180)`.
    #[serde(default = "thirty")]
    pub fov: f32,
    /// Above 1 moves in from the preset's distance, `> 0`.
    #[serde(default = "one")]
    pub zoom: f32,
    /// Composition offset, as fractions of the frame's width and height.
    #[serde(default, skip_serializing_if = "no_shift")]
    pub shift: [f32; 2],
    /// Free cameras only: what `position` and `target` are measured against. Default `stage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor: Option<Anchor>,
    /// Free cameras only: where the camera is, in the anchor's units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<[f32; 3]>,
    /// Free cameras only: what it looks at, in the anchor's units.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<[f32; 3]>,
}

impl CameraDef {
    /// A preset camera with every default (yaw and pitch 0, 30 degrees, no zoom or shift).
    #[must_use]
    pub fn preset(framing: Framing) -> Self {
        Self {
            framing,
            subject: Anchor::Subject,
            yaw: 0.0,
            pitch: 0.0,
            fov: 30.0,
            zoom: 1.0,
            shift: [0.0, 0.0],
            anchor: None,
            position: None,
            target: None,
        }
    }

    /// The anchor a free camera's `position` and `target` are measured against.
    #[must_use]
    pub fn free_anchor(&self) -> Anchor {
        self.anchor.clone().unwrap_or(Anchor::Stage)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn defaults_are_not_written() {
        let c: CameraDef = serde_json::from_value(json!({"framing": "bust"})).unwrap();
        assert_eq!(c, CameraDef::preset(Framing::Bust));
        assert_eq!(
            serde_json::to_value(&c).unwrap(),
            json!({"framing": "bust", "subject": "subject", "fov": 30.0, "zoom": 1.0})
        );
    }

    #[test]
    fn a_free_camera_keeps_its_pose() {
        let v = json!({"framing": "free", "subject": "subject", "fov": 40.0, "zoom": 1.0, "anchor": "stage",
            "position": [0.0, 1.0, 3.0], "target": [0.0, 0.5, 0.0]});
        let c: CameraDef = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(c.free_anchor(), Anchor::Stage);
        assert_eq!(serde_json::to_value(&c).unwrap(), v);
    }
}
