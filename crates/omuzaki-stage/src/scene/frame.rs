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

/// The largest pitch `CameraDef` allows, in degrees; straight down (or up) uses `Camera::look_at`'s fallback basis.
const MAX_PITCH_DEG: f32 = 90.0;
/// How far in front of the nearest corner the distance search starts, in metres.
const NEAREST_GAP: f32 = 1e-3;
/// The distance search doubles its upper end at most this many times, then bisects this many times.
const SEARCH_STEPS: u32 = 64;
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
    /// The point that must land at `aim` in the frame: the span's middle, the eyes or the crown, on the box's centre
    /// column.
    anchor: Vec3,
    /// Where `anchor` lands, as a height in the frame from -1 (bottom) to 1 (top).
    aim: f32,
}

impl Shot {
    /// Refuses a span that is not finite or runs downward, which `Landmarks::measure` never gives but a hand-made
    /// `Landmarks` can.
    fn check(&self, framing: Framing) -> Result<(), StageError> {
        let finite = [self.lo, self.hi].iter().all(|v| v.is_finite())
            && self.anchor.is_finite()
            && self.bounds.min.is_finite()
            && self.bounds.max.is_finite();
        if !finite || self.hi < self.lo || self.bounds.max.cmplt(self.bounds.min).any() {
            return Err(StageError::Framing {
                reason: format!("the {framing:?} landmarks are not a finite box with its minimum below its maximum"),
            });
        }
        Ok(())
    }

    /// A shot whose `anchor` lies at height `y` on the box's centre column, landing at `from_top` of the frame height
    /// below its top edge.
    fn aimed(lo: f32, hi: f32, bounds: Bounds, fill: f32, y: f32, from_top: f32) -> Self {
        let centre = bounds.centre();
        Self { lo, hi, bounds, fill, anchor: Vec3::new(centre.x, y, centre.z), aim: 1.0 - 2.0 * from_top }
    }

    /// The shot of a span, its middle in the middle of the frame.
    fn centred(lo: f32, hi: f32, bounds: Bounds, fill: f32) -> Self {
        Self::aimed(lo, hi, bounds, fill, f32::midpoint(lo, hi), 0.5)
    }
}

/// The shot a preset asks of a character, or why the character can't give it.
fn shot(framing: Framing, l: &Landmarks) -> Result<Shot, StageError> {
    let missing = |what: &str| StageError::Framing { reason: format!("a {framing:?} shot needs {what}, which the character lacks") };
    match framing {
        Framing::Face => {
            let (head, eyes) = (l.head.ok_or_else(|| missing("a head"))?, l.eyes.ok_or_else(|| missing("eyes"))?);
            Ok(Shot::aimed(head.min.y, head.max.y, head, FACE_FILL, eyes, FACE_EYES_FROM_TOP))
        }
        Framing::Bust => {
            let chest = l.chest.ok_or_else(|| missing("a chest"))?;
            let crown = l.crown().ok_or_else(|| missing("a head"))?;
            let bust = l.bust.ok_or_else(|| missing("vertices at or above the chest"))?;
            Ok(Shot::aimed(chest, crown, bust, BUST_FILL, crown, BUST_CROWN_FROM_TOP))
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
    if !def.pitch.is_finite() || def.pitch.abs() > MAX_PITCH_DEG {
        return bad("pitch", def.pitch, &format!("degrees from -{MAX_PITCH_DEG} to {MAX_PITCH_DEG}"));
    }
    if !def.yaw.is_finite() {
        return bad("yaw", def.yaw, "a finite number of degrees");
    }
    if !def.shift.iter().all(|s| s.is_finite()) {
        return bad("shift", f32::NAN, "finite fractions of the frame");
    }
    Ok(())
}

/// The eight corners of the box from `min` to `max`.
fn corners(min: Vec3, max: Vec3) -> [Vec3; 8] {
    std::array::from_fn(|i| {
        Vec3::new(
            if i & 1 == 0 { min.x } else { max.x },
            if i & 2 == 0 { min.y } else { max.y },
            if i & 4 == 0 { min.z } else { max.z },
        )
    })
}

/// A box corner in the camera's frame, relative to the shot's anchor: across (`x`), up (`y`) and toward the camera
/// (`depth_toward`, so the corner is `d - depth_toward` in front of a camera `d` from the anchor).
struct Seen {
    x: f32,
    y: f32,
    toward: f32,
}

fn seen(points: &[Vec3; 8], anchor: Vec3, (right, up, back): (Vec3, Vec3, Vec3)) -> [Seen; 8] {
    std::array::from_fn(|i| {
        let rel = points[i] - anchor;
        Seen { x: rel.dot(right), y: rel.dot(up), toward: rel.dot(back) }
    })
}

/// The largest `toward` of `points`: the camera has to be farther than this from the anchor to see all of them.
fn nearest(points: &[Seen; 8]) -> f32 {
    points.iter().map(|p| p.toward).fold(0.0, f32::max)
}

/// The distance past `floor` at which `excess` (the projected extent over what is wanted, falling as the camera backs
/// off) reaches zero. `None` if even right in front of the box the extent is within what is wanted: it never binds.
fn distance_where_it_fits(floor: f32, excess: impl Fn(f32) -> f32) -> Option<f32> {
    let mut near = floor + NEAREST_GAP;
    let at_the_box = excess(near);
    // A NaN extent binds nothing here; the finished camera is checked at the end.
    if at_the_box.is_nan() || at_the_box <= 0.0 {
        return None;
    }
    let (mut step, mut far) = (near.max(1.0), near);
    for _ in 0..SEARCH_STEPS {
        far = near + step;
        if excess(far) <= 0.0 {
            break;
        }
        near = far;
        step *= 2.0;
    }
    for _ in 0..SEARCH_STEPS {
        let mid = f32::midpoint(near, far);
        if excess(mid) > 0.0 {
            near = mid;
        } else {
            far = mid;
        }
    }
    Some(far)
}

/// The perspective camera a preset framing gives on a character with `landmarks`, in the character's frame, for a
/// target of `aspect` (width / height).
///
/// The fit is done in the camera's own space, so pitch, yaw and the box's depth all count. The camera stands far
/// enough back that the eight corners of the preset's span, projected through the lens, fill their share of the frame
/// height, or the eight corners of its box their share of the frame width if that needs more distance; `zoom` then
/// divides the distance. The camera is turned so that the preset's anchor (the span's middle, the eyes or the crown)
/// lands where D5 puts it, and `shift` moves the subject within the frame afterwards.
///
/// # Errors
/// [`StageError::Framing`] for [`Framing::Free`], which has its own position. Also for a landmark the framing needs and
/// the character lacks: `face` without a head or eyes, `bust` without a chest or head, `hands` without hands. Also
/// for a non-finite or out-of-range input: `aspect <= 0`, `fov` outside (0, 180), `zoom <= 0`, `|pitch| > 90`; for
/// landmarks that aren't a finite, upright box or a character height that isn't finite and above zero; for a span
/// with no height and no width, which has no distance to frame it from; and for a camera that comes out non-finite or
/// with an unusable depth range.
pub fn frame(framing: Framing, landmarks: &Landmarks, def: &CameraDef, aspect: f32) -> Result<Camera, StageError> {
    check_inputs(def, aspect)?;
    let shot = shot(framing, landmarks)?;
    shot.check(framing)?;
    if !(landmarks.height_m.is_finite() && landmarks.height_m > 0.0) {
        return Err(StageError::Framing {
            reason: format!(
                "the character's height {} m is not usable: it sets the far plane, so give a finite number above zero",
                landmarks.height_m
            ),
        });
    }

    // The camera's axes depend only on where it looks from, not on how far, so take them from a camera at unit distance.
    let (sin_yaw, cos_yaw) = def.yaw.to_radians().sin_cos();
    let (sin_pitch, cos_pitch) = def.pitch.to_radians().sin_cos();
    let back = Vec3::new(sin_yaw * cos_pitch, sin_pitch, cos_yaw * cos_pitch);
    let lens = Projection::Perspective { fov_y_deg: def.fov, near: NEAR_FLOOR, far: 1.0 };
    let mut camera = Camera::look_at(back, Vec3::ZERO, Vec3::Y, lens);
    let axes = (camera.transform.x_axis.truncate(), camera.transform.y_axis.truncate(), camera.transform.z_axis.truncate());
    let (right, up, back) = axes;

    // What the frame sees at distance `d` from the anchor: NDC = across or up over `(d - toward) * t`.
    let t = (def.fov.to_radians() * 0.5).tan();
    let b = &shot.bounds;
    let span = seen(&corners(Vec3::new(b.min.x, shot.lo, b.min.z), Vec3::new(b.max.x, shot.hi, b.max.z)), shot.anchor, axes);
    let width = seen(&corners(b.min, b.max), shot.anchor, axes);
    let floor = nearest(&span).max(nearest(&width));
    let extent = |points: &[Seen; 8], value: fn(&Seen) -> f32, shift: f32, scale: f32, d: f32| {
        let projected = points.iter().map(|p| (value(p) + shift) / ((d - p.toward) * t * scale));
        let (low, high) = projected.fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
        high - low
    };
    // Taller: the anchor stays at `aim` as the camera backs off, so the whole span shifts with it.
    let tall = distance_where_it_fits(floor, |d| extent(&span, |p| p.y, shot.aim * d * t, 1.0, d) - 2.0 * shot.fill);
    let wide = distance_where_it_fits(floor, |d| extent(&width, |p| p.x, 0.0, aspect, d) - 2.0 * shot.fill);
    let fit = tall.unwrap_or(0.0).max(wide.unwrap_or(0.0));
    let d = fit / def.zoom;
    if !(d.is_finite() && d > 0.0) {
        return Err(StageError::Framing { reason: format!("the {framing:?} span has no extent to frame") });
    }

    // Looking at a point `aim * d * t` above the anchor, in the camera's up, puts the anchor at `aim` in the frame.
    let h = 2.0 * d * t;
    let target = shot.anchor - up * (shot.aim * d * t);
    let position = target + back * d;
    camera.transform.w_axis = position.extend(1.0);
    camera.projection = Projection::Perspective {
        fov_y_deg: def.fov,
        near: (NEAR_SHARE * d).max(NEAR_FLOOR),
        far: d + FAR_HEIGHTS * landmarks.height_m,
    };
    // Moving the eye and the target together keeps the orientation, so the shift is a shift of the transform.
    let offset = right * (-def.shift[0] * aspect * h) + up * (-def.shift[1] * h);
    camera.transform.w_axis += offset.extend(0.0);
    // Fail closed: landmarks that came from outside `Landmarks::measure` can still give a non-finite camera.
    camera.validate().map_err(|e| StageError::Framing { reason: format!("the {framing:?} camera isn't usable: {e}") })?;
    Ok(camera)
}
