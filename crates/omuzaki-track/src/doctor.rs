//! The tracking doctor (spec v3 PF-28): says, live, what is hurting tracking and how to fix it.
//!
//! Each [`Finding`] has a measured detector, a threshold and a plain-language fix:
//!
//! | Finding | Measured | On when |
//! |---|---|---|
//! | [`LowLight`](Finding::LowLight) | the face's mean luma (the whole frame's with no face) | under 0.25 |
//! | [`Backlight`](Finding::Backlight) | how much brighter the background is than the face | over 0.25 |
//! | [`GlassesGlare`](Finding::GlassesGlare) | the share of near-white pixels around the eyes | over 3% |
//! | [`FaceNearEdge`](Finding::FaceNearEdge) | the face's distance from the frame's nearest edge | under 2% of the frame |
//! | [`DroppedFrames`](Finding::DroppedFrames) | the share of frames that never arrived or were skipped | over 5% |
//! | [`Overloaded`](Finding::Overloaded) | the tracker's latency, p95 | over the frame interval |
//! | [`Jitter`](Finding::Jitter) | the spread of the gaps between frames (a phone over Wi-Fi) | over 15 ms |
//!
//! Readings are averaged over about a second ([`AVERAGE_SECS`]) and each finding has hysteresis (it clears only
//! once the reading is back past [`RELEASE`] of its threshold), so a flicker of light or one late packet does not
//! make a diagnosis blink. [`Doctor::recheck`] forgets everything, for the "recheck" button after a fix.
//!
//! **Cost.** [`Doctor::observe_frame`] reads a grid of at most about [`MAX_SAMPLES`] pixels, whatever the frame's
//! size, and allocates nothing: well under PF-28's 0.2 ms per frame. It runs on the tracker's thread, never the UI's,
//! and works the same on a recorded take's frames.

use std::time::Duration;

use omuzaki_track_api::tracker::LatencyStats;

use crate::frame::RgbFrame;
use crate::landmarks::{Landmark, NUM_LANDMARKS};

/// How long readings are averaged over, in seconds.
pub const AVERAGE_SECS: f32 = 1.0;

/// A finding clears once its reading is back past this share of its threshold (hysteresis).
pub const RELEASE: f32 = 0.8;

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

/// What the doctor can find.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum Finding {
    /// The face is too dark for the landmarks to hold.
    LowLight,
    /// A bright window or lamp behind the person leaves the face in shadow.
    Backlight,
    /// Light reflected on glasses hides the eyes.
    GlassesGlare,
    /// The face is at the edge of the picture or partly out of it.
    FaceNearEdge,
    /// Frames are lost before they reach the tracker.
    DroppedFrames,
    /// The machine cannot keep up: tracking takes longer than a frame.
    Overloaded,
    /// Frames arrive unevenly: a phone over a busy Wi-Fi network.
    Jitter,
}

impl Finding {
    /// Every finding, in report order.
    pub const ALL: [Self; 7] = [
        Self::LowLight,
        Self::Backlight,
        Self::GlassesGlare,
        Self::FaceNearEdge,
        Self::DroppedFrames,
        Self::Overloaded,
        Self::Jitter,
    ];

    /// Its stable id, for ops and settings (`low_light` …).
    pub const fn id(self) -> &'static str {
        match self {
            Self::LowLight => "low_light",
            Self::Backlight => "backlight",
            Self::GlassesGlare => "glasses_glare",
            Self::FaceNearEdge => "face_near_edge",
            Self::DroppedFrames => "dropped_frames",
            Self::Overloaded => "overloaded",
            Self::Jitter => "jitter",
        }
    }

    /// The finding with this [`id`](Self::id).
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.id() == id)
    }

    /// What to do about it, in plain words.
    pub const fn fix(self) -> &'static str {
        match self {
            Self::LowLight => "Your face is too dark. Turn on a lamp in front of you, or raise the camera's exposure.",
            Self::Backlight => {
                "A bright light is behind you. Close the curtain or move the lamp in front of you, facing you."
            }
            Self::GlassesGlare => {
                "Light is reflecting off your glasses. Move the lamp or screen higher or to the side, or tilt your \
                 glasses down a little."
            }
            Self::FaceNearEdge => "Your face is at the edge of the picture. Move to the middle, or turn the camera.",
            Self::DroppedFrames => {
                "Frames are being lost. Close other apps using the camera, use a USB port on the computer itself, or \
                 pick a lower resolution."
            }
            Self::Overloaded => {
                "Your computer can't keep up with tracking. Switch to a lighter tracking quality, or close other \
                 heavy apps."
            }
            Self::Jitter => {
                "The phone's frames arrive unevenly. Move closer to the Wi-Fi router, use 5 GHz Wi-Fi, or connect \
                 the computer by cable."
            }
        }
    }

    /// Whether a high reading is the bad side (otherwise a low one is).
    const fn high_is_bad(self) -> bool {
        !matches!(self, Self::LowLight | Self::FaceNearEdge)
    }

    const fn index(self) -> usize {
        self as usize
    }
}

/// One thing the doctor found: what, how bad it measured and where the line is.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Diagnosis {
    /// What is wrong.
    pub finding: Finding,
    /// The averaged reading, in the finding's unit (a 0 to 1 share or luma, or milliseconds).
    pub measured: f32,
    /// The threshold it crossed, in the same unit.
    pub threshold: f32,
}

impl Diagnosis {
    /// What to do about it.
    pub const fn fix(&self) -> &'static str {
        self.finding.fix()
    }
}

/// The thresholds, in each finding's unit (see the module table).
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct DoctorConfig {
    /// Thresholds in [`Finding::ALL`] order.
    pub thresholds: [f32; Finding::ALL.len()],
}

impl Default for DoctorConfig {
    fn default() -> Self {
        Self { thresholds: [0.25, 0.25, 0.03, 0.02, 0.05, 33.3, 15.0] }
    }
}

impl DoctorConfig {
    /// The threshold of `finding`.
    pub const fn threshold(&self, finding: Finding) -> f32 {
        self.thresholds[finding.index()]
    }

    /// With `finding`'s threshold set to `value`.
    #[must_use]
    pub const fn with_threshold(mut self, finding: Finding, value: f32) -> Self {
        self.thresholds[finding.index()] = value;
        self
    }
}

/// A reading averaged over [`AVERAGE_SECS`]: the first sample sets it, later ones move it by their share of time.
#[derive(Copy, Clone, Debug, Default)]
struct Average(Option<f32>);

impl Average {
    fn add(&mut self, value: f32, dt: f32) {
        if !value.is_finite() {
            return;
        }
        let dt = if dt.is_finite() { dt.clamp(0.0, AVERAGE_SECS) } else { 0.0 };
        self.0 = Some(match self.0 {
            None => value,
            Some(old) => old + (value - old) * (1.0 - (-dt / AVERAGE_SECS).exp()),
        });
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

/// The doctor: feed it frames, the tracker's latency and frame arrivals; read its [`diagnoses`](Self::diagnoses).
#[derive(Clone, Debug)]
pub struct Doctor {
    config: DoctorConfig,
    readings: [Average; Finding::ALL.len()],
    active: [bool; Finding::ALL.len()],
    /// The last latency counters seen, to turn the totals into a rate.
    counters: Option<(u64, u64)>,
    /// The last frame arrival, and the averaged gap and squared deviation between arrivals, in seconds.
    arrival: Option<f64>,
    gap: Average,
    gap_variance: Average,
}

impl Default for Doctor {
    fn default() -> Self {
        Self::new(DoctorConfig::default())
    }
}

impl Doctor {
    /// A doctor with these thresholds, having seen nothing.
    pub fn new(config: DoctorConfig) -> Self {
        Self {
            config,
            readings: [Average::default(); Finding::ALL.len()],
            active: [false; Finding::ALL.len()],
            counters: None,
            arrival: None,
            gap: Average::default(),
            gap_variance: Average::default(),
        }
    }

    /// Reads a frame `dt` seconds after the last one: light, backlight, glare and the face's place in the picture.
    /// `landmarks` is the face found in it, if any. Hot path: allocates nothing.
    pub fn observe_frame(&mut self, frame: &RgbFrame<'_>, landmarks: Option<&[Landmark; NUM_LANDMARKS]>, dt: f32) {
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

    /// Reads the tracker's counters, `dt` seconds after the last time: the share of frames dropped since then, and its
    /// p95 against `frame_interval` (the camera's, or the frame rate the stage asks for).
    #[allow(clippy::cast_precision_loss)] // frame counts between two reads are small
    pub fn observe_latency(&mut self, stats: &LatencyStats, frame_interval: Duration, dt: f32) {
        if let Some((frames, dropped)) = self.counters {
            let (new_frames, new_dropped) =
                (stats.frames.saturating_sub(frames), stats.dropped.saturating_sub(dropped));
            let total = new_frames + new_dropped;
            if total > 0 {
                self.record(Finding::DroppedFrames, new_dropped as f32 / total as f32, dt);
            }
        }
        self.counters = Some((stats.frames, stats.dropped));
        // The threshold follows the frame interval, so a 60 fps camera is held to 16.7 ms.
        let interval_ms = frame_interval.as_secs_f32() * 1e3;
        if interval_ms > 0.0 {
            self.config.thresholds[Finding::Overloaded.index()] = interval_ms;
        }
        self.record(Finding::Overloaded, stats.p95.as_secs_f32() * 1e3, dt);
    }

    /// Notes a frame arriving at `time` seconds (any clock, increasing): the spread of the gaps is the jitter.
    #[allow(clippy::cast_possible_truncation)] // gaps between frames are far inside f32
    pub fn observe_arrival(&mut self, time: f64) {
        if let Some(last) = self.arrival.replace(time) {
            let gap = (time - last) as f32;
            if gap <= 0.0 || !gap.is_finite() {
                return;
            }
            let mean = self.gap.0.unwrap_or(gap);
            self.gap.add(gap, gap);
            self.gap_variance.add((gap - mean) * (gap - mean), gap);
            let jitter_ms = self.gap_variance.0.unwrap_or(0.0).sqrt() * 1e3;
            self.record(Finding::Jitter, jitter_ms, gap);
        }
    }

    fn record(&mut self, finding: Finding, value: f32, dt: f32) {
        let i = finding.index();
        self.readings[i].add(value, dt);
        let Some(reading) = self.readings[i].0 else { return };
        let threshold = self.config.thresholds[i];
        let (on, off) = if finding.high_is_bad() {
            (reading > threshold, reading < threshold * RELEASE)
        } else {
            (reading < threshold, reading > threshold / RELEASE)
        };
        if on {
            self.active[i] = true;
        } else if off {
            self.active[i] = false;
        }
    }

    /// What it finds now, in [`Finding::ALL`] order.
    pub fn diagnoses(&self) -> impl Iterator<Item = Diagnosis> + '_ {
        Finding::ALL.into_iter().filter(|f| self.active[f.index()]).filter_map(|finding| {
            let measured = self.readings[finding.index()].0?;
            Some(Diagnosis { finding, measured, threshold: self.config.threshold(finding) })
        })
    }

    /// The averaged reading of `finding`, found or not; `None` before anything was measured for it.
    pub fn reading(&self, finding: Finding) -> Option<f32> {
        self.readings[finding.index()].0
    }

    /// Forgets every reading and finding, to measure again from now (the "recheck" after a fix).
    pub fn recheck(&mut self) {
        *self = Self::new(self.config);
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
    fn dropped_frames_and_a_slow_tracker_come_from_the_counters() {
        let mut doctor = Doctor::default();
        let interval = Duration::from_secs_f32(1.0 / 30.0);
        let ms = Duration::from_millis;
        doctor.observe_latency(&LatencyStats::new(0, 0, ms(8), ms(12), ms(20)), interval, 1.0);
        doctor.observe_latency(&LatencyStats::new(30, 0, ms(8), ms(12), ms(20)), interval, 1.0);
        assert_eq!(found(&doctor), []);
        // A second of drops and slow frames, twice: the average needs more than one bad reading.
        doctor.observe_latency(&LatencyStats::new(50, 10, ms(30), ms(45), ms(60)), interval, 1.0);
        doctor.observe_latency(&LatencyStats::new(70, 20, ms(30), ms(45), ms(60)), interval, 1.0);
        assert_eq!(found(&doctor), [Finding::DroppedFrames, Finding::Overloaded]);
        // A 60 fps camera is held to its own frame interval.
        let mut doctor = Doctor::default();
        doctor.observe_latency(
            &LatencyStats::new(0, 0, ms(15), ms(20), ms(25)),
            Duration::from_secs_f32(1.0 / 60.0),
            1.0,
        );
        assert_eq!(found(&doctor), [Finding::Overloaded]);
    }

    #[test]
    fn uneven_arrivals_are_jitter_and_even_ones_are_not() {
        let mut steady = Doctor::default();
        let mut uneven = Doctor::default();
        let mut t = 0.0;
        for i in 0..120 {
            steady.observe_arrival(f64::from(i) / 60.0);
            t += if i % 3 == 0 { 0.06 } else { 0.005 };
            uneven.observe_arrival(t);
        }
        assert_eq!(found(&steady), []);
        assert_eq!(found(&uneven), [Finding::Jitter]);
        assert!(uneven.reading(Finding::Jitter).unwrap() > 15.0);
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
    fn ids_round_trip_and_every_finding_has_a_fix() {
        for f in Finding::ALL {
            assert_eq!(Finding::from_id(f.id()), Some(f));
            assert!(f.fix().ends_with('.'));
        }
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
