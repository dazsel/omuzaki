//! ROI reuse and frame skipping (`docs/design/track-roi-reuse.md`): which of the frame's passes a video-mode frame
//! needs. The decision is integer arithmetic over a fixed-size luma signature of the ROI, so it is deterministic for
//! a frame sequence and allocates nothing.

use crate::frame::RgbFrame;
use crate::roi::{NormRect, roi_to_frame};

/// Samples per side of the ROI signature.
const GRID: usize = 16;
/// Samples per side of one block of the peak measure (the signature is `GRID / BLOCK` blocks wide).
const BLOCK: usize = 4;
/// Samples in a signature.
const SAMPLES: usize = GRID * GRID;
/// The largest luma level.
const LEVELS: f32 = 255.0;

/// The knobs of ROI reuse and frame skipping, on [`FaceTrackerConfig`](crate::pipeline::FaceTrackerConfig). Only
/// video mode uses them.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ReuseConfig {
    /// Whether frames may be skipped and the detector re-run on a timer. Off, the tracker is MediaPipe's own video
    /// mode: the landmark pass runs on every frame and the detector only when the face is lost.
    pub enabled: bool,
    /// The detector re-runs every this many frames, whatever the confidence, so a second face or a drifting ROI is
    /// caught. `0` never re-runs it on a timer.
    pub redetect_every: u32,
    /// The most frames in a row that reuse the last result, so a result is never older than this many frames.
    pub max_skips: u32,
    /// A frame is skipped only while the ROI's mean luma change since the last full pass (a fraction of full scale)
    /// stays under this.
    pub skip_mean: f32,
    /// ... and no block of the ROI changed by more than this (a fraction of full scale), which catches a small local
    /// change such as a mouth opening that the mean hides.
    pub skip_peak: f32,
    /// A frame is skipped only when the last full pass's presence score was at least this: a doubtful face is looked
    /// at again.
    pub skip_presence: f32,
}

impl ReuseConfig {
    /// MediaPipe's video mode, with nothing skipped and no timed re-detection.
    pub const OFF: Self = Self { enabled: false, ..Self::DEFAULT };

    /// Re-detect every 60 frames, skip at most 2 frames in a row below 2% mean and 4% block luma change, when the
    /// last presence was at least 0.9.
    const DEFAULT: Self =
        Self { enabled: true, redetect_every: 60, max_skips: 2, skip_mean: 0.02, skip_peak: 0.04, skip_presence: 0.9 };
}

impl Default for ReuseConfig {
    /// Reuse on, with the values chosen in `docs/design/track-roi-reuse.md`.
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// What a video-mode frame needs.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Plan {
    /// Nothing: the last result stands.
    Skip,
    /// The landmark pass inside the ROI the last frame's landmarks gave.
    Track,
    /// The detector first. If it finds nothing and there is a tracked ROI, that one is used.
    Detect,
}

/// The luma of the ROI sampled on a `GRID` x `GRID` lattice.
type Signature = [u8; SAMPLES];

/// The reuse state machine: counts frames, remembers the ROI signature of the last full pass and decides each frame.
pub(super) struct ReuseGate {
    config: ReuseConfig,
    /// Frames handled since the detector last ran, counting that frame, so it is the age of the detection a frame sees.
    since_detect: u32,
    /// Frames in a row skipped so far.
    skipped: u32,
    /// The last full pass: the ROI it produced for the next frame, its signature on that frame, and its presence.
    reference: Option<Reference>,
}

struct Reference {
    rect: NormRect,
    signature: Signature,
    presence: f32,
}

impl ReuseGate {
    pub(super) fn new(config: ReuseConfig) -> Self {
        Self { config, since_detect: 0, skipped: 0, reference: None }
    }

    /// Forgets the last pass; the next frame runs the full pipeline.
    pub(super) fn reset(&mut self) {
        self.since_detect = 0;
        self.skipped = 0;
        self.reference = None;
    }

    /// What `frame` needs, given the ROI tracked from the last frame (`None` when the face is new or lost).
    pub(super) fn plan(&mut self, frame: &RgbFrame<'_>, tracked: Option<&NormRect>) -> Plan {
        let Some(tracked) = tracked else { return Plan::Detect };
        if !self.config.enabled {
            return Plan::Track;
        }
        if self.config.redetect_every > 0 && self.since_detect >= self.config.redetect_every {
            return Plan::Detect;
        }
        // The reference is only good for the ROI it was taken in: `track_roi` can have replaced that one.
        let Some(reference) = self.reference.as_ref().filter(|r| r.rect == *tracked) else { return Plan::Track };
        if self.skipped >= self.config.max_skips || reference.presence < self.config.skip_presence {
            return Plan::Track;
        }
        let (mean, peak) = difference(&reference.signature, &signature(frame, tracked));
        if mean < self.config.skip_mean && peak < self.config.skip_peak { Plan::Skip } else { Plan::Track }
    }

    /// Records that the last frame was skipped.
    pub(super) fn skipped(&mut self) {
        self.skipped += 1;
        self.since_detect = self.since_detect.saturating_add(1);
    }

    /// Records a full pass: the face was found (`detected`: by the detector) with `presence`, and `next` is the ROI
    /// its landmarks give the next frame.
    pub(super) fn passed(&mut self, frame: &RgbFrame<'_>, detected: bool, next: Option<NormRect>, presence: f32) {
        self.skipped = 0;
        self.since_detect = if detected { 1 } else { self.since_detect.saturating_add(1) };
        self.reference = if self.config.enabled {
            next.map(|rect| Reference { signature: signature(frame, &rect), rect, presence })
        } else {
            None
        };
    }

    /// Records that the face was not found, or that a pass failed.
    pub(super) fn lost(&mut self) {
        self.reset();
    }
}

/// `frame` sampled through `rect`: at each lattice point the mean luma of the 2x2 pixels there, which halves the
/// sensor noise a single pixel carries.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn signature(frame: &RgbFrame<'_>, rect: &NormRect) -> Signature {
    let (fw, fh) = (frame.width as f32, frame.height as f32);
    let roi = rect.to_pixels(fw, fh);
    let (max_x, max_y) = (frame.width as usize - 1, frame.height as usize - 1);
    let mut out = [0_u8; SAMPLES];
    for (i, level) in out.iter_mut().enumerate() {
        let (u, v) = (((i % GRID) as f32 + 0.5) / GRID as f32, ((i / GRID) as f32 + 0.5) / GRID as f32);
        let (x, y) = roi_to_frame(&roi, fw, fh, u, v);
        // Out-of-frame points (an ROI hanging over the edge) clamp to the edge pixel, like the crop's replicate border.
        let (px, py) = ((x * fw).clamp(0.0, max_x as f32) as usize, (y * fh).clamp(0.0, max_y as f32) as usize);
        let (qx, qy) = ((px + 1).min(max_x), (py + 1).min(max_y));
        let sum: u32 = [(px, py), (qx, py), (px, qy), (qx, qy)].iter().map(|&(x, y)| luma(frame, x, y)).sum();
        *level = (sum / 4) as u8;
    }
    out
}

/// The luma (Rec. 601 weights, 8-bit fixed point) of the pixel at `(x, y)`, which must be inside the frame.
fn luma(frame: &RgbFrame<'_>, x: usize, y: usize) -> u32 {
    let pixel = &frame.data[y * frame.stride + x * 3..][..3];
    (77 * u32::from(pixel[0]) + 150 * u32::from(pixel[1]) + 29 * u32::from(pixel[2])) >> 8
}

/// The mean and the largest block-mean absolute difference between two signatures, as fractions of full scale.
#[allow(clippy::cast_precision_loss)]
fn difference(a: &Signature, b: &Signature) -> (f32, f32) {
    const BLOCKS: usize = GRID / BLOCK;
    let mut blocks = [0_u32; BLOCKS * BLOCKS];
    for (i, (x, y)) in a.iter().zip(b).enumerate() {
        let (col, row) = (i % GRID / BLOCK, i / GRID / BLOCK);
        blocks[row * BLOCKS + col] += u32::from(x.abs_diff(*y));
    }
    let total: u32 = blocks.iter().sum();
    let peak = blocks.iter().copied().max().unwrap_or(0);
    let mean = total as f32 / (SAMPLES as f32 * LEVELS);
    let peak = peak as f32 / ((BLOCK * BLOCK) as f32 * LEVELS);
    (mean, peak)
}

#[cfg(test)]
mod tests {
    use super::Plan::{Skip, Track};
    use super::*;

    const SIDE: u32 = 64;
    const RECT: NormRect = NormRect { x_center: 0.5, y_center: 0.5, width: 0.5, height: 0.5, rotation: 0.0 };

    fn grey(level: u8) -> Vec<u8> {
        vec![level; (SIDE * SIDE * 3) as usize]
    }

    fn frame(pixels: &[u8]) -> RgbFrame<'_> {
        RgbFrame { data: pixels, width: SIDE, height: SIDE, stride: SIDE as usize * 3 }
    }

    fn gate() -> ReuseGate {
        ReuseGate::new(ReuseConfig::default())
    }

    #[test]
    fn identical_frames_have_no_difference_and_a_level_shift_is_the_mean() {
        let (a, b) = (signature(&frame(&grey(100)), &RECT), signature(&frame(&grey(100)), &RECT));
        assert_eq!(difference(&a, &b), (0.0, 0.0));
        let c = signature(&frame(&grey(100 + 25)), &RECT);
        let (mean, peak) = difference(&a, &c);
        assert!((mean - 25.0 / 255.0).abs() < 1e-6 && (peak - 25.0 / 255.0).abs() < 1e-6);
    }

    #[test]
    fn a_change_in_one_corner_shows_in_the_peak_far_more_than_in_the_mean() {
        let base = grey(100);
        let mut changed = base.clone();
        // The top-left 8x8 pixels of the 32x32 ROI at (16,16): one block of the lattice.
        for y in 16..24 {
            for x in 16..24 {
                changed[(y * SIDE as usize + x) * 3..][..3].fill(200);
            }
        }
        let (mean, peak) = difference(&signature(&frame(&base), &RECT), &signature(&frame(&changed), &RECT));
        assert!(peak > 5.0 * mean && peak > 0.3, "mean {mean} peak {peak}");
    }

    #[test]
    fn a_rect_over_the_edge_samples_the_edge_without_panicking() {
        let over = NormRect { x_center: 1.2, y_center: -0.2, width: 0.8, height: 0.8, rotation: 0.7 };
        assert_eq!(signature(&frame(&grey(9)), &over), [9; SAMPLES]);
    }

    #[test]
    fn the_gate_detects_first_skips_a_still_face_a_bounded_run_and_detects_on_a_timer() {
        let pixels = grey(100);
        let f = frame(&pixels);
        let mut g = gate();
        assert_eq!(g.plan(&f, None), Plan::Detect);
        g.passed(&f, true, Some(RECT), 0.99);
        let mut plans = Vec::new();
        for _ in 0..8 {
            let plan = g.plan(&f, Some(&RECT));
            match plan {
                Plan::Skip => g.skipped(),
                _ => g.passed(&f, false, Some(RECT), 0.99),
            }
            plans.push(plan);
        }
        assert_eq!(plans, [Skip, Skip, Track, Skip, Skip, Track, Skip, Skip]);
        // 8 frames since the detector, plus the timer's 60.
        g.since_detect = 60;
        assert_eq!(g.plan(&f, Some(&RECT)), Plan::Detect);
    }

    #[test]
    fn a_doubtful_face_a_foreign_roi_and_a_disabled_scheme_are_never_skipped() {
        let pixels = grey(100);
        let f = frame(&pixels);
        let mut g = gate();
        g.passed(&f, true, Some(RECT), 0.8);
        assert_eq!(g.plan(&f, Some(&RECT)), Plan::Track, "presence under skip_presence");
        g.passed(&f, true, Some(RECT), 0.99);
        let moved = NormRect { x_center: 0.6, ..RECT };
        assert_eq!(g.plan(&f, Some(&moved)), Plan::Track, "the reference was taken in another ROI");
        let mut off = ReuseGate::new(ReuseConfig::OFF);
        off.passed(&f, true, Some(RECT), 0.99);
        assert_eq!(off.plan(&f, Some(&RECT)), Plan::Track);
        off.since_detect = 10_000;
        assert_eq!(off.plan(&f, Some(&RECT)), Plan::Track, "no timed re-detection when off");
    }
}
