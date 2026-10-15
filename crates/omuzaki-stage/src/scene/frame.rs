//! The one framing function: a preset, the landmarks and an aspect ratio to a camera pose (design D5).
//!
//! Scene cameras, `render.preview`, library previews and the sheet's turnaround all frame through [`frame`], so a
//! preset means the same shot everywhere. Perspective only: `CameraDef` has no projection field.

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{Camera, Projection};
use omuzaki_stage_api::{CameraDef, Framing};

use super::landmarks::{Bounds, Landmarks};
use crate::error::StageError;

/// The share of the frame height a `face` shot's span (chin to crown) fills.
pub const FACE_FILL: f32 = 0.75;
/// The share of the frame height a `bust` shot's span (chest to crown) fills.
pub const BUST_FILL: f32 = 0.80;
/// The share of the frame height a `full` shot's span fills.
pub const FULL_FILL: f32 = 0.80;
/// The share of the frame height a `hands` shot's span fills.
pub const HANDS_FILL: f32 = 0.70;
/// The eyes sit this share of the frame height below its top edge in a face shot.
pub const FACE_EYES_FROM_TOP: f32 = 0.45;
/// The crown sits this share of the frame height below its top edge in a bust shot.
pub const BUST_CROWN_FROM_TOP: f32 = 0.08;

/// The pitch, in degrees, at and beyond which the camera would look straight along the up axis.
const MAX_PITCH_DEG: f32 = 89.9;
/// The near plane as a share of the camera distance, with a floor so a close shot keeps a usable depth range.
const NEAR_SHARE: f32 = 0.02;
const NEAR_FLOOR: f32 = 0.01;
/// The far plane lies this many character heights behind the subject.
const FAR_HEIGHTS: f32 = 10.0;

/// What a preset frames: the span it fills the frame with, the box whose width must fit, and where it aims.
struct Shot {
    lo: f32,
    hi: f32,
    bounds: Bounds,
    fill: f32,
    /// The height the aim hangs from: the span's middle, the eyes or the crown.
    anchor: f32,
    /// How far the aim lies above `anchor`, in frame heights.
    lift: f32,
}

impl Shot {
    fn centred(lo: f32, hi: f32, bounds: Bounds, fill: f32) -> Self {
        Self { lo, hi, bounds, fill, anchor: f32::midpoint(lo, hi), lift: 0.0 }
    }
}

/// The shot a preset asks of a character, or why the character can't give it.
fn shot(framing: Framing, l: &Landmarks) -> Result<Shot, StageError> {
    let missing = |what: &str| StageError::Framing { reason: format!("a {framing:?} shot needs {what}, which the character lacks") };
    match framing {
        Framing::Face => {
            let (head, eyes) = (l.head.ok_or_else(|| missing("a head"))?, l.eyes.ok_or_else(|| missing("eyes"))?);
            Ok(Shot {
                lo: head.min.y,
                hi: head.max.y,
                bounds: head,
                fill: FACE_FILL,
                anchor: eyes,
                lift: FACE_EYES_FROM_TOP - 0.5,
            })
        }
        Framing::Bust => {
            let chest = l.chest.ok_or_else(|| missing("a chest"))?;
            let crown = l.crown().ok_or_else(|| missing("a head"))?;
            let bust = l.bust.ok_or_else(|| missing("vertices at or above the chest"))?;
            Ok(Shot {
                lo: chest,
                hi: crown,
                bounds: bust,
                fill: BUST_FILL,
                anchor: crown,
                lift: BUST_CROWN_FROM_TOP - 0.5,
            })
        }
        Framing::Full => Ok(Shot::centred(l.bottom(), l.top(), l.body, FULL_FILL)),
        Framing::Hands => {
            let hands = l.hands.ok_or_else(|| missing("hands"))?;
            Ok(Shot::centred(hands.min.y, hands.max.y, hands, HANDS_FILL))
        }
        Framing::Free => Err(StageError::Framing {
            reason: "a free camera has its own position and target, so there is no preset to frame".to_owned(),
        }),
    }
}

/// Checks the numbers of `def` and `aspect` that the method divides by or takes a tangent of.
fn check_inputs(def: &CameraDef, aspect: f32) -> Result<(), StageError> {
    let bad = |what: &str, value: f32, want: &str| {
        Err(StageError::Framing { reason: format!("the camera's {what} {value} is not usable: give {want}") })
    };
    if !aspect.is_finite() || aspect <= 0.0 {
        return bad("aspect", aspect, "a finite width / height above zero");
    }
    if !def.fov.is_finite() || def.fov <= 0.0 || def.fov >= 180.0 {
        return bad("fov", def.fov, "degrees above 0 and below 180");
    }
    if !def.zoom.is_finite() || def.zoom <= 0.0 {
        return bad("zoom", def.zoom, "a finite number above zero");
    }
    if !def.pitch.is_finite() || def.pitch.abs() >= MAX_PITCH_DEG {
        return bad("pitch", def.pitch, &format!("degrees below {MAX_PITCH_DEG} either way"));
    }
    if !def.yaw.is_finite() {
        return bad("yaw", def.yaw, "a finite number of degrees");
    }
    if !def.shift.iter().all(|s| s.is_finite()) {
        return bad("shift", f32::NAN, "finite fractions of the frame");
    }
    Ok(())
}

/// The perspective camera a preset framing gives on a character with `landmarks`, in the character's frame, for a
/// target of `aspect` (width / height).
///
/// The camera stands far enough back that the preset's span fills its share of the frame height, or its box its share
/// of the frame width if that needs more distance; `zoom` then divides the distance. It looks at the box's centre
/// column, at the height the preset aims for, and `shift` moves the subject within the frame afterwards.
///
/// # Errors
/// [`StageError::Framing`] for [`Framing::Free`], which has its own position. Also for a landmark the framing needs and
/// the character lacks: `face` without a head or eyes, `bust` without a chest or head, `hands` without hands. Also
/// for a non-finite or out-of-range input: `aspect <= 0`, `fov` outside (0, 180), `zoom <= 0`, `|pitch| >= 89.9`; and
/// for a span with no height and no width, which has no distance to frame it from.
pub fn frame(framing: Framing, landmarks: &Landmarks, def: &CameraDef, aspect: f32) -> Result<Camera, StageError> {
    check_inputs(def, aspect)?;
    let shot = shot(framing, landmarks)?;

    let t = (def.fov.to_radians() * 0.5).tan();
    let d_v = (shot.hi - shot.lo) / (2.0 * shot.fill * t);
    let d_w = shot.bounds.width_at(def.yaw) / (2.0 * shot.fill * aspect * t);
    let d = d_v.max(d_w) / def.zoom;
    if !d.is_finite() || d <= 0.0 {
        return Err(StageError::Framing {
            reason: format!("the {framing:?} span is {d_v} m tall and {d_w} m wide in frame units: there is nothing to frame"),
        });
    }

    let h = 2.0 * d * t;
    let centre = shot.bounds.centre();
    let target = Vec3::new(centre.x, shot.anchor + shot.lift * h, centre.z);
    let (sin_yaw, cos_yaw) = def.yaw.to_radians().sin_cos();
    let (sin_pitch, cos_pitch) = def.pitch.to_radians().sin_cos();
    let position = target + d * Vec3::new(sin_yaw * cos_pitch, sin_pitch, cos_yaw * cos_pitch);

    let projection = Projection::Perspective {
        fov_y_deg: def.fov,
        near: (NEAR_SHARE * d).max(NEAR_FLOOR),
        far: d + FAR_HEIGHTS * landmarks.height_m,
    };
    let mut camera = Camera::look_at(position, target, Vec3::Y, projection);
    // Moving the eye and the target together keeps the orientation, so the shift is a shift of the transform.
    let (right, up) = (camera.transform.x_axis.truncate(), camera.transform.y_axis.truncate());
    let offset = right * (-def.shift[0] * aspect * h) + up * (-def.shift[1] * h);
    camera.transform.w_axis += offset.extend(0.0);
    Ok(camera)
}
