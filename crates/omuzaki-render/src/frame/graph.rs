//! The fixed frame graph: eight passes in the contract's order, each timed on the CPU and bracketed by GPU timestamps.

use std::time::Instant;

use omuzaki_core::time::Seconds;
use omuzaki_render_api::{GpuTimings, Pass, PassTimes, RenderError};

use super::timing::GpuTimer;
use crate::pass::{self, FramePass, Outcome, PassCx};
use crate::shared::Shared;

/// What a recorded frame did.
pub(crate) struct Recorded {
    /// Which passes had work, indexed like [`Pass::ALL`].
    pub(crate) ran: [bool; Pass::COUNT],
    /// CPU time spent recording each pass that ran.
    pub(crate) cpu: PassTimes,
}

/// The passes and the GPU timer.
pub(crate) struct FrameGraph {
    passes: [Box<dyn FramePass>; Pass::COUNT],
    timer: Option<GpuTimer>,
}

impl FrameGraph {
    pub(crate) fn new(shared: &Shared) -> Self {
        Self {
            passes: pass::build(shared),
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

    /// Runs every pass in order into `cx.encoder`.
    ///
    /// # Errors
    /// The first error a pass returns; the encoder then holds a partial frame the caller must drop unsubmitted.
    pub(crate) fn record(&mut self, cx: &mut PassCx<'_>) -> Result<Recorded, RenderError> {
        let mut recorded = Recorded { ran: [false; Pass::COUNT], cpu: PassTimes::default() };
        for pass in &mut self.passes {
            let which = pass.pass();
            if let Some(timer) = &self.timer {
                timer.write(cx.encoder, which, false);
            }
            cx.extra_cpu = std::time::Duration::ZERO;
            let start = Instant::now();
            let outcome = pass.record(cx)?;
            let elapsed = start.elapsed() + cx.extra_cpu;
            if let Some(timer) = &self.timer {
                timer.write(cx.encoder, which, true);
            }
            if outcome == Outcome::Ran {
                recorded.ran[which.index()] = true;
                recorded.cpu.set(which, Seconds(elapsed.as_secs_f64()));
            }
        }
        Ok(recorded)
    }

    /// Resolves this frame's GPU timestamps into the frame's commands. Call after [`record`](Self::record), before
    /// the encoder is finished.
    pub(crate) fn resolve_timings(&mut self, encoder: &mut wgpu::CommandEncoder, frame: u64, ran: [bool; Pass::COUNT]) {
        if let Some(timer) = &mut self.timer {
            timer.resolve(encoder, frame, ran);
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
