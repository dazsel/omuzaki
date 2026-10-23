//! The fixed frame graph: nine passes in the contract's order, each timed on the CPU and bracketed by GPU timestamps,
//! with the geometry passes repeated per round when a frame supersamples (`supersample.rs`).

use std::time::{Duration, Instant};

use omuzaki_core::time::Seconds;
use omuzaki_render_api::{GpuTimings, Pass, PassTimes, RenderError};

use super::supersample::{self, Accumulate};
use super::timing::{GpuTimer, Span};
use crate::pass::{self, FramePass, Outcome, PassCx};
use crate::shared::Shared;

/// What a recorded frame did.
pub(crate) struct Recorded {
    /// Which passes had work, indexed like [`Pass::ALL`].
    pub(crate) ran: [bool; Pass::COUNT],
    /// CPU time spent recording each pass that ran.
    pub(crate) cpu: PassTimes,
}

/// Adds `elapsed` to `pass`'s CPU time: a pass that repeats, or `Resolve` with the accumulation, is charged every part.
fn charge(times: &mut PassTimes, pass: Pass, elapsed: Duration) {
    let before = times.get(pass).map_or(0.0, |s| s.0);
    times.set(pass, Seconds(before + elapsed.as_secs_f64()));
}

/// The passes, the supersampling average and the GPU timer.
pub(crate) struct FrameGraph {
    passes: [Box<dyn FramePass>; Pass::COUNT],
    accumulate: Accumulate,
    timer: Option<GpuTimer>,
}

impl FrameGraph {
    pub(crate) fn new(shared: &Shared) -> Self {
        Self {
            passes: pass::build(shared),
            accumulate: Accumulate::new(shared),
            timer: GpuTimer::new(&shared.gpu.device, &shared.gpu.queue, shared.gpu.timestamps),
        }
    }

    /// The passes that are implemented, in frame order.
    pub(crate) fn supported(&self) -> Vec<Pass> {
        self.passes.iter().filter(|p| p.supported()).map(|p| p.pass()).collect()
    }

    /// Whether GPU timings will ever be reported.
    pub(crate) fn measures_gpu(&self) -> bool {
        self.timer.is_some()
    }

    /// The timer, for the GPU painters that run before a frame (`paint_texture`): their commands are timed as part of
    /// `PaintTextures`. `None` without timestamp support.
    pub(crate) fn timer_mut(&mut self) -> Option<&mut GpuTimer> {
        self.timer.as_mut()
    }

    /// Runs every pass in order into `cx.encoder`. The geometry passes run once per round (`rounds` is more than one
    /// when the frame supersamples), each round drawn with its own frame group and added into the average before the
    /// next; the passes before them and after them run once. Each round's accumulation is timed and charged to
    /// `Resolve`.
    ///
    /// # Errors
    /// The first error a pass returns; the encoder then holds a partial frame the caller must drop unsubmitted.
    pub(crate) fn record(&mut self, cx: &mut PassCx<'_>, rounds: u32) -> Result<Recorded, RenderError> {
        let mut recorded = Recorded { ran: [false; Pass::COUNT], cpu: PassTimes::default() };
        let rounds = rounds.clamp(1, supersample::MAX_ROUNDS);
        let repeats = self.passes.each_ref().map(|p| supersample::repeats(p.pass()));
        let first_repeated = repeats.iter().position(|&r| r);
        for index in 0..self.passes.len() {
            if !repeats[index] {
                self.run(index, 0, cx, &mut recorded)?;
                continue;
            }
            if Some(index) != first_repeated {
                continue;
            }
            for round in 0..rounds {
                cx.round = round as usize;
                cx.attachments.begin_round();
                for (repeated, _) in repeats.into_iter().enumerate().skip(index).filter(|&(_, r)| r) {
                    self.run(repeated, round, cx, &mut recorded)?;
                }
                self.accumulate(round, rounds, cx, &mut recorded);
            }
            cx.round = 0;
        }
        Ok(recorded)
    }

    /// Records pass `index` for `round`, bracketed by its timestamps; its CPU time adds to the pass's total.
    fn run(
        &mut self,
        index: usize,
        round: u32,
        cx: &mut PassCx<'_>,
        recorded: &mut Recorded,
    ) -> Result<(), RenderError> {
        let pass = &mut self.passes[index];
        let which = pass.pass();
        if let Some(timer) = &self.timer {
            timer.write(cx.encoder, round, Span::Pass(which), false);
        }
        cx.extra_cpu = std::time::Duration::ZERO;
        let start = Instant::now();
        let outcome = pass.record(cx)?;
        let elapsed = start.elapsed() + cx.extra_cpu;
        if let Some(timer) = &self.timer {
            timer.write(cx.encoder, round, Span::Pass(which), true);
        }
        if outcome == Outcome::Ran {
            recorded.ran[which.index()] = true;
            charge(&mut recorded.cpu, which, elapsed);
        }
        Ok(())
    }

    /// Adds `round` into the average when the frame supersamples; with one round there is nothing to add, but the
    /// timestamps are still written so the timer's block is whole.
    fn accumulate(&self, round: u32, rounds: u32, cx: &mut PassCx<'_>, recorded: &mut Recorded) {
        if let Some(timer) = &self.timer {
            timer.write(cx.encoder, round, Span::Accumulate, false);
        }
        if rounds > 1 {
            let start = Instant::now();
            cx.attachments.accumulate(cx.shared, cx.encoder, &self.accumulate, round, rounds);
            cx.stats.draw_calls += 1;
            charge(&mut recorded.cpu, Pass::Resolve, start.elapsed());
        }
        if let Some(timer) = &self.timer {
            timer.write(cx.encoder, round, Span::Accumulate, true);
        }
    }

    /// Resolves this frame's GPU timestamps into the frame's commands. Call after [`record`](Self::record), before
    /// the encoder is finished.
    pub(crate) fn resolve_timings(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        frame: u64,
        ran: [bool; Pass::COUNT],
        rounds: u32,
    ) {
        if let Some(timer) = &mut self.timer {
            timer.resolve(encoder, frame, ran, rounds);
        }
    }

    /// After the frame is submitted: requests the read-back and returns the newest finished GPU timings.
    pub(crate) fn collect_timings(&mut self) -> Option<GpuTimings> {
        let timer = self.timer.as_mut()?;
        timer.request_maps();
        timer.poll()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::{Gpu, HeadlessOptions};

    #[test]
    fn the_passes_are_built_in_frame_order() {
        let gpu = Gpu::open_headless(HeadlessOptions { adapter: crate::AdapterPreference::Software }).unwrap();
        let graph = FrameGraph::new(&Shared::new(gpu));
        let order: Vec<Pass> = graph.passes.iter().map(|p| p.pass()).collect();
        assert_eq!(order, Pass::ALL, "build() must list the passes in Pass::ALL order");
    }
}
