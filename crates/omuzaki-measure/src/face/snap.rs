//! The landmark snap (design D2 and 4.1): each given point moves to the centroid of the drawn mark around it, so a click
//! a few percent off lands on the same point every time.
//!
//! The rule is a windowed mean shift on the mask of marks (pixels inside the face region that are not skin): six times,
//! the point moves to the centroid of the mask inside a disc around it. A window, not the mark's connected shape, because
//! a lash that touches the brow, which touches the bangs, would make the mark's shape jump between images. The mouth
//! then moves to the mark's top edge, which is the lip line whether the mouth is open or closed.

use super::frame::{FaceFrame, Roi};
use super::segment::{STEP, Segmentation, disc};

/// Mean-shift iterations.
pub const ITERATIONS: usize = 6;

/// The eye window's radius and the share of its area the mask must cover (IOD of the given eyes; fraction).
pub const EYE: (f64, f64) = (0.25, 0.05);

/// The mouth window's radius and the share of its area the mask must cover.
pub const MOUTH: (f64, f64) = (0.15, 0.005);

/// The nose window's radius and the share of its area the mask must cover.
pub const NOSE: (f64, f64) = (0.10, 0.005);

/// How far either side of the mouth's centroid the top edge is looked for, along the face's down, IOD.
pub const MOUTH_REACH: f64 = 0.15;

/// The windowed mean shift from `start` (ROI pixels): the centroid of the marks in a disc of `radius`, six times.
/// `None` when, at any step, the marks cover less than `min_share` of the disc's area.
fn mean_shift(roi: &Roi, seg: &Segmentation, start: [f64; 2], radius: f64, min_share: f64) -> Option<[f64; 2]> {
    let area = std::f64::consts::PI * radius * radius;
    let mut at = start;
    for _ in 0..ITERATIONS {
        let (mut sx, mut sy, mut n) = (0.0, 0.0, 0_u32);
        for i in disc(roi, at, radius) {
            if seg.mark(i) {
                let c = roi.centre(i);
                sx += c[0];
                sy += c[1];
                n += 1;
            }
        }
        if f64::from(n) < min_share * area || n == 0 {
            return None;
        }
        at = [sx / f64::from(n), sy / f64::from(n)];
    }
    Some(at)
}

/// The top edge of the mouth mark through `centroid`: walking down the face from `MOUTH_REACH` above it to as far
/// below, the first mark pixel, and the height of the mark's run from there (the mouth's height), both in ROI pixels.
fn mouth_top(roi: &Roi, seg: &Segmentation, frame: &FaceFrame, centroid: [f64; 2]) -> Option<([f64; 2], f64)> {
    let reach = MOUTH_REACH * frame.iod_px;
    let point = |t: f64| [centroid[0] + t * frame.v[0], centroid[1] + t * frame.v[1]];
    let is_mark = |t: f64| roi.index(point(t)).is_some_and(|i| seg.mark(i));
    let mut t = -reach;
    while t <= reach {
        if is_mark(t) {
            let top = t;
            while is_mark(t + STEP) {
                t += STEP;
            }
            return Some((point(top), t - top + STEP));
        }
        t += STEP;
    }
    None
}

/// A landmark as given, as used, and whether the snap moved it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Snapped {
    /// The point the caller gave, image pixels.
    pub given: [f64; 2],
    /// The point the measure used, image pixels.
    pub used: [f64; 2],
    /// Whether the snap found a mark; `false` keeps the given point.
    pub snapped: bool,
}

/// The landmarks after the snap.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SnappedLandmarks {
    /// The character's right eye.
    pub eye_right: Snapped,
    /// The character's left eye.
    pub eye_left: Snapped,
    /// The mouth's top edge, when a mouth was given.
    pub mouth: Option<Snapped>,
    /// The nose mark, when a nose was given.
    pub nose: Option<Snapped>,
    /// The height of the mouth mark below its top edge, IOD, when the snap found it: what tells an open mouth from a
    /// closed one.
    pub mouth_height: Option<f64>,
}

impl SnappedLandmarks {
    /// Whether any landmark failed to snap.
    #[must_use]
    pub fn any_failed(&self) -> bool {
        [Some(self.eye_right), Some(self.eye_left), self.mouth, self.nose].into_iter().flatten().any(|s| !s.snapped)
    }
}

/// The given landmarks, in image pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Given {
    /// The character's right eye.
    pub eye_right: [f64; 2],
    /// The character's left eye.
    pub eye_left: [f64; 2],
    /// The mouth, anywhere on it.
    pub mouth: Option<[f64; 2]>,
    /// The nose mark.
    pub nose: Option<[f64; 2]>,
}

/// Snaps the given landmarks (image pixels) on the ROI's marks. `refine = false` keeps them as given (all `snapped:
/// false`). The eyes snap first; the mouth's top edge is then found along the face's down of the snapped eyes' frame.
#[must_use]
pub fn snap(given: &Given, roi: &Roi, seg: &Segmentation, refine: bool) -> SnappedLandmarks {
    let keep = |p: [f64; 2]| Snapped { given: p, used: p, snapped: false };
    let to_roi = |p: [f64; 2]| roi.from_image(p);
    let d0 = {
        let (r, l) = (to_roi(given.eye_right), to_roi(given.eye_left));
        (l[0] - r[0]).hypot(l[1] - r[1])
    };
    let shift = |p: [f64; 2], (radius, share): (f64, f64)| {
        mean_shift(roi, seg, to_roi(p), radius * d0, share).map(|at| Snapped {
            given: p,
            used: roi.to_image(at),
            snapped: true,
        })
    };
    if !refine {
        return SnappedLandmarks {
            eye_right: keep(given.eye_right),
            eye_left: keep(given.eye_left),
            mouth: given.mouth.map(keep),
            nose: given.nose.map(keep),
            mouth_height: None,
        };
    }
    let eye_right = shift(given.eye_right, EYE).unwrap_or_else(|| keep(given.eye_right));
    let eye_left = shift(given.eye_left, EYE).unwrap_or_else(|| keep(given.eye_left));
    let frame = FaceFrame::from_eyes(to_roi(eye_right.used), to_roi(eye_left.used));
    let mut mouth_height = None;
    let mouth = given.mouth.map(|p| {
        let found = frame.and_then(|frame| {
            let centroid = mean_shift(roi, seg, to_roi(p), MOUTH.0 * d0, MOUTH.1)?;
            let (top, height) = mouth_top(roi, seg, &frame, centroid)?;
            mouth_height = Some(height / frame.iod_px);
            Some(Snapped { given: p, used: roi.to_image(top), snapped: true })
        });
        found.unwrap_or_else(|| keep(p))
    });
    let nose = given.nose.map(|p| shift(p, NOSE).unwrap_or_else(|| keep(p)));
    SnappedLandmarks { eye_right, eye_left, mouth, nose, mouth_height }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)] // the ROI stores f32
mod tests {
    use super::*;
    use crate::colour::lab;

    /// A skin square with a dark eye disc at (30, 30), radius 4, and a mouth bar from x 40 to 60 at y 70 to 72.
    fn roi_and_seg() -> (Roi, Segmentation) {
        let (w, h) = (100_usize, 100_usize);
        let mut marks = vec![false; w * h];
        let mut lab_px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                #[allow(clippy::cast_precision_loss)]
                let (cx, cy) = (x as f64 + 0.5, y as f64 + 0.5);
                let eye = (cx - 30.0).hypot(cy - 30.0) < 4.0 || (cx - 70.0).hypot(cy - 30.0) < 4.0;
                let mouth = (40..60).contains(&x) && (70..73).contains(&y);
                marks[y * w + x] = eye || mouth;
                let rgb = if eye || mouth { [40, 30, 30] } else { [247, 220, 203] };
                lab_px.push(lab(rgb).map(|v| v as f32));
            }
        }
        let roi = Roi { width: w, height: h, lab: lab_px, alpha: vec![255; w * h], scale: 1, offset: [0.0, 0.0] };
        let seg = Segmentation {
            skin: lab([247, 220, 203]),
            line_l: None,
            lower_dl: -20.0,
            skin_like: marks.iter().map(|m| !m).collect(),
            filled: vec![true; w * h],
            background: None,
        };
        (roi, seg)
    }

    #[test]
    fn eyes_snap_to_the_mark_and_the_mouth_to_its_top_edge() {
        let (roi, seg) = roi_and_seg();
        let given = Given { eye_right: [31.5, 28.7], eye_left: [69.0, 31.0], mouth: Some([52.0, 71.0]), nose: None };
        let s = snap(&given, &roi, &seg, true);
        for (got, want) in [(s.eye_right.used, [30.0, 30.0]), (s.eye_left.used, [70.0, 30.0])] {
            assert!((got[0] - want[0]).abs() < 0.05 && (got[1] - want[1]).abs() < 0.05, "{got:?}");
        }
        let mouth = s.mouth.unwrap();
        assert!(mouth.snapped && (mouth.used[1] - 70.0).abs() <= 0.25, "{mouth:?}");
        let height = s.mouth_height.unwrap() * 40.0;
        assert!((height - 3.0).abs() <= 0.5, "{height}");
        assert!(!s.any_failed());
    }

    #[test]
    fn a_point_with_no_mark_nearby_keeps_its_place_and_says_so() {
        let (roi, seg) = roi_and_seg();
        let given = Given { eye_right: [30.0, 30.0], eye_left: [70.0, 30.0], mouth: None, nose: Some([50.0, 50.0]) };
        let s = snap(&given, &roi, &seg, true);
        let nose = s.nose.unwrap();
        assert!(!nose.snapped);
        assert_eq!(nose.used, [50.0, 50.0]);
        assert!(s.any_failed());
        let off = snap(&given, &roi, &seg, false);
        assert!(!off.eye_right.snapped);
        assert_eq!(off.eye_right.used, [30.0, 30.0]);
    }
}
