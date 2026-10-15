//! Cooperative cancellation: the [`Cancel`] trait and the two handles every mock and adapter shares.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// A cancel handle a transport polls. Cancellation is cooperative: the owner flips it, the transport notices.
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

/// A settable flag. Clones share the one flag, so a caller keeps a clone to cancel while the transport holds another.
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
