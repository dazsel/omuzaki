//! The numbers a face shape answers with: the head's front silhouette (`head::presets::measure`) and the layout's
//! CB-12 ratios.

use omuzaki_char_api::{HeadParams, ShapeProfile};
use serde_json::{Value, json};

use crate::head::mesh::{Resolution, build};
use crate::head::presets::measure::Silhouette;

/// Rounds a ratio to four places, so answers read cleanly and compare across runs.
fn r4(x: f64) -> f64 {
    (x * 1e4).round() / 1e4
}

/// The silhouette of the head `params` builds, with `profile` in place of its outline, measured from the layout's eye
/// line; `null` when the head doesn't build.
pub(super) fn silhouette(params: &HeadParams, profile: &ShapeProfile) -> Value {
    let mut head = params.clone();
    head.outline = profile.outline;
    let Ok(mesh) = build(&head, Resolution::LIVE, None) else {
        return Value::Null;
    };
    let Some(shape) = Silhouette::of(&mesh, head.cranium.centre[1]) else {
        return Value::Null;
    };
    let m = shape.metrics(profile.layout.eye_y);
    json!({
        "width_to_height": r4(m.width_to_height()),
        "face_length_to_width": r4(m.face_length_to_width()),
        "eye_line_fraction": r4(m.eye_line_fraction()),
        "mid_face_fullness": r4(m.mid_face_fullness),
        "chin_angle_deg": (m.chin_angle_deg * 10.0).round() / 10.0,
        "jaw_width_mm": (m.jaw_width * 1e4).round() / 10.0,
    })
}

/// The layout's CB-12 ratios against the profile's own chin; `null` when the chin is not below the eye line.
pub(super) fn ratios(profile: &ShapeProfile) -> Value {
    profile.layout.ratios(profile.outline.chin_y()).map_or(Value::Null, |r| {
        json!({
            "eye_width_to_iod": r4(r.eye_width_to_iod),
            "nose": r4(r.nose),
            "mouth": r4(r.mouth),
            "drop_to_iod": r4(r.drop_to_iod),
        })
    })
}

/// Both, as one object.
pub(super) fn numbers(params: &HeadParams, profile: &ShapeProfile) -> Value {
    json!({ "silhouette": silhouette(params, profile), "ratios": ratios(profile) })
}
