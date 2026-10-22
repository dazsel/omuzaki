//! The update hold of the live outputs (`docs/design/updates.md` §7; #1106): while any live output (a virtual camera,
//! a capture window, a stream) runs, the app holds one `Streaming` hold, so it never restarts into an update on air.
//!
//! [`LiveHolds`] is shared by every live output of a [`StreamService`](crate::service::StreamService). Each output is
//! wrapped in a [`HeldOutput`] with a hold taken by the caller (`output.start`, which has the updater). The first
//! wrapped output to start keeps its hold for all of them; the others' holds are released at their start. The last one
//! to stop, or to be dropped, releases the kept hold. The recorder and the replay buffer are not wrapped: they hold
//! `Recording` and `Capturing` themselves.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_stream_api::encoder::Frame;
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::output::{Output, OutputConfig, OutputInfo, OutputStatus, SubmitReport};
use omuzaki_update_api::hold::Hold;

/// The live outputs running, and the one hold they keep.
#[derive(Default)]
struct Shared {
    running: usize,
    hold: Option<Hold>,
}

/// The shared count behind every [`HeldOutput`] of one service. Cloning shares it.
#[derive(Clone, Default)]
pub struct LiveHolds {
    shared: Arc<Mutex<Shared>>,
}

impl std::fmt::Debug for LiveHolds {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let shared = lock(&self.shared);
        f.debug_struct("LiveHolds").field("running", &shared.running).field("held", &shared.hold.is_some()).finish()
    }
}

impl LiveHolds {
    /// No live output running yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Wraps `output` with `hold` (`None` in a build without an updater: every call then just forwards).
    pub fn wrap<O: Output>(&self, output: O, hold: Option<Hold>) -> HeldOutput<O> {
        self.wrap_boxed(Box::new(output), hold)
    }

    /// As [`wrap`](Self::wrap), for an output already boxed (a registered output kind's `make`).
    pub fn wrap_boxed<O: Output + ?Sized>(&self, output: Box<O>, hold: Option<Hold>) -> HeldOutput<O> {
        HeldOutput { inner: output, shared: Arc::clone(&self.shared), spare: hold, counted: false }
    }

    /// How many wrapped outputs run.
    pub fn running(&self) -> usize {
        lock(&self.shared).running
    }

    /// Whether the live outputs hold an update back now.
    pub fn held(&self) -> bool {
        lock(&self.shared).hold.is_some()
    }
}

/// A live output counted in its [`LiveHolds`]; every [`Output`] method forwards to the output inside.
pub struct HeldOutput<O: Output + ?Sized> {
    inner: Box<O>,
    shared: Arc<Mutex<Shared>>,
    /// The hold the caller took, until `start` either keeps it for everyone or releases it.
    spare: Option<Hold>,
    /// Whether this output is counted as running.
    counted: bool,
}

impl<O: Output + ?Sized> HeldOutput<O> {
    /// The output inside.
    pub fn inner(&self) -> &O {
        &self.inner
    }

    fn uncount(&mut self) {
        if !std::mem::take(&mut self.counted) {
            return;
        }
        let mut shared = lock(&self.shared);
        shared.running = shared.running.saturating_sub(1);
        if shared.running == 0 {
            // Released outside the lock: a hold's release runs the updater's code.
            let hold = shared.hold.take();
            drop(shared);
            drop(hold);
        }
    }
}

impl<O: Output + ?Sized> Output for HeldOutput<O> {
    fn info(&self) -> OutputInfo {
        self.inner.info()
    }

    fn start(&mut self, cfg: &OutputConfig) -> Result<(), StreamError> {
        let spare = self.spare.take();
        self.inner.start(cfg)?;
        if !self.counted {
            self.counted = true;
            let mut shared = lock(&self.shared);
            shared.running += 1;
            if shared.hold.is_none() {
                shared.hold = spare;
            }
        }
        Ok(())
    }

    fn submit(&mut self, frame: &Frame<'_>) -> Result<SubmitReport, StreamError> {
        self.inner.submit(frame)
    }

    fn status(&self) -> OutputStatus {
        self.inner.status()
    }

    fn stop(&mut self) {
        self.inner.stop();
        self.uncount();
    }
}

impl<O: Output + ?Sized> Drop for HeldOutput<O> {
    fn drop(&mut self) {
        self.uncount();
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}
