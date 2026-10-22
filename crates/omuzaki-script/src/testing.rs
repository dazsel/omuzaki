//! Hooks for tests that drive this crate from outside it. Not part of the API.

use std::cell::RefCell;
use std::sync::Arc;

use omuzaki_core::time::{Clock, SystemClock};

thread_local! {
    static REPLAY_CLOCK: RefCell<Option<Arc<dyn Clock>>> = const { RefCell::new(None) };
}

/// Runs `f` with `clock` as the clock a macro replay on this thread reads, so a test needs no real waiting.
pub fn with_replay_clock<R>(clock: Arc<dyn Clock>, f: impl FnOnce() -> R) -> R {
    let before = REPLAY_CLOCK.with(|slot| slot.replace(Some(clock)));
    let result = f();
    REPLAY_CLOCK.with(|slot| *slot.borrow_mut() = before);
    result
}

/// The clock a macro replay reads: the test's, if one is set on this thread, else the system's.
pub(crate) fn replay_clock() -> Arc<dyn Clock> {
    REPLAY_CLOCK.with(|slot| slot.borrow().clone()).unwrap_or_else(|| Arc::new(SystemClock::new()))
}
