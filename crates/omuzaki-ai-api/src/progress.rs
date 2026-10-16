//! Progress and cancellation for long registry work (downloads, adding a model, evaluations).

/// Receives progress from long work and lets the caller stop it. Same shape as `omuzaki_io_api::Progress`, and the two
/// stay separate types. An ai op that runs as a job (`model.add`) passes an adapter over its `JobCtx`, as
/// `omuzaki_io_ops::ops::progress::JobProgress` does for io.
pub trait Progress: Send + Sync {
    /// Reports that `stage` is `fraction` (0..=1) done.
    fn stage(&self, name: &str, fraction: f32);
    /// True once the caller wants the work stopped; long loops check it between chunks.
    fn is_cancelled(&self) -> bool;
}

/// Progress that goes nowhere and never cancels.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoProgress;

impl Progress for NoProgress {
    fn stage(&self, _: &str, _: f32) {}
    fn is_cancelled(&self) -> bool {
        false
    }
}
