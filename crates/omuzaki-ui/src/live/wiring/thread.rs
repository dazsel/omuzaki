//! [`PerformThread`]: the perform loop on its own thread, at the performer's fixed step.
//!
//! The thread owns the [`Performer`] and its [`PerformControl`]; nothing else touches them. Each pass it swaps in the
//! newest frame source the input posted ([`SourceSlot::swap_into`]: a new camera, or no capture), lets the control take
//! the ops' requests (settings, calibration, triggers) and run the due steps, publishes the newest step's [`LiveFrame`]
//! into the mailbox, and sleeps until the next step is due. In steady state a pass neither locks nor allocates
//! (`tests/live_wiring.rs` counts it); the exceptions are the ops' requests, which arrive between frames (a settings
//! change runs the retarget's `set_config`). A swapped-out source goes back to the slot and is dropped on the ops side.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;

use omuzaki_core::time::Seconds;
use omuzaki_stage::perform::host::{PerformControl, PerformHandle, PerformHost, perform_channel};
use omuzaki_stage::perform::input::SourceSlot;
use omuzaki_stage::perform::{Performer, STEP_HZ};

use super::clock::PerformClock;
use super::mailbox::{FrameReader, FrameWriter, LiveFrame, frame_mailbox};

/// A consumer of every published frame, called on the perform thread right after the frame is published (the
/// `VTube Studio` bridge's feed). It must not block or allocate in steady state: it runs inside the 60 Hz step.
pub type FrameTap = Box<dyn FnMut(&LiveFrame) + Send>;

/// The perform thread's name, as debuggers and profilers show it.
pub const THREAD_NAME: &str = "omuzaki-perform";

/// Why the wiring refused a request.
#[derive(Debug)]
#[non_exhaustive]
pub enum WiringError {
    /// The OS would not start the thread.
    Spawn(std::io::Error),
    /// The perform thread has stopped.
    Stopped,
    /// The perform thread panicked; the performer is gone.
    Panicked,
}

impl std::fmt::Display for WiringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "the perform thread could not start: {error}"),
            Self::Stopped => f.write_str("the performer is not running"),
            Self::Panicked => f.write_str("the perform thread stopped with a panic"),
        }
    }
}

impl std::error::Error for WiringError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn(error) => Some(error),
            _ => None,
        }
    }
}

/// What the loop has done so far, read without waiting.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct LoopReport {
    /// Passes that ran at least one step (and published its frame).
    pub published: u64,
    /// Steps run, the catch-up steps of a late pass included: a pass that wakes two steps late runs both and publishes
    /// once, so this, not `published`, is what adds up with `skipped` to the steps that fell due.
    pub stepped: u64,
    /// Steps the control dropped because the loop fell more than four steps behind.
    pub skipped: u64,
}

#[derive(Default)]
struct Stats {
    published: AtomicU64,
    stepped: AtomicU64,
    skipped: AtomicU64,
}

impl Stats {
    fn report(&self) -> LoopReport {
        LoopReport {
            published: self.published.load(Ordering::Acquire),
            stepped: self.stepped.load(Ordering::Acquire),
            skipped: self.skipped.load(Ordering::Acquire),
        }
    }
}

/// The perform thread and the ways to reach it. Dropping it stops the thread and waits for it.
pub struct PerformThread {
    handle: PerformHandle,
    sources: Arc<SourceSlot>,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    thread: Option<JoinHandle<()>>,
}

impl PerformThread {
    /// Starts `performer` on its own thread, timed by `clock`, and returns the thread with a reader of the frames it
    /// publishes. The performer keeps the source it was built with until the input posts another into
    /// [`sources`](Self::sources). Cold: allocates the slot, the mailbox and the thread.
    ///
    /// # Errors
    /// [`WiringError::Spawn`] if the OS refuses the thread.
    pub fn spawn(performer: Performer, clock: Box<dyn PerformClock>) -> Result<(Self, FrameReader), WiringError> {
        Self::spawn_tapped(performer, clock, None)
    }

    /// [`spawn`](Self::spawn), with `tap` handed each frame as it is published.
    ///
    /// # Errors
    /// [`WiringError::Spawn`] if the OS refuses the thread.
    pub fn spawn_tapped(
        performer: Performer,
        clock: Box<dyn PerformClock>,
        tap: Option<FrameTap>,
    ) -> Result<(Self, FrameReader), WiringError> {
        let (handle, control) = perform_channel(&performer);
        let (writer, reader) = frame_mailbox();
        let sources = Arc::new(SourceSlot::new());
        let (stop, stats) = (Arc::new(AtomicBool::new(false)), Arc::new(Stats::default()));
        let mut run = PerformLoop {
            performer,
            control,
            writer,
            sources: Arc::clone(&sources),
            stats: Arc::clone(&stats),
            arrival: None,
            last: None,
            tap,
        };
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || run.run(clock, &flag))
            .map_err(WiringError::Spawn)?;
        Ok((Self { handle, sources, stop, stats, thread: Some(thread) }, reader))
    }

    /// The slot the input posts frame sources into (`LiveInput`, the app's `InputHost`); the loop takes the newest
    /// at its next pass.
    pub fn sources(&self) -> Arc<SourceSlot> {
        Arc::clone(&self.sources)
    }

    /// The performer as the ops see it: inject it into the runner's services as `Arc<dyn PerformHost>`.
    pub fn host(&self) -> Arc<dyn PerformHost> {
        Arc::new(self.handle.clone())
    }

    /// The handle itself, for a caller that waits on a settings change ([`PerformHandle::await_retarget`]).
    pub fn handle(&self) -> &PerformHandle {
        &self.handle
    }

    /// What the loop has done so far.
    pub fn report(&self) -> LoopReport {
        self.stats.report()
    }

    /// Whether the thread is still running.
    pub fn is_running(&self) -> bool {
        self.thread.as_ref().is_some_and(|thread| !thread.is_finished())
    }

    /// Stops the loop after its current pass and waits for the thread. The performer and its source are dropped on
    /// the perform thread; from then on the ops' handle answers `Disconnected`.
    ///
    /// # Errors
    /// [`WiringError::Panicked`] if the thread had panicked.
    pub fn stop(mut self) -> Result<LoopReport, WiringError> {
        self.join()?;
        Ok(self.stats.report())
    }

    fn join(&mut self) -> Result<(), WiringError> {
        self.stop.store(true, Ordering::Release);
        match self.thread.take() {
            Some(thread) => thread.join().map_err(|_| WiringError::Panicked),
            None => Ok(()),
        }
    }
}

impl Drop for PerformThread {
    fn drop(&mut self) {
        if let Err(error) = self.join() {
            tracing::warn!(%error, "the perform thread ended badly");
        }
    }
}

impl std::fmt::Debug for PerformThread {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PerformThread").field("report", &self.report()).finish_non_exhaustive()
    }
}

/// The steps a pass ran that ended on step `index` with `skipped` steps dropped so far, after one that ended on `last`:
/// the steps since, less the ones dropped since. A reset (a new source) starts the timeline and its skip count again.
fn steps_run(last: Option<(u64, u64)>, index: u64, skipped: u64) -> u64 {
    match last {
        Some((before, dropped)) if index > before && skipped >= dropped => (index - before) - (skipped - dropped),
        _ => (index + 1).saturating_sub(skipped),
    }
}

/// Everything the perform thread owns.
struct PerformLoop {
    performer: Performer,
    control: PerformControl,
    writer: FrameWriter,
    sources: Arc<SourceSlot>,
    stats: Arc<Stats>,
    /// When the newest tracker frame arrived, on the loop's clock.
    arrival: Option<Seconds>,
    /// The last step this loop ran and the control's skip count then, to count the steps each pass runs.
    last: Option<(u64, u64)>,
    /// Handed each published frame.
    tap: Option<FrameTap>,
}

impl PerformLoop {
    /// Passes until `stop` is set: each one runs what is due, then waits for the next step boundary.
    fn run(&mut self, mut clock: Box<dyn PerformClock>, stop: &AtomicBool) {
        let period = 1.0 / f64::from(STEP_HZ);
        let origin = clock.now().0;
        while !stop.load(Ordering::Acquire) {
            let now = clock.now();
            self.pass(now);
            // The next boundary after `now`, on the grid that starts at the loop's origin.
            let next = ((now.0 - origin) / period).floor() + 1.0;
            clock.wait_until(Seconds(origin + next * period));
        }
    }

    /// One pass at `now`.
    fn pass(&mut self, now: Seconds) {
        // Lock-free and allocation-free: the source it replaces goes back into the slot for the ops side to drop.
        self.sources.swap_into(&mut self.performer);
        if let Some(out) = self.control.advance(&mut self.performer, now) {
            if out.frame.is_some() {
                self.arrival = Some(now);
            }
            let frame = LiveFrame::from_output(&out, self.arrival);
            self.writer.publish(&frame);
            if let Some(tap) = &mut self.tap {
                tap(&frame);
            }
            self.stats.published.fetch_add(1, Ordering::Release);
            let (index, skipped) = (out.tick.index, self.control.skipped_steps());
            self.stats.stepped.fetch_add(steps_run(self.last, index, skipped), Ordering::Release);
            self.last = Some((index, skipped));
        }
        self.stats.skipped.store(self.control.skipped_steps(), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::steps_run;

    #[test]
    fn a_late_pass_counts_its_catch_up_steps_and_a_reset_starts_again() {
        assert_eq!(steps_run(None, 0, 0), 1, "the first step");
        assert_eq!(steps_run(Some((0, 0)), 1, 0), 1, "on time");
        assert_eq!(steps_run(Some((1, 0)), 3, 0), 2, "two steps late, it ran both");
        assert_eq!(steps_run(Some((3, 0)), 10, 3), 4, "seven late: four ran and three were skipped");
        assert_eq!(steps_run(Some((10, 3)), 2, 0), 3, "a new source restarts the timeline at step 0");
    }
}
