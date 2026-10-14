//! The scheduler: EMA smoothing of `FrameReport` times, hysteresis, and the [`Change`] it reports.

use omuzaki_core::time::Seconds;
use omuzaki_render_api::{FrameReport, Pass, PassTimes, Quality, QualityMode, budget};

use crate::ladder::{Step, quality_at};

/// Smoothing factor of the exponential moving average: the prototype's `gpuPoll` uses 0.1 too.
pub const EMA_ALPHA: f64 = 0.1;
/// Consecutive smoothed samples over budget before quality steps down. 12 samples is 0.4 s at 30 fps: a hitch (a
/// shader compile, a window drag) doesn't trigger it, a sustained overrun does quickly.
pub const DOWN_SAMPLES: u32 = 12;
/// Quality steps back up only while the smoothed time stays under this fraction of the budget...
pub const UP_MARGIN: f64 = 0.7;
/// ...for this many consecutive samples (5 s at 30 fps), times the current [`Scheduler`] hold.
pub const UP_SAMPLES: u32 = 150;
/// A step down within this many frames of a step up means the step up was a mistake (it flapped).
pub const FLAP_WINDOW: u64 = 600;
/// Each flap doubles the wait before the next step up, up to this multiple of [`UP_SAMPLES`].
pub const MAX_HOLD: u32 = 8;

/// Which way quality moved.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Over budget: a rung was applied.
    Down,
    /// Comfortably under budget for long enough: a rung was taken back.
    Up,
}

/// One change of quality, as a plain value: the app logs it and tells the user (spec §8). The scheduler itself logs
/// and draws nothing.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Change {
    /// The frame (`FrameReport::frame`) on which it was decided; the new quality applies to later frames.
    pub frame: u64,
    /// Lowered or restored.
    pub direction: Direction,
    /// The rung that was applied (`Down`) or taken back (`Up`).
    pub step: Step,
    /// The quality before.
    pub from: Quality,
    /// The quality from now on (the same as [`Scheduler::quality`]).
    pub to: Quality,
    /// The smoothed frame time that caused it.
    pub measured: Seconds,
    /// The budget it was compared with.
    pub budget: Seconds,
}

/// How the face painter's smoothed GPU time compares with [`budget::FACE_PAINT_GPU`]. The face system reads it to
/// shrink or grow its atlas (a separate follow-up; the atlas is its resource, not a `Quality` knob).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FacePressure {
    /// No measurement yet.
    Unknown,
    /// Over the face budget.
    Over,
    /// Under the budget but not by a wide margin: leave the atlas alone.
    Within,
    /// Under [`UP_MARGIN`] of the budget: there is room for a larger atlas.
    Under,
}

/// Steps `Quality` down when the smoothed frame time is over budget and back up when it has had room for a while.
///
/// Feed it every frame's [`FrameReport`] through [`observe`](Self::observe) and render with
/// [`quality`](Self::quality). Only [`QualityMode::Live`] is scheduled: an offline render has no time constraint, so
/// it is never degraded.
///
/// **What is compared.** The frame's GPU time without `PaintTextures` (the face painter has its own line in §10; see
/// [`face_pressure`](Self::face_pressure)); when the backend has no GPU timings, the CPU recording time instead.
/// GPU timings describe an earlier finished frame ([`GpuTimings::frame`](omuzaki_render_api::GpuTimings)), so a
/// measurement is used once, and never when it was taken before the last change took effect.
#[derive(Clone, Debug)]
pub struct Scheduler {
    requested: Quality,
    budget: Seconds,
    /// How many ladder rungs are applied to `requested`.
    rungs: usize,
    quality: Quality,
    ema: Option<f64>,
    face_ema: Option<f64>,
    /// Submission number of the newest face timing used.
    last_face: Option<u64>,
    /// `FrameReport::frame` of the newest report observed: the sequence position, used to spot a rewind.
    newest: Option<u64>,
    /// Submission number of the newest measurement used; measurements at or before it are repeats.
    last_measured: Option<u64>,
    /// Measurements of frames up to here were taken with an earlier quality.
    stale_through: u64,
    over: u32,
    under: u32,
    hold: u32,
    /// Submission number of the last step up.
    last_up: Option<u64>,
}

impl Scheduler {
    /// A scheduler that keeps `requested` inside `budget` (for example [`budget::LIVE_GPU`]), starting at full
    /// `requested` quality.
    pub fn new(requested: Quality, budget: Seconds) -> Self {
        Self {
            requested,
            budget,
            rungs: 0,
            quality: requested,
            ema: None,
            face_ema: None,
            last_face: None,
            newest: None,
            last_measured: None,
            stale_through: 0,
            over: 0,
            under: 0,
            hold: 1,
            last_up: None,
        }
    }

    /// The quality to render the next frame with.
    pub fn quality(&self) -> Quality {
        self.quality
    }

    /// The quality the user asked for, before any degrading.
    pub fn requested(&self) -> Quality {
        self.requested
    }

    /// The budget being kept.
    pub fn budget(&self) -> Seconds {
        self.budget
    }

    /// How many rungs of [`Step::LADDER`] are applied (0 is the full requested quality).
    pub fn rungs(&self) -> usize {
        self.rungs
    }

    /// The smoothed frame time, once there is a measurement since the last change.
    pub fn smoothed(&self) -> Option<Seconds> {
        self.ema.map(Seconds)
    }

    /// The face painter against [`budget::FACE_PAINT_GPU`].
    pub fn face_pressure(&self) -> FacePressure {
        match self.face_ema {
            None => FacePressure::Unknown,
            Some(t) if t > budget::FACE_PAINT_GPU.0 => FacePressure::Over,
            Some(t) if t < budget::FACE_PAINT_GPU.0 * UP_MARGIN => FacePressure::Under,
            Some(_) => FacePressure::Within,
        }
    }

    /// The user picked another quality (or budget): start again from the full request. Timings still in flight from
    /// before the switch are ignored, like those that predate a [`Change`].
    pub fn set_requested(&mut self, requested: Quality, budget: Seconds) {
        *self = Self {
            face_ema: self.face_ema,
            last_face: self.last_face,
            newest: self.newest,
            stale_through: self.newest.unwrap_or(0),
            ..Self::new(requested, budget)
        };
    }

    /// Takes one frame's report. Returns the change of quality it caused, if any; render with
    /// [`quality`](Self::quality) from the next frame on. Allocation-free and lock-free, so it is safe on the frame
    /// path.
    ///
    /// **Precondition.** While the scheduler observes a renderer, the caller gives every submission a
    /// `FrameInfo::index` at least one higher than the last (timeline position belongs in `FrameInfo::time`). That
    /// index is the submission's identity in `FrameReport::frame` and `GpuTimings::frame`. A timeline index would
    /// repeat while paused and jump back on a loop, so a cached GPU result could not be told from a fresh one.
    /// Offline renders are not observed and may repeat an index on purpose (exact re-renders).
    ///
    /// **If the caller breaks it,** the scheduler never degrades repeatedly because of it: a `frame` it has already
    /// measured (a cached GPU result reported again) counts once, and a `FrameReport::frame` lower than the newest
    /// seen starts a new sequence: the averages, counters and the stale cutoff are cleared, the quality stays, and no
    /// decision is taken on that report. A GPU timing that names a frame after its own report's (a result cached from
    /// before the restart) is ignored.
    pub fn observe(&mut self, report: &FrameReport) -> Option<Change> {
        if self.newest.is_some_and(|newest| report.frame < newest) {
            self.start_new_sequence(report.frame);
            return None;
        }
        self.newest = Some(report.frame);
        self.observe_face(report);
        if self.requested.mode != QualityMode::Live {
            return None;
        }
        let (frame, time) = measurement(report)?;
        if self.last_measured.is_some_and(|last| frame <= last) || frame <= self.stale_through {
            return None;
        }
        self.last_measured = Some(frame);
        let smoothed = smooth(self.ema, time.0);
        self.ema = Some(smoothed);

        if smoothed > self.budget.0 {
            self.under = 0;
            self.over += 1;
            if self.over >= DOWN_SAMPLES {
                return self.step_down(report.frame, smoothed);
            }
        } else {
            self.over = 0;
            if smoothed < self.budget.0 * UP_MARGIN {
                self.under += 1;
                if self.under >= UP_SAMPLES * self.hold {
                    return self.step_up(report.frame, smoothed);
                }
            } else {
                self.under = 0;
            }
        }
        None
    }

    /// `FrameReport::frame` went backwards, so the caller broke the contract on [`observe`](Self::observe) or restarted
    /// its numbering: forget everything measured so far (the quality stays) and take no decision on this report.
    fn start_new_sequence(&mut self, frame: u64) {
        self.newest = Some(frame);
        self.ema = None;
        self.face_ema = None;
        self.last_measured = None;
        self.last_face = None;
        self.stale_through = 0;
        self.last_up = None;
        (self.over, self.under) = (0, 0);
    }

    /// The face painter's own line: only a GPU timing carries it, and each finished frame counts once.
    fn observe_face(&mut self, report: &FrameReport) {
        let Some(gpu) = report.gpu.filter(|gpu| gpu.frame <= report.frame) else { return };
        if self.last_face.is_some_and(|last| gpu.frame <= last) || gpu.frame <= self.stale_through {
            return;
        }
        self.last_face = Some(gpu.frame);
        if let Some(time) = gpu.passes.get(Pass::PaintTextures) {
            self.face_ema = Some(smooth(self.face_ema, time.0));
        }
    }

    fn step_down(&mut self, frame: u64, measured: f64) -> Option<Change> {
        let from = self.quality;
        let (rungs, step) = (self.rungs + 1..=Step::LADDER.len())
            .find(|&r| quality_at(&self.requested, r) != from)
            .map(|r| (r, Step::LADDER[r - 1]))?;
        if self.last_up.is_some_and(|up| frame.saturating_sub(up) < FLAP_WINDOW) {
            self.hold = (self.hold * 2).min(MAX_HOLD);
        } else {
            self.hold = 1;
        }
        Some(self.change(frame, Direction::Down, step, rungs, from, measured))
    }

    fn step_up(&mut self, frame: u64, measured: f64) -> Option<Change> {
        let from = self.quality;
        let rungs = (0..self.rungs).rev().find(|&r| quality_at(&self.requested, r) != from)?;
        self.last_up = Some(frame);
        Some(self.change(frame, Direction::Up, Step::LADDER[rungs], rungs, from, measured))
    }

    fn change(
        &mut self,
        frame: u64,
        direction: Direction,
        step: Step,
        rungs: usize,
        from: Quality,
        measured: f64,
    ) -> Change {
        self.rungs = rungs;
        self.quality = quality_at(&self.requested, rungs);
        // Timings of frames already submitted with the old quality say nothing about the new one.
        self.stale_through = frame;
        self.ema = None;
        (self.over, self.under) = (0, 0);
        Change { frame, direction, step, from, to: self.quality, measured: Seconds(measured), budget: self.budget }
    }
}

/// The frame time to hold against the budget, and the frame it belongs to. `None` for a GPU timing that names a frame
/// after the report's own: a timing cannot come from the future, so it is a cached result from before a restart of the
/// numbering and says nothing about the frames since.
fn measurement(report: &FrameReport) -> Option<(u64, Seconds)> {
    match report.gpu {
        Some(gpu) if gpu.frame <= report.frame => Some((gpu.frame, without_paint(&gpu.passes))),
        Some(_) => None,
        None => Some((report.frame, without_paint(&report.cpu))),
    }
}

fn without_paint(times: &PassTimes) -> Seconds {
    Seconds(times.iter().filter(|&(pass, _)| pass != Pass::PaintTextures).map(|(_, t)| t.0).sum())
}

fn smooth(previous: Option<f64>, sample: f64) -> f64 {
    previous.map_or(sample, |p| p + (sample - p) * EMA_ALPHA)
}
