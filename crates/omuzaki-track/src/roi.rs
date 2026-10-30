//! Regions of interest: the rectangles MediaPipe crops faces with, and the maths that moves points between a
//! model's input tensor and the full frame.
//!
//! Conventions (all as in MediaPipe): image y points down; a positive rotation turns the rectangle clockwise on
//! screen; normalised values are fractions of the frame width and height.

use std::f32::consts::PI;

/// A rectangle in pixels, rotated about its centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RotatedRect {
    /// Centre x in pixels.
    pub cx: f32,
    /// Centre y in pixels.
    pub cy: f32,
    /// Width in pixels (before rotation).
    pub width: f32,
    /// Height in pixels (before rotation).
    pub height: f32,
    /// Clockwise rotation in radians.
    pub rotation: f32,
}

/// A rotated rectangle normalised by the frame size (MediaPipe's `NormalizedRect`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormRect {
    /// Centre x as a fraction of the frame width.
    pub x_center: f32,
    /// Centre y as a fraction of the frame height.
    pub y_center: f32,
    /// Width as a fraction of the frame width.
    pub width: f32,
    /// Height as a fraction of the frame height.
    pub height: f32,
    /// Clockwise rotation in radians.
    pub rotation: f32,
}

impl NormRect {
    /// The whole frame, unrotated.
    pub fn whole_frame() -> Self {
        Self { x_center: 0.5, y_center: 0.5, width: 1.0, height: 1.0, rotation: 0.0 }
    }

    /// The same rectangle in pixels of a `frame_w` x `frame_h` frame.
    pub fn to_pixels(&self, frame_w: f32, frame_h: f32) -> RotatedRect {
        RotatedRect {
            cx: self.x_center * frame_w,
            cy: self.y_center * frame_h,
            width: self.width * frame_w,
            height: self.height * frame_h,
            rotation: self.rotation,
        }
    }

    /// MediaPipe's `RectTransformationCalculator` with no shift: optionally squares the rectangle to its longer side
    /// (measured in pixels), then scales it. The centre and rotation are unchanged.
    #[must_use]
    pub fn expanded(&self, frame_w: f32, frame_h: f32, scale: f32, square_long: bool) -> Self {
        let (mut width, mut height) = (self.width, self.height);
        if square_long {
            let long_side = (width * frame_w).max(height * frame_h);
            width = long_side / frame_w;
            height = long_side / frame_h;
        }
        Self { width: width * scale, height: height * scale, ..*self }
    }
}

impl NormRect {
    /// MediaPipe's `RectTransformationCalculator` shift: moves the centre by `shift_x` widths and `shift_y` heights
    /// along the rectangle's own (rotated) axes, measured in pixels of a `frame_w` x `frame_h` frame.
    #[must_use]
    pub fn shifted(&self, frame_w: f32, frame_h: f32, shift_x: f32, shift_y: f32) -> Self {
        let (sin, cos) = self.rotation.sin_cos();
        let (dx, dy) = (frame_w * self.width * shift_x, frame_h * self.height * shift_y);
        Self {
            x_center: self.x_center + (dx * cos - dy * sin) / frame_w,
            y_center: self.y_center + (dx * sin + dy * cos) / frame_h,
            ..*self
        }
    }

    /// MediaPipe's `AlignmentPointsRectsCalculator`: a square centred on `start` whose side is twice the distance to
    /// `end` (in pixels), turned so the line from `start` to `end` points at `target` radians (pi/2: straight up).
    pub fn aligned(start: (f32, f32), end: (f32, f32), frame_w: f32, frame_h: f32, target: f32) -> Self {
        let side = 2.0 * ((end.0 - start.0) * frame_w).hypot((end.1 - start.1) * frame_h);
        Self {
            x_center: start.0,
            y_center: start.1,
            width: side / frame_w,
            height: side / frame_h,
            rotation: normalize_radians(target + rotation_from_points(start, end, frame_w, frame_h)),
        }
    }
}

/// Wraps an angle into `[-pi, pi)`.
pub fn normalize_radians(angle: f32) -> f32 {
    angle - 2.0 * PI * ((angle + PI) / (2.0 * PI)).floor()
}

/// The angle that makes the line from `start` to `end` (normalised image points) horizontal, as MediaPipe's
/// `DetectionsToRects` computes it: `0 - atan2(-(y1 - y0), x1 - x0)` in pixels, wrapped to `[-pi, pi)`.
pub fn rotation_from_points(start: (f32, f32), end: (f32, f32), frame_w: f32, frame_h: f32) -> f32 {
    let (dx, dy) = ((end.0 - start.0) * frame_w, (end.1 - start.1) * frame_h);
    normalize_radians(-(-dy).atan2(dx))
}

/// A normalised axis-aligned box: the bounding box of a detection or a landmark set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BBox {
    /// Left edge.
    pub xmin: f32,
    /// Top edge.
    pub ymin: f32,
    /// Width.
    pub width: f32,
    /// Height.
    pub height: f32,
}

impl BBox {
    /// The smallest box containing every point, or `None` for an empty set.
    pub fn around(points: impl IntoIterator<Item = (f32, f32)>) -> Option<Self> {
        let mut it = points.into_iter();
        let first = it.next()?;
        let (mut x0, mut y0, mut x1, mut y1) = (first.0, first.1, first.0, first.1);
        for (x, y) in it {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        Some(Self { xmin: x0, ymin: y0, width: x1 - x0, height: y1 - y0 })
    }

    /// The upright rectangle with this box's centre and size.
    pub fn to_rect(&self, rotation: f32) -> NormRect {
        NormRect {
            x_center: self.xmin + self.width / 2.0,
            y_center: self.ymin + self.height / 2.0,
            width: self.width,
            height: self.height,
            rotation,
        }
    }

    /// Intersection over union with another box; 0 when they do not overlap.
    pub fn iou(&self, other: &Self) -> f32 {
        let w = (self.xmin + self.width).min(other.xmin + other.width) - self.xmin.max(other.xmin);
        let h = (self.ymin + self.height).min(other.ymin + other.height) - self.ymin.max(other.ymin);
        if w <= 0.0 || h <= 0.0 {
            return 0.0;
        }
        let inter = w * h;
        inter / (self.width * self.height + other.width * other.height - inter)
    }
}

/// How much of each side of a model input is padding (as a fraction of the input), after fitting an ROI to the
/// input's aspect ratio without distortion.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Letterbox {
    /// Fraction of the width padded on the left.
    pub left: f32,
    /// Fraction of the height padded on the top.
    pub top: f32,
    /// Fraction of the width padded on the right.
    pub right: f32,
    /// Fraction of the height padded on the bottom.
    pub bottom: f32,
}

/// Grows `roi` to the aspect ratio of a `tensor_w` x `tensor_h` input (about its centre) and reports the padding
/// that adds. This is MediaPipe's `PadRoi` with `keep_aspect_ratio`.
///
/// Returns `None` when the ROI has no area (zero, negative or not a number), which only a broken model output or a
/// bad caller produces; `roi` is left untouched then.
pub fn pad_to_aspect(roi: &mut RotatedRect, tensor_w: usize, tensor_h: usize) -> Option<Letterbox> {
    if !(roi.width > 0.0 && roi.height > 0.0) {
        return None;
    }
    #[allow(clippy::cast_precision_loss)] // model input sizes are tiny
    let tensor_aspect = tensor_h as f32 / tensor_w as f32;
    let roi_aspect = roi.height / roi.width;
    let (mut horizontal, mut vertical) = (0.0, 0.0);
    if tensor_aspect > roi_aspect {
        roi.height = roi.width * tensor_aspect;
        vertical = (1.0 - roi_aspect / tensor_aspect) / 2.0;
    } else {
        roi.width = roi.height / tensor_aspect;
        horizontal = (1.0 - tensor_aspect / roi_aspect) / 2.0;
    }
    Some(Letterbox { left: horizontal, top: vertical, right: horizontal, bottom: vertical })
}

/// Maps a point in a rotated ROI's own unit square (`(0,0)` top-left, `(1,1)` bottom-right) to normalised frame
/// coordinates: MediaPipe's `GetRotatedSubRectToRectTransformMatrix` applied to one point.
pub fn roi_to_frame(roi: &RotatedRect, frame_w: f32, frame_h: f32, u: f32, v: f32) -> (f32, f32) {
    let (cos, sin) = (roi.rotation.cos(), roi.rotation.sin());
    let (dx, dy) = ((u - 0.5) * roi.width, (v - 0.5) * roi.height);
    ((roi.cx + cos * dx - sin * dy) / frame_w, (roi.cy + sin * dx + cos * dy) / frame_h)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn radians_wrap_into_half_open_range() {
        assert!(close(normalize_radians(3.0 * PI), -PI));
        assert!(close(normalize_radians(-PI), -PI));
        assert!(close(normalize_radians(0.5), 0.5));
        assert!(close(normalize_radians(2.0 * PI + 0.25), 0.25));
    }

    #[test]
    fn level_eyes_give_zero_rotation_and_a_tilt_gives_the_opposite_angle() {
        assert!(close(rotation_from_points((0.4, 0.5), (0.6, 0.5), 100.0, 100.0), 0.0));
        // Right eye lower on screen than the left: the eye line tilts clockwise, so the ROI turns clockwise to follow it.
        let r = rotation_from_points((0.4, 0.5), (0.6, 0.6), 100.0, 100.0);
        assert!(close(r, (0.1f32).atan2(0.2)));
    }

    #[test]
    fn padding_fits_the_aspect_ratio_without_distortion() {
        // A wide 200x100 ROI into a square tensor grows to 200x200: a quarter of the height is padding top and bottom.
        let mut wide = RotatedRect { cx: 50.0, cy: 50.0, width: 200.0, height: 100.0, rotation: 0.0 };
        let lb = pad_to_aspect(&mut wide, 128, 128).unwrap();
        assert!(close(wide.width, 200.0) && close(wide.height, 200.0));
        assert!(close(lb.top, 0.25) && close(lb.bottom, 0.25) && close(lb.left, 0.0));
        let mut tall = RotatedRect { cx: 0.0, cy: 0.0, width: 100.0, height: 400.0, rotation: 0.0 };
        let lb = pad_to_aspect(&mut tall, 256, 256).unwrap();
        assert!(close(tall.width, 400.0) && close(lb.left, 0.375));
        let mut empty = RotatedRect { cx: 0.0, cy: 0.0, width: 0.0, height: 1.0, rotation: 0.0 };
        assert!(pad_to_aspect(&mut empty, 4, 4).is_none());
    }

    #[test]
    fn roi_corners_land_on_the_rotated_rectangle() {
        let roi = RotatedRect { cx: 50.0, cy: 50.0, width: 20.0, height: 10.0, rotation: PI / 2.0 };
        // Quarter turn clockwise: the ROI's top-left corner (-10, -5) from the centre becomes (+5, -10).
        let (x, y) = roi_to_frame(&roi, 100.0, 100.0, 0.0, 0.0);
        assert!(close(x, 0.55) && close(y, 0.40), "{x} {y}");
        let (cx, cy) = roi_to_frame(&roi, 100.0, 100.0, 0.5, 0.5);
        assert!(close(cx, 0.5) && close(cy, 0.5));
    }

    #[test]
    fn expansion_squares_by_pixels_then_scales() {
        let r = NormRect { x_center: 0.5, y_center: 0.5, width: 0.25, height: 0.5, rotation: 0.3 };
        // In a 200x100 frame the rect is 50x50 pixels: already square, so squaring changes nothing.
        let e = r.expanded(200.0, 100.0, 1.5, true);
        assert!(close(e.width, 0.375) && close(e.height, 0.75) && close(e.rotation, 0.3));
        // In a 100x100 frame it is 25x50: squaring takes the long side.
        let s = r.expanded(100.0, 100.0, 1.0, true);
        assert!(close(s.width, 0.5) && close(s.height, 0.5));
    }

    #[test]
    fn shifts_follow_the_rotated_axes() {
        let r = NormRect { x_center: 0.5, y_center: 0.5, width: 0.2, height: 0.4, rotation: 0.0 };
        let s = r.shifted(100.0, 100.0, 0.0, -0.5);
        assert!(close(s.x_center, 0.5) && close(s.y_center, 0.3));
        // A quarter turn clockwise: the rectangle's up points at the image's right, so the shift moves right.
        let q = NormRect { rotation: PI / 2.0, ..r }.shifted(100.0, 100.0, 0.0, -0.5);
        assert!(close(q.x_center, 0.7) && close(q.y_center, 0.5), "{q:?}");
    }

    #[test]
    fn aligned_squares_point_their_line_at_the_target() {
        // End straight above start: already at 90 degrees, so no rotation; side twice the distance.
        let r = NormRect::aligned((0.5, 0.6), (0.5, 0.4), 100.0, 100.0, PI / 2.0);
        assert!(close(r.rotation, 0.0) && close(r.width, 0.4) && close(r.height, 0.4) && close(r.y_center, 0.6));
        // End to the right of start: the rectangle turns a quarter clockwise, so its up points at the end.
        let t = NormRect::aligned((0.5, 0.5), (0.6, 0.5), 200.0, 100.0, PI / 2.0);
        assert!(close(t.rotation, PI / 2.0) && close(t.width, 0.2) && close(t.height, 0.4), "{t:?}");
    }

    #[test]
    fn bbox_around_points_and_iou() {
        let b = BBox::around([(0.2, 0.3), (0.6, 0.1), (0.4, 0.9)]).unwrap();
        assert!(close(b.xmin, 0.2) && close(b.ymin, 0.1) && close(b.width, 0.4) && close(b.height, 0.8));
        assert!(BBox::around(std::iter::empty()).is_none());
        let a = BBox { xmin: 0.0, ymin: 0.0, width: 1.0, height: 1.0 };
        let half = BBox { xmin: 0.5, ymin: 0.0, width: 1.0, height: 1.0 };
        assert!(close(a.iou(&a), 1.0) && close(a.iou(&half), 1.0 / 3.0));
        assert!(close(a.iou(&BBox { xmin: 2.0, ymin: 2.0, width: 1.0, height: 1.0 }), 0.0));
    }
}
