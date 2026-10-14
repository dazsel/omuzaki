//! Locking helper shared by the executor's two mutexes.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Locks `mutex`, recovering the data if another thread panicked while holding it.
///
/// Recovering is sound here because the executor's state is never left half-updated: ops plan before anything
/// changes, `DocWrite::apply` is atomic, and the bookkeeping after it is infallible. A panic inside an op's `plan`
/// therefore must not brick the whole authoring session for every other surface.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}
