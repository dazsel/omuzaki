//! The one framing function: a preset, the landmarks and an aspect ratio to a camera pose (design D5).
//!
//! Scene cameras, `render.preview`, library previews and the sheet's turnaround all frame through [`frame`], so a
//! preset means the same shot everywhere. Perspective only: `CameraDef` has no projection field.

use omuzaki_core::glam::{Mat4, Vec3, Vec4};
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
/// Where a face shot takes the eye line of a body without eye bones: this share of the head's height above the chin.
pub const EYES_WITHOUT_BONES: f32 = 0.45;
/// The crown sits this share of the frame height below its top edge in a bust shot.
pub const BUST_CROWN_FROM_TOP: f32 = 0.08;

/// The ranges `CameraDef` documents and `SceneDef::check` enforces, in degrees; `tests/scene_framing.rs` checks that
/// `frame` accepts exactly what the check does, so the two can't drift apart.
const MAX_YAW_DEG: f32 = 180.0;
const MAX_PITCH_DEG: f32 = 90.0;
/// How far in front of the nearest corner the distance search starts, in metres.
const NEAREST_GAP: f32 = 1e-3;
/// Once the distance is bracketed, bisect this many times: far more than f32 has bits, so it has converged.
const BISECTIONS: u32 = 64;
/// The near plane lies this share of the way from the camera to the nearest framed corner, so no corner is cut.
const NEAR_MARGIN: f32 = 0.5;
/// When the camera is zoomed into the subject and no corner is in front of it, the near plane is this share of the
/// camera distance (and never below `NEAR_FLOOR`).
const NEAR_SHARE: f32 = 0.02;
const NEAR_FLOOR: f32 = 0.01;
/// How far outside the frame, in frame units, a corner of a finished shot may land (rounding).
const FRAME_TOLERANCE: f32 = 1e-4;
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
            let head = l.head.ok_or_else(|| missing("a head"))?;
            // A body without eye bones (a VRM may have none) still has a face shot: its eye line is taken at
            // `EYES_WITHOUT_BONES` of the head's height, where an anime face's eyes sit.
            let eyes = l.eyes.unwrap_or(head.min.y + EYES_WITHOUT_BONES * head.height());
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
    if !def.yaw.is_finite() || def.yaw.abs() > MAX_YAW_DEG {
        return bad("yaw", def.yaw, &format!("degrees from -{MAX_YAW_DEG} to {MAX_YAW_DEG}"));
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

/// The lens of a shot: `t` is the tangent of half the field of view, `scale` the frame's size along the axis measured
/// relative to its height (the aspect for across, 1 for up).
#[derive(Clone, Copy)]
struct Lens {
    t: f32,
    scale: f32,
}

impl Lens {
    /// The size of a half-frame at `toward` in front of the camera, which is `d` from the anchor.
    fn half(self, d: f32, toward: f32) -> f32 {
        (d - toward) * self.t * self.scale
    }
}

/// The projected extent of `points` along `value`'s axis, in frame units (the frame is 2 across), when the camera
/// looks `slide` along that axis from the anchor at distance `d`.
fn extent(points: &[Seen; 8], value: fn(&Seen) -> f32, lens: Lens, d: f32, slide: f32) -> f32 {
    let projected = points.iter().map(|p| (value(p) - slide) / lens.half(d, p.toward));
    let (low, high) = projected.fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| (lo.min(v), hi.max(v)));
    high - low
}

/// How far along `value`'s axis the camera looks from the anchor: `wanted` (what puts the anchor where the preset
/// aims), unless that pushes a projected corner past the frame's edge. Then it is the nearest slide that keeps every
/// corner inside, so no part of the span is cut off by the aim. `None` if no slide keeps every corner inside at this
/// distance: the camera has to back off.
fn slide_into_frame(points: &[Seen; 8], value: fn(&Seen) -> f32, lens: Lens, d: f32, wanted: f32) -> Option<f32> {
    // A corner is inside when `-1 <= (value - slide) / half <= 1`, that is `value - half <= slide <= value + half`.
    let (mut lowest, mut highest) = (f32::NEG_INFINITY, f32::INFINITY);
    for p in points {
        let half = lens.half(d, p.toward);
        lowest = lowest.max(value(p) - half);
        highest = highest.min(value(p) + half);
    }
    (lowest <= highest).then(|| wanted.clamp(lowest, highest))
}

/// What the search for the distance that fits found.
enum Fit {
    /// Even right in front of the box the extent is within what is wanted: it never binds.
    Never,
    /// The distance at which the extent is what is wanted.
    At(f32),
    /// The extent was still too big at the farthest distance tried, so no distance was found.
    Unreachable,
}

/// The smallest `toward` of `points`: the farthest corner from the camera.
fn farthest(points: &[Seen; 8]) -> f32 {
    points.iter().map(|p| p.toward).fold(f32::INFINITY, f32::min)
}

/// The distance past `floor` at which `excess` (the projected extent over what is wanted, falling as the camera backs
/// off) reaches zero.
fn distance_where_it_fits(floor: f32, excess: impl Fn(f32) -> f32) -> Fit {
    let mut near = floor + NEAREST_GAP;
    let at_the_box = excess(near);
    // A NaN extent binds nothing here; the finished camera is checked at the end.
    if at_the_box.is_nan() || at_the_box <= 0.0 {
        return Fit::Never;
    }
    // Double the step until the far end fits, so that the root lies between the ends.
    let mut step = near.max(1.0);
    let far = loop {
        let far = near + step;
        if excess(far) <= 0.0 {
            break Some(far);
        }
        near = far;
        step *= 2.0;
        if !step.is_finite() {
            break None;
        }
    };
    let Some(mut far) = far else { return Fit::Unreachable };
    for _ in 0..BISECTIONS {
        let mid = f32::midpoint(near, far);
        if excess(mid) > 0.0 {
            near = mid;
        } else {
            far = mid;
        }
    }
    Fit::At(far)
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
/// the character lacks: `face` without a head (without eyes it frames on [`EYES_WITHOUT_BONES`]), `bust` without a chest or head, `hands` without hands. Also
/// for a non-finite or out-of-range input: `aspect <= 0`, `fov` outside (0, 180), `zoom <= 0`, `|pitch| > 90`, `|yaw| > 180` (the ranges `SceneDef::check` enforces); for
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

    // The camera's axes depend only on where it looks from, not on how far. Built here, not by `Camera::look_at`:
    // at the poles `cos(90°)` is not exactly zero in f32, so look_at would flip right and up between 89.9999° and 90°.
    // The right axis is the horizontal one at every pitch, which is what look_at gives short of the poles.
    let (sin_yaw, cos_yaw) = def.yaw.to_radians().sin_cos();
    let (sin_pitch, cos_pitch) = if def.pitch.abs() == MAX_PITCH_DEG {
        (def.pitch.signum(), 0.0)
    } else {
        def.pitch.to_radians().sin_cos()
    };
    let back = Vec3::new(sin_yaw * cos_pitch, sin_pitch, cos_yaw * cos_pitch);
    let right = Vec3::new(cos_yaw, 0.0, -sin_yaw);
    let up = back.cross(right);
    let axes = (right, up, back);
    let mut camera = Camera {
        transform: Mat4::from_cols(right.extend(0.0), up.extend(0.0), back.extend(0.0), Vec4::W),
        projection: Projection::Perspective { fov_y_deg: def.fov, near: NEAR_FLOOR, far: 1.0 },
    };

    // What the frame sees at distance `d` from the anchor: NDC = across or up over `(d - toward) * t`.
    let t = (def.fov.to_radians() * 0.5).tan();
    let b = &shot.bounds;
    let span_corners = corners(Vec3::new(b.min.x, shot.lo, b.min.z), Vec3::new(b.max.x, shot.hi, b.max.z));
    let width_corners = corners(b.min, b.max);
    let span = seen(&span_corners, shot.anchor, axes);
    let width = seen(&width_corners, shot.anchor, axes);
    let floor = nearest(&span).max(nearest(&width));
    // Where the corners land when the camera looks `slide` metres along `value`'s axis from the anchor: across or up
    // over `(d - toward) * t * scale`, so -1 and 1 are the frame's edges.
    let lens = Lens { t, scale: 1.0 };
    let wide_lens = Lens { t, scale: aspect };
    // A distance where no slide keeps every corner inside is too close, however the extent compares: back off.
    let tall = distance_where_it_fits(floor, |d| {
        let Some(slide) = slide_into_frame(&span, |p| p.y, lens, d, -shot.aim * d * t) else { return f32::INFINITY };
        extent(&span, |p| p.y, lens, d, slide) - 2.0 * shot.fill
    });
    let wide = distance_where_it_fits(floor, |d| {
        let Some(slide) = slide_into_frame(&width, |p| p.x, wide_lens, d, 0.0) else { return f32::INFINITY };
        extent(&width, |p| p.x, wide_lens, d, slide) - 2.0 * shot.fill
    });
    let mut fit = 0.0_f32;
    for found in [tall, wide] {
        match found {
            Fit::At(d) => fit = fit.max(d),
            Fit::Never => {}
            Fit::Unreachable => {
                return Err(StageError::Framing {
                    reason: format!("no camera distance frames the {framing:?} shot with a {} degree lens", def.fov),
                });
            }
        }
    }
    let d = fit / def.zoom;
    if !(d.is_finite() && d > 0.0) {
        // Neither extent exceeds its share even 1 mm in front of the nearest corner: the lens is so wide that the shot
        // fits from inside the subject's box, where there is no camera to place. Or the span has no extent at all.
        return Err(StageError::Framing {
            reason: format!(
                "the {framing:?} shot with a {} degree lens fits from closer than {NEAREST_GAP} m to the subject, or the \
                 span has no extent: there is no camera distance to give it",
                def.fov
            ),
        });
    }

    // Looking at a point `aim * d * t` above the anchor, in the camera's up, puts the anchor at `aim` in the frame,
    // unless that would push a corner out of it: then the camera slides just far enough to bring the corner back.
    let h = 2.0 * d * t;
    // (Zoomed in past the fit, nothing may fit; the aim then stands.)
    let rise = slide_into_frame(&span, |p| p.y, lens, d, -shot.aim * d * t).unwrap_or(-shot.aim * d * t);
    let across = slide_into_frame(&width, |p| p.x, wide_lens, d, 0.0).unwrap_or(0.0);
    let target = shot.anchor + up * rise + right * across;
    let position = target + back * d;
    camera.transform.w_axis = position.extend(1.0);
    // The near plane stops short of the nearest corner, the far plane lies past the farthest by some character heights.
    let nearest_depth = d - floor;
    let near = if nearest_depth > 0.0 { NEAR_MARGIN * nearest_depth } else { (NEAR_SHARE * d).max(NEAR_FLOOR) };
    camera.projection = Projection::Perspective {
        fov_y_deg: def.fov,
        near,
        far: (d - farthest(&span).min(farthest(&width))) + FAR_HEIGHTS * landmarks.height_m,
    };
    // Moving the eye and the target together keeps the orientation, so the shift is a shift of the transform.
    let offset = right * (-def.shift[0] * aspect * h) + up * (-def.shift[1] * h);
    camera.transform.w_axis += offset.extend(0.0);
    // Fail closed: landmarks that came from outside `Landmarks::measure` can still give a non-finite camera.
    camera.validate().map_err(|e| StageError::Framing { reason: format!("the {framing:?} camera isn't usable: {e}") })?;
    // The shot as asked (no zoom in, no shift) frames every corner, in f32 as the renderer will project it. Extreme
    // lenses can defeat the fit by rounding, and a shot that cuts the subject is refused, not returned.
    if def.zoom <= 1.0 && def.shift == [0.0, 0.0] && !frames(&camera, aspect, &span_corners, &width_corners) {
        return Err(StageError::Framing {
            reason: format!(
                "the {framing:?} shot can't be framed with a {} degree lens at aspect {aspect}: a corner would fall outside the frame",
                def.fov
            ),
        });
    }
    Ok(camera)
}

/// Whether `camera` shows every span corner, and the box corners across, inside the frame and in front of its near
/// plane. Uses the projection the renderers use (`Camera::projection_matrix` and `view`).
fn frames(camera: &Camera, aspect: f32, span: &[Vec3; 8], width: &[Vec3; 8]) -> bool {
    let Projection::Perspective { near, .. } = camera.projection else { return false };
    let (view, to_clip) = (camera.view(), camera.projection_matrix(aspect) * camera.view());
    let limit = 1.0 + FRAME_TOLERANCE;
    let projects_inside = |p: Vec3, vertically: bool| {
        let clip = to_clip * Vec4::from((p, 1.0));
        let ahead = -(view * Vec4::from((p, 1.0))).z > near;
        ahead && clip.w > 0.0 && (clip.x / clip.w).abs() <= limit && (!vertically || (clip.y / clip.w).abs() <= limit)
    };
    span.iter().all(|p| projects_inside(*p, true)) && width.iter().all(|p| projects_inside(*p, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_finds_the_root_reports_a_shot_that_never_binds_and_gives_up_on_one_that_never_fits() {
        // 10 / d - 1 is zero at d = 10.
        assert!(matches!(distance_where_it_fits(0.0, |d| 10.0 / d - 1.0), Fit::At(d) if (d - 10.0).abs() < 1e-3));
        assert!(matches!(distance_where_it_fits(0.0, |_| -1.0), Fit::Never));
        // Too big at every distance, so the doubling runs out and no root is claimed.
        assert!(matches!(distance_where_it_fits(0.0, |_| 1.0), Fit::Unreachable));
    }
}
