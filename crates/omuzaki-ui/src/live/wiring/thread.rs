//! [`PerformThread`]: the perform loop on its own thread, at the performer's fixed step.
//!
//! The thread owns the [`Performer`] and its [`PerformControl`]; nothing else touches them. Each pass it takes at most
//! [`MAX_LOOP_COMMANDS`] requests of its own (a new frame source), lets the control take the ops' requests (settings,
//! calibration, triggers) and run the due steps, publishes the newest step's [`LiveFrame`] into the mailbox, and sleeps
//! until the next step is due. In steady state a pass neither locks nor allocates (`tests/live_wiring.rs` counts it);
//! the exceptions are the requests themselves, which arrive between frames: a new source frees the old one, and a
//! settings change runs the retarget's `set_config`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

use omuzaki_anim_api::FrameSource;
use omuzaki_core::time::Seconds;
use omuzaki_stage::perform::host::{PerformControl, PerformHandle, PerformHost, perform_channel};
use omuzaki_stage::perform::{Performer, STEP_HZ};

use super::clock::PerformClock;
use super::mailbox::{FrameReader, FrameWriter, LiveFrame, frame_mailbox};

/// Requests the loop's own queue holds before [`WiringError::Busy`].
pub const LOOP_QUEUE_LEN: usize = 8;
/// Requests one pass takes from the loop's own queue; the rest wait for the next pass.
pub const MAX_LOOP_COMMANDS: usize = 2;
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
    /// The loop has not caught up with the requests already queued (it holds [`LOOP_QUEUE_LEN`]).
    Busy,
    /// The perform thread panicked; the performer is gone.
    Panicked,
}

impl std::fmt::Display for WiringError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(error) => write!(f, "the perform thread could not start: {error}"),
            Self::Stopped => f.write_str("the performer is not running"),
            Self::Busy => f.write_str("the performer has too many requests waiting; try again in a moment"),
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

/// A request to the loop itself (the ops' requests, triggers included, go through the [`PerformHandle`]).
enum LoopCommand {
    Source(Box<dyn FrameSource>),
}

/// What the loop has done so far, read without waiting.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct LoopReport {
    /// Passes that ran at least one step (and published its frame).
    pub published: u64,
    /// Steps the control dropped because the loop fell more than four steps behind.
    pub skipped: u64,
}

#[derive(Default)]
struct Stats {
    published: AtomicU64,
    skipped: AtomicU64,
}

impl Stats {
    fn report(&self) -> LoopReport {
        LoopReport { published: self.published.load(Ordering::Acquire), skipped: self.skipped.load(Ordering::Acquire) }
    }
}

/// The perform thread and the ways to reach it. Dropping it stops the thread and waits for it.
pub struct PerformThread {
    handle: PerformHandle,
    commands: SyncSender<LoopCommand>,
    stop: Arc<AtomicBool>,
    stats: Arc<Stats>,
    thread: Option<JoinHandle<()>>,
}

impl PerformThread {
    /// Starts `performer` on its own thread, timed by `clock`, and returns the thread with a reader of the frames it
    /// publishes. Cold: allocates the channels and the thread.
    ///
    /// # Errors
    /// [`WiringError::Spawn`] if the OS refuses the thread.
    pub fn spawn(performer: Performer, clock: Box<dyn PerformClock>) -> Result<(Self, FrameReader), WiringError> {
        let (handle, control) = perform_channel(&performer);
        let (writer, reader) = frame_mailbox();
        let (commands, inbox) = sync_channel(LOOP_QUEUE_LEN);
        let (stop, stats) = (Arc::new(AtomicBool::new(false)), Arc::new(Stats::default()));
        let mut run = PerformLoop { performer, control, writer, inbox, stats: Arc::clone(&stats) };
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name(THREAD_NAME.to_owned())
            .spawn(move || run.run(clock, &flag))
            .map_err(WiringError::Spawn)?;
        Ok((Self { handle, commands, stop, stats, thread: Some(thread) }, reader))
    }

    /// The performer as the ops see it: inject it into the runner's services as `Arc<dyn PerformHost>`.
    pub fn host(&self) -> Arc<dyn PerformHost> {
        Arc::new(self.handle.clone())
    }

    /// The handle itself, for a caller that waits on a settings change ([`PerformHandle::await_retarget`]).
    pub fn handle(&self) -> &PerformHandle {
        &self.handle
    }

    /// Switches the performer to `source` (a new camera, or the old one back after an unplug). The old source is
    /// dropped on the perform thread.
    ///
    /// # Errors
    /// [`WiringError::Busy`] or [`WiringError::Stopped`]; the source is dropped then.
    pub fn set_source(&self, source: Box<dyn FrameSource>) -> Result<(), WiringError> {
        self.send(LoopCommand::Source(source))
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

    fn send(&self, command: LoopCommand) -> Result<(), WiringError> {
        self.commands.try_send(command).map_err(|e| match e {
            TrySendError::Full(_) => WiringError::Busy,
            TrySendError::Disconnected(_) => WiringError::Stopped,
        })
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

/// Everything the perform thread owns.
struct PerformLoop {
    performer: Performer,
    control: PerformControl,
    writer: FrameWriter,
    inbox: Receiver<LoopCommand>,
    stats: Arc<Stats>,
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
        for _ in 0..MAX_LOOP_COMMANDS {
            let Ok(command) = self.inbox.try_recv() else { break };
            self.apply(command);
        }
        if let Some(out) = self.control.advance(&mut self.performer, now) {
            self.writer.publish(&LiveFrame::from_output(&out));
            self.stats.published.fetch_add(1, Ordering::Release);
        }
        self.stats.skipped.store(self.control.skipped_steps(), Ordering::Release);
    }

    fn apply(&mut self, command: LoopCommand) {
        match command {
            LoopCommand::Source(source) => self.performer.set_source(source),
        }
    }
}
