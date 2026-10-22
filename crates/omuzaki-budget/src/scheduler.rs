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
/// Particles come back only while both the frame and the `Particles` pass stay under this fraction of their budgets...
pub const DENSITY_UP_MARGIN: f64 = 0.6;
/// ...for this many consecutive samples (2 s at 30 fps), times the current hold (particles.md D8).
pub const DENSITY_UP_SAMPLES: u32 = 60;
/// The highest encoder load (encode time per frame over the frame period) an output may sustain (record.md D2). Over it
/// for [`DOWN_SAMPLES`] samples, quality steps down to free the CPU the encoder shares with the renderer; what the
/// output encodes never changes.
pub const OUTPUT_LOAD_LIMIT: f32 = 0.9;

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
/// **What is compared.** The frame's GPU time without `PaintTextures` and `Particles` (the face painter and the
/// particles have their own lines in §10; see [`face_pressure`](Self::face_pressure)); when the backend has no GPU
/// timings, the CPU recording time instead.
///
/// **Particles first** (particles.md D8). The `Particles` pass's smoothed GPU time is held against
/// [`budget::PARTICLES_GPU`]: over it for [`DOWN_SAMPLES`], density steps down ([`Step::DENSITY`]). A frame over budget
/// while particles are drawn also steps density down before any rung of [`Step::LADDER`]. Density comes back last,
/// once the ladder is fully given back and both times have stayed under [`DENSITY_UP_MARGIN`] of their budgets for
/// [`DENSITY_UP_SAMPLES`].
/// GPU timings describe an earlier finished frame ([`GpuTimings::frame`](omuzaki_render_api::GpuTimings)), so a
/// measurement is used once, and never when it was taken before the last change took effect.
#[derive(Clone, Debug)]
pub struct Scheduler {
    requested: Quality,
    budget: Seconds,
    /// How many ladder rungs are applied to `requested`.
    rungs: usize,
    /// How many of [`Step::DENSITY`] are applied.
    density: usize,
    particle_ema: Option<f64>,
    /// Submission number of the newest particle timing used.
    last_particles: Option<u64>,
    particle_over: u32,
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
    /// The newest highest encoder load ([`observe_output`](Self::observe_output)).
    output_load: Option<f32>,
    output_over: u32,
    output_under: u32,
}

impl Scheduler {
    /// A scheduler that keeps `requested` inside `budget` (for example [`budget::LIVE_GPU`]), starting at full
    /// `requested` quality.
    pub fn new(requested: Quality, budget: Seconds) -> Self {
        Self {
            requested,
            budget,
            rungs: 0,
            density: 0,
            particle_ema: None,
            last_particles: None,
            particle_over: 0,
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
            output_load: None,
            output_over: 0,
            output_under: 0,
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

    /// How many rungs of [`Step::DENSITY`] are applied (0 draws every particle the user asked for).
    pub fn density_rungs(&self) -> usize {
        self.density
    }

    /// The `Particles` pass's smoothed GPU time, once there is a measurement since the last change.
    pub fn particles_smoothed(&self) -> Option<Seconds> {
        self.particle_ema.map(Seconds)
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
            output_load: self.output_load,
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
        if let Some(change) = self.observe_particles(report) {
            return Some(change);
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
                if report.particles > 0
                    && let Some(change) = self.density_down(report.frame, smoothed)
                {
                    return Some(change);
                }
                return self.step_down(report.frame, smoothed);
            }
        } else {
            self.over = 0;
            if self.outputs_busy() {
                self.under = 0;
            } else if self.density > 0 && self.ladder_given_back() {
                let particles_calm = self.particle_ema.is_none_or(|t| t < budget::PARTICLES_GPU.0 * DENSITY_UP_MARGIN);
                if smoothed < self.budget.0 * DENSITY_UP_MARGIN && particles_calm {
                    self.under += 1;
                    if self.under >= DENSITY_UP_SAMPLES * self.hold {
                        return self.density_up(report.frame, smoothed);
                    }
                } else {
                    self.under = 0;
                }
            } else if smoothed < self.budget.0 * UP_MARGIN {
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

    /// Takes one sample of the highest encoder load among the running outputs (`OutputStatus::load`: encode time per
    /// frame over the frame period, smoothed by the output). Returns the change of quality it caused, if any.
    /// Allocation-free and lock-free. It never touches an output's size or rate: those are fixed when it starts.
    ///
    /// Over [`OUTPUT_LOAD_LIMIT`] for [`DOWN_SAMPLES`] samples in a row steps one rung down, as frame time does. Under
    /// [`UP_MARGIN`] of the limit for [`UP_SAMPLES`] times the hold, while the frame time also has room, takes one back;
    /// flaps double the hold as they do for frame time, counted in `FrameReport::frame`s. [`observe`](Self::observe)
    /// holds its own step up back while the latest load is at or above that margin. A `NaN` or negative load is
    /// ignored, and only [`QualityMode::Live`] is scheduled.
    pub fn observe_output(&mut self, load: f32) -> Option<Change> {
        if load.is_nan() || load < 0.0 {
            return None;
        }
        self.output_load = Some(load);
        if self.requested.mode != QualityMode::Live {
            return None;
        }
        let frame = self.newest.unwrap_or(0);
        let measured = self.ema.unwrap_or(self.budget.0);
        if load > OUTPUT_LOAD_LIMIT {
            self.output_under = 0;
            self.output_over = self.output_over.saturating_add(1);
            if self.output_over >= DOWN_SAMPLES {
                return self.step_down(frame, measured);
            }
            return None;
        }
        self.output_over = 0;
        let frame_has_room = self.ema.is_none_or(|t| t < self.budget.0 * UP_MARGIN);
        if !self.outputs_busy() && frame_has_room {
            self.output_under = self.output_under.saturating_add(1);
            if self.output_under >= UP_SAMPLES * self.hold {
                return self.step_up(frame, measured);
            }
        } else {
            self.output_under = 0;
        }
        None
    }

    /// The newest encoder load, if an output has reported one.
    pub fn output_load(&self) -> Option<f32> {
        self.output_load
    }

    /// Whether the outputs leave too little room to take quality back: the latest load is at or above [`UP_MARGIN`] of
    /// [`OUTPUT_LOAD_LIMIT`].
    fn outputs_busy(&self) -> bool {
        self.output_load.is_some_and(|load| f64::from(load) >= f64::from(OUTPUT_LOAD_LIMIT) * UP_MARGIN)
    }

    /// `FrameReport::frame` went backwards, so the caller broke the contract on [`observe`](Self::observe) or restarted
    /// its numbering: forget everything measured so far (the quality stays) and take no decision on this report.
    fn start_new_sequence(&mut self, frame: u64) {
        self.newest = Some(frame);
        self.ema = None;
        self.face_ema = None;
        self.particle_ema = None;
        self.last_particles = None;
        self.particle_over = 0;
        self.last_measured = None;
        self.last_face = None;
        self.stale_through = 0;
        self.last_up = None;
        (self.over, self.under) = (0, 0);
        (self.output_over, self.output_under) = (0, 0);
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

    /// The `Particles` pass's own line: only a GPU timing carries it, each finished frame counts once, and a sustained
    /// overrun of [`budget::PARTICLES_GPU`] thins the particles.
    fn observe_particles(&mut self, report: &FrameReport) -> Option<Change> {
        let gpu = report.gpu.filter(|gpu| gpu.frame <= report.frame)?;
        if self.last_particles.is_some_and(|last| gpu.frame <= last) || gpu.frame <= self.stale_through {
            return None;
        }
        self.last_particles = Some(gpu.frame);
        let Some(time) = gpu.passes.get(Pass::Particles) else {
            self.particle_over = 0;
            return None;
        };
        let smoothed = smooth(self.particle_ema, time.0);
        self.particle_ema = Some(smoothed);
        if smoothed <= budget::PARTICLES_GPU.0 {
            self.particle_over = 0;
            return None;
        }
        self.particle_over = self.particle_over.saturating_add(1);
        if self.particle_over < DOWN_SAMPLES {
            return None;
        }
        let change = self.density_down(gpu.frame, smoothed)?;
        Some(Change { budget: budget::PARTICLES_GPU, ..change })
    }

    fn density_down(&mut self, frame: u64, measured: f64) -> Option<Change> {
        let from = self.quality;
        let density =
            (self.density + 1..=Step::DENSITY.len()).find(|&d| quality_at(&self.requested, self.rungs, d) != from)?;
        self.hold_after_flap(frame);
        Some(self.change(frame, Direction::Down, Step::DENSITY[density - 1], self.rungs, density, from, measured))
    }

    fn density_up(&mut self, frame: u64, measured: f64) -> Option<Change> {
        let from = self.quality;
        let density = (0..self.density).rev().find(|&d| quality_at(&self.requested, self.rungs, d) != from)?;
        self.last_up = Some(frame);
        Some(self.change(frame, Direction::Up, Step::DENSITY[density], self.rungs, density, from, measured))
    }

    fn step_down(&mut self, frame: u64, measured: f64) -> Option<Change> {
        let from = self.quality;
        let (rungs, step) = (self.rungs + 1..=Step::LADDER.len())
            .find(|&r| quality_at(&self.requested, r, self.density) != from)
            .map(|r| (r, Step::LADDER[r - 1]))?;
        self.hold_after_flap(frame);
        Some(self.change(frame, Direction::Down, step, rungs, self.density, from, measured))
    }

    fn step_up(&mut self, frame: u64, measured: f64) -> Option<Change> {
        let from = self.quality;
        let rungs = (0..self.rungs).rev().find(|&r| quality_at(&self.requested, r, self.density) != from)?;
        self.last_up = Some(frame);
        Some(self.change(frame, Direction::Up, Step::LADDER[rungs], rungs, self.density, from, measured))
    }

    /// Every ladder rung that changed anything is taken back (rungs that change nothing for this request may remain).
    fn ladder_given_back(&self) -> bool {
        quality_at(&self.requested, self.rungs, 0) == self.requested
    }

    /// A step down soon after a step up means the step up was a mistake: wait longer before the next one.
    fn hold_after_flap(&mut self, frame: u64) {
        if self.last_up.is_some_and(|up| frame.saturating_sub(up) < FLAP_WINDOW) {
            self.hold = (self.hold * 2).min(MAX_HOLD);
        } else {
            self.hold = 1;
        }
    }

    #[allow(clippy::too_many_arguments)] // one call per kind of step; a struct would only rename the arguments
    fn change(
        &mut self,
        frame: u64,
        direction: Direction,
        step: Step,
        rungs: usize,
        density: usize,
        from: Quality,
        measured: f64,
    ) -> Change {
        self.rungs = rungs;
        self.density = density;
        self.quality = quality_at(&self.requested, rungs, density);
        // Timings of frames already submitted with the old quality say nothing about the new one.
        self.stale_through = frame;
        self.ema = None;
        self.particle_ema = None;
        (self.over, self.under, self.particle_over) = (0, 0, 0);
        (self.output_over, self.output_under) = (0, 0);
        Change { frame, direction, step, from, to: self.quality, measured: Seconds(measured), budget: self.budget }
    }
}

/// The frame time to hold against the budget, and the frame it belongs to. `None` for a GPU timing that names a frame
/// after the report's own: a timing cannot come from the future, so it is a cached result from before a restart of the
/// numbering and says nothing about the frames since.
fn measurement(report: &FrameReport) -> Option<(u64, Seconds)> {
    match report.gpu {
        Some(gpu) if gpu.frame <= report.frame => Some((gpu.frame, frame_time(&gpu.passes))),
        Some(_) => None,
        None => Some((report.frame, frame_time(&report.cpu))),
    }
}

/// `gpu.frame` of §10: every pass but the two with lines of their own, face paint and particles.
fn frame_time(times: &PassTimes) -> Seconds {
    let own_line = |pass| matches!(pass, Pass::PaintTextures | Pass::Particles);
    Seconds(times.iter().filter(|&(pass, _)| !own_line(pass)).map(|(_, t)| t.0).sum())
}

fn smooth(previous: Option<f64>, sample: f64) -> f64 {
    previous.map_or(sample, |p| p + (sample - p) * EMA_ALPHA)
}
