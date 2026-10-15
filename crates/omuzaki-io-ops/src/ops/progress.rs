//! The adapter from a job's context to the io crate's progress trait.
//!
//! An io op that runs as a job reports progress and checks cancellation through [`JobProgress`], which forwards to the
//! job's [`JobCtx`]. The io trait stays a separate type because the io crate is used outside jobs too (the CLI importing
//! a file), so this adapter is the one place the two meet (ADR 0036, "Consequences").

use omuzaki_io_api::Progress;
use omuzaki_ops_api::JobCtx;

/// Adapts a job's context to the io crate's progress trait.
///
/// Stage reports go to the job's signal, which the executor coalesces, and cancellation is the job's own flag. So the
/// trait's `checkpoint` reports and then fails with `IoError::Cancelled` once the job is cancelled.
pub struct JobProgress<'a, 'b>(pub &'a JobCtx<'b>);

impl Progress for JobProgress<'_, '_> {
    fn stage(&self, name: &str, fraction: f32) {
        self.0.progress(name, fraction);
    }

    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}
