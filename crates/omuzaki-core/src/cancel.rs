//! Cooperative cancellation, defined once for every crate (#887): the [`Cancel`] trait a long piece of work polls, a
//! handle that never cancels, and a settable flag whose clones share one state.
//!
//! `omuzaki-ops-api` (jobs), `omuzaki-http-api` (transports), `omuzaki-git-api` and `omuzaki-script` re-export these
//! under their own paths, so a job's flag reaches an HTTP transport or a script run as it is.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A cancel handle that work polls. Cancellation is cooperative: the owner flips it, the work notices.
pub trait Cancel: Send + Sync {
    /// True once the work this handle guards should stop.
    fn is_cancelled(&self) -> bool;
}

/// A handle that is never cancelled, for callers with nothing to stop.
#[derive(Clone, Copy, Debug, Default)]
pub struct NeverCancel;

impl Cancel for NeverCancel {
    fn is_cancelled(&self) -> bool {
        false
    }
}

/// A settable flag. Clones share the one flag, so a caller keeps a clone to cancel while the work holds another.
#[derive(Clone, Debug, Default)]
pub struct CancelFlag(Arc<AtomicBool>);

impl CancelFlag {
    /// A flag that is not cancelled.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Cancel: every clone of this flag now reads true. It cannot be undone.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Cancel for CancelFlag {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_is_never_cancelled() {
        assert!(!NeverCancel.is_cancelled());
    }

    #[test]
    fn a_flag_starts_clear_and_every_clone_sees_the_cancel() {
        let flag = CancelFlag::new();
        let held = flag.clone();
        assert!(!held.is_cancelled());
        flag.cancel();
        assert!(held.is_cancelled() && flag.is_cancelled());
        let on_thread = std::thread::spawn(move || held.is_cancelled());
        assert!(on_thread.join().unwrap_or(false), "another thread sees it");
    }

    #[test]
    fn a_handle_works_as_a_trait_object() {
        let handles: [&dyn Cancel; 2] = [&NeverCancel, &CancelFlag::new()];
        assert!(handles.iter().all(|h| !h.is_cancelled()));
    }
}
