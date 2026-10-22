//! The tracking doctor's image readings (spec v3 PF-28): light, backlight, glasses glare and the face's place in the
//! picture, read from the webcam's frames. The findings, the averaging and hysteresis, and the readings that need no
//! pixels (drops, latency, jitter) are the contract's, in [`omuzaki_track_api::doctor`], and re-exported here; this
//! module adds [`ObserveFrame::observe_frame`], which feeds a frame's readings to a [`Doctor`].
//!
//! **Cost.** [`ObserveFrame::observe_frame`] reads a grid of at most about [`MAX_SAMPLES`] pixels, whatever the
//! frame's size, and allocates nothing: well under PF-28's 0.2 ms per frame. It runs on the tracker's thread, never the
//! UI's, and works the same on a recorded take's frames.

pub use omuzaki_track_api::doctor::{AVERAGE_SECS, Diagnosis, Doctor, DoctorBoard, DoctorConfig, Finding, RELEASE};

use crate::frame::RgbFrame;
use crate::landmarks::{Landmark, NUM_LANDMARKS};

/// The most pixels one frame's image readings sample.
pub const MAX_SAMPLES: u32 = 8_192;

/// Face Mesh's contour of the person's right eye (the image's left), MediaPipe's `FACEMESH_RIGHT_EYE` indices.
const RIGHT_EYE: [usize; 16] = [33, 7, 163, 144, 145, 153, 154, 155, 133, 173, 157, 158, 159, 160, 161, 246];
/// Face Mesh's contour of the person's left eye, MediaPipe's `FACEMESH_LEFT_EYE` indices.
const LEFT_EYE: [usize; 16] = [263, 249, 390, 373, 374, 380, 381, 382, 362, 398, 384, 385, 386, 387, 388, 466];
/// How far around an eye's contour glare is looked for, as a share of the contour's size (glasses' lenses are wider
/// than the eye).
const EYE_MARGIN: f32 = 0.6;
/// A pixel this bright (luma, 0 to 1) is a highlight, not skin or iris.
const GLARE_LUMA: f32 = 0.96;

/// Feeds a frame's image readings to a [`Doctor`].
pub trait ObserveFrame {
    /// Reads a frame `dt` seconds after the last one: light, backlight, glare and the face's place in the picture.
    /// `landmarks` is the face found in it, if any. Hot path: allocates nothing.
    fn observe_frame(&mut self, frame: &RgbFrame<'_>, landmarks: Option<&[Landmark; NUM_LANDMARKS]>, dt: f32);
}

impl ObserveFrame for Doctor {
    fn observe_frame(&mut self, frame: &RgbFrame<'_>, landmarks: Option<&[Landmark; NUM_LANDMARKS]>, dt: f32) {
        let Some(reading) = read_image(frame, landmarks) else { return };
        self.record(Finding::LowLight, reading.face_luma, dt);
        if let Some(background) = reading.background_luma {
            self.record(Finding::Backlight, background - reading.face_luma, dt);
        }
        if let Some(glare) = reading.eye_glare {
            self.record(Finding::GlassesGlare, glare, dt);
        }
        if let Some(margin) = reading.edge_margin {
            self.record(Finding::FaceNearEdge, margin, dt);
        }
    }
}

/// One frame's image readings, before averaging.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ImageReading {
    /// The face's mean luma (0 to 1), or the frame's with no face.
    pub face_luma: f32,
    /// The mean luma outside the face; `None` with no face.
    pub background_luma: Option<f32>,
    /// The share of near-white pixels around the eyes; `None` with no face.
    pub eye_glare: Option<f32>,
    /// The face's distance from the nearest frame edge, as a share of the frame (negative when partly out); `None`
    /// with no face.
    pub edge_margin: Option<f32>,
}

/// Reads one frame: the face from `landmarks` (normalised image coordinates), or the whole frame with none. At most
/// about [`MAX_SAMPLES`] pixels are read for the face and background together, and as many again around the eyes;
/// nothing is allocated.
#[allow(clippy::cast_precision_loss)] // pixel counts are far below f32's exact range
pub fn read_image(frame: &RgbFrame<'_>, landmarks: Option<&[Landmark; NUM_LANDMARKS]>) -> Option<ImageReading> {
    frame.validate().ok()?;
    let whole = Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 };
    let Some(points) = landmarks else {
        let mut all = Tally::default();
        scan(frame, whole, |_, luma| all.add(luma, 0.0));
        return (all.count > 0).then(|| ImageReading {
            face_luma: all.mean(),
            background_luma: None,
            eye_glare: None,
            edge_margin: None,
        });
    };
    let face = Rect::around(points.iter())?;
    let (mut inside, mut outside) = (Tally::default(), Tally::default());
    scan(frame, whole, |(u, v), luma| if face.contains(u, v) { inside.add(luma, 0.0) } else { outside.add(luma, 0.0) });
    let mut eyes = Tally::default();
    for contour in [RIGHT_EYE, LEFT_EYE] {
        let eye = Rect::around(contour.iter().filter_map(|&i| points.get(i)))?.grown(EYE_MARGIN);
        scan(frame, eye, |_, luma| eyes.add(luma, GLARE_LUMA));
    }
    Some(ImageReading {
        face_luma: inside.mean(),
        background_luma: (outside.count > 0).then(|| outside.mean()),
        eye_glare: (eyes.count > 0).then(|| eyes.bright as f32 / eyes.count as f32),
        edge_margin: Some(face.x0.min(face.y0).min(1.0 - face.x1).min(1.0 - face.y1)),
    })
}

/// A running sum of luma, with the count of samples and of those at least a brightness.
#[derive(Copy, Clone, Debug, Default)]
struct Tally {
    sum: f32,
    count: u32,
    bright: u32,
}

impl Tally {
    fn add(&mut self, luma: f32, bright: f32) {
        self.sum += luma;
        self.count += 1;
        if bright > 0.0 && luma >= bright {
            self.bright += 1;
        }
    }

    #[allow(clippy::cast_precision_loss)] // pixel counts are far below f32's exact range
    fn mean(&self) -> f32 {
        if self.count > 0 { self.sum / self.count as f32 } else { 0.0 }
    }
}

/// The sampling grid's step, so at most about [`MAX_SAMPLES`] of `pixels` are read.
fn grid_step(pixels: u64) -> usize {
    let mut step = 1u64;
    while pixels / (step * step) > u64::from(MAX_SAMPLES) {
        step += 1;
    }
    usize::try_from(step).unwrap_or(usize::MAX)
}

/// Calls `each((u, v), luma)` for a grid of at most about [`MAX_SAMPLES`] pixels inside `area` (normalised, clipped
/// to the frame), with `(u, v)` the pixel's centre in normalised coordinates and luma BT.601 from 0 to 1. Only the
/// sampled pixels are touched.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // pixel coordinates are far below f32's exact range, and the clipped bounds are inside the frame
fn scan(frame: &RgbFrame<'_>, area: Rect, mut each: impl FnMut((f32, f32), f32)) {
    let (width, height) = (frame.width as f32, frame.height as f32);
    let clip = |value: f32, size: u32| (value.max(0.0) * size as f32).min(size as f32) as usize;
    let (left, right) = (clip(area.x0, frame.width), clip(area.x1, frame.width));
    let (top, bottom) = (clip(area.y0, frame.height), clip(area.y1, frame.height));
    if right <= left || bottom <= top {
        return;
    }
    let step = grid_step((right - left) as u64 * (bottom - top) as u64);
    for py in (top..bottom).step_by(step) {
        let row = py * frame.stride;
        let row_v = (py as f32 + 0.5) / height;
        for px in (left..right).step_by(step) {
            let at = row + px * 3;
            let Some(&[r, g, b]) = frame.data.get(at..at + 3).and_then(|p| <&[u8; 3]>::try_from(p).ok()) else {
                continue;
            };
            let luma = (0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b)) / 255.0;
            each(((px as f32 + 0.5) / width, row_v), luma);
        }
    }
}

/// An axis-aligned box in normalised image coordinates.
#[derive(Copy, Clone, Debug)]
struct Rect {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl Rect {
    fn around<'a>(points: impl Iterator<Item = &'a Landmark>) -> Option<Self> {
        let mut rect = Self { x0: f32::INFINITY, y0: f32::INFINITY, x1: f32::NEG_INFINITY, y1: f32::NEG_INFINITY };
        for p in points.filter(|p| p[0].is_finite() && p[1].is_finite()) {
            rect = Self { x0: rect.x0.min(p[0]), y0: rect.y0.min(p[1]), x1: rect.x1.max(p[0]), y1: rect.y1.max(p[1]) };
        }
        (rect.x1 > rect.x0 && rect.y1 > rect.y0).then_some(rect)
    }

    fn grown(self, share: f32) -> Self {
        let (dx, dy) = ((self.x1 - self.x0) * share, (self.y1 - self.y0) * share);
        Self { x0: self.x0 - dx, y0: self.y0 - dy, x1: self.x1 + dx, y1: self.y1 + dy }
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x0 && x < self.x1 && y >= self.y0 && y < self.y1
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss, clippy::manual_midpoint)] // test fixtures: small pixel coordinates
mod tests {
    use super::*;

    const W: u32 = 320;
    const H: u32 = 240;

    /// A frame of one grey level, with an optional square patch of another.
    fn frame(level: u8, patch: Option<(u8, [f32; 4])>) -> Vec<u8> {
        let mut data = vec![level; (W * H * 3) as usize];
        if let Some((value, [x0, y0, x1, y1])) = patch {
            for y in 0..H {
                for x in 0..W {
                    let (u, v) = ((x as f32 + 0.5) / W as f32, (y as f32 + 0.5) / H as f32);
                    if u >= x0 && u < x1 && v >= y0 && v < y1 {
                        let i = ((y * W + x) * 3) as usize;
                        data[i..i + 3].fill(value);
                    }
                }
            }
        }
        data
    }

    fn rgb(data: &[u8]) -> RgbFrame<'_> {
        RgbFrame { data, width: W, height: H, stride: (W * 3) as usize }
    }

    /// A face filling [x0, x1) x [y0, y1), with its eye contours in its upper half.
    fn face([x0, y0, x1, y1]: [f32; 4]) -> [Landmark; NUM_LANDMARKS] {
        let mut points = [[(x0 + x1) / 2.0, (y0 + y1) / 2.0, 0.0]; NUM_LANDMARKS];
        points[0] = [x0, y0, 0.0];
        points[1] = [x1, y1, 0.0];
        let (fw, fh) = (x1 - x0, y1 - y0);
        for (eye, cx) in [(RIGHT_EYE, x0 + fw * 0.3), (LEFT_EYE, x0 + fw * 0.7)] {
            for (k, &i) in eye.iter().enumerate() {
                let a = k as f32 / eye.len() as f32 * std::f32::consts::TAU;
                points[i] = [cx + a.cos() * fw * 0.08, y0 + fh * 0.4 + a.sin() * fh * 0.04, 0.0];
            }
        }
        points
    }

    fn found(doctor: &Doctor) -> Vec<Finding> {
        doctor.diagnoses().map(|d| d.finding).collect()
    }

    #[test]
    fn a_well_lit_centred_face_is_healthy() {
        let data = frame(110, Some((140, [0.3, 0.2, 0.7, 0.8])));
        let points = face([0.3, 0.2, 0.7, 0.8]);
        let mut doctor = Doctor::default();
        for _ in 0..30 {
            doctor.observe_frame(&rgb(&data), Some(&points), 1.0 / 30.0);
        }
        assert_eq!(found(&doctor), []);
    }

    #[test]
    fn a_dark_face_is_low_light_and_a_bright_window_behind_it_is_backlight() {
        let points = face([0.3, 0.2, 0.7, 0.8]);
        let dark = frame(30, Some((35, [0.3, 0.2, 0.7, 0.8])));
        let mut doctor = Doctor::default();
        doctor.observe_frame(&rgb(&dark), Some(&points), 1.0 / 30.0);
        assert_eq!(found(&doctor), [Finding::LowLight]);

        let window = frame(240, Some((70, [0.3, 0.2, 0.7, 0.8])));
        let mut doctor = Doctor::default();
        doctor.observe_frame(&rgb(&window), Some(&points), 1.0 / 30.0);
        assert_eq!(found(&doctor), [Finding::Backlight]);
        let d = doctor.diagnoses().next().unwrap();
        assert!(d.measured > 0.6 && (d.threshold - 0.25).abs() < 1e-6, "{d:?}");
        assert!(d.fix().contains("behind you"));
    }

    #[test]
    fn white_patches_over_the_eyes_are_glasses_glare() {
        let points = face([0.3, 0.2, 0.7, 0.8]);
        let mut data = frame(110, Some((140, [0.3, 0.2, 0.7, 0.8])));
        // A lens reflection over the right eye's region.
        for y in 0..H {
            for x in 0..W {
                let (u, v) = ((x as f32 + 0.5) / W as f32, (y as f32 + 0.5) / H as f32);
                if (u - 0.42).abs() < 0.02 && (v - 0.44).abs() < 0.015 {
                    let i = ((y * W + x) * 3) as usize;
                    data[i..i + 3].fill(255);
                }
            }
        }
        let mut doctor = Doctor::default();
        doctor.observe_frame(&rgb(&data), Some(&points), 1.0 / 30.0);
        assert_eq!(found(&doctor), [Finding::GlassesGlare]);
    }

    #[test]
    fn a_face_at_the_edge_is_found_and_clears_with_hysteresis() {
        let data = frame(110, None);
        let mut doctor = Doctor::default();
        doctor.observe_frame(&rgb(&data), Some(&face([0.005, 0.2, 0.4, 0.8])), 1.0);
        assert_eq!(found(&doctor), [Finding::FaceNearEdge]);
        // Just past the threshold is not enough to clear: the release needs 0.025.
        doctor.observe_frame(&rgb(&data), Some(&face([0.022, 0.2, 0.4, 0.8])), 1.0);
        doctor.observe_frame(&rgb(&data), Some(&face([0.022, 0.2, 0.4, 0.8])), 1.0);
        assert_eq!(found(&doctor), [Finding::FaceNearEdge]);
        for _ in 0..5 {
            doctor.observe_frame(&rgb(&data), Some(&face([0.3, 0.2, 0.7, 0.8])), 1.0);
        }
        assert_eq!(found(&doctor), []);
    }

    #[test]
    fn recheck_forgets_and_no_face_reads_the_whole_frame() {
        let dark = frame(20, None);
        let mut doctor = Doctor::default();
        doctor.observe_frame(&rgb(&dark), None, 1.0 / 30.0);
        assert_eq!(found(&doctor), [Finding::LowLight]);
        doctor.recheck();
        assert_eq!(found(&doctor), []);
        assert_eq!(doctor.reading(Finding::LowLight), None);
    }

    #[test]
    fn the_grid_reads_at_most_the_sample_cap() {
        for (w, h) in [(640u64, 480u64), (1280, 720), (1920, 1080), (3840, 2160), (64, 64)] {
            let step = grid_step(w * h) as u64;
            let read = w.div_ceil(step) * h.div_ceil(step);
            assert!(read <= u64::from(MAX_SAMPLES) * 11 / 10, "{w}x{h}: {read}");
        }
    }
}
