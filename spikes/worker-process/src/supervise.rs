//! Spawns the worker, kills it at seeded moments, respawns it, and keeps the timeline the analysis reads.
//!
//! Platform-neutral: how a frame reaches the output is the caller's `on_frame` hook, so the Linux lane (shared memory)
//! and the Windows lane (shared texture) run the same kill loop.

use std::io::Read;
use std::process::{Child, ChildStdout};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::Error;
use crate::protocol::{MSG_BYTES, Msg};
use crate::schedule::KillSchedule;

/// How long a fresh worker may take to post its first frame before the run is abandoned.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(60);

/// A frame the host accepted.
#[derive(Clone, Copy, Debug)]
pub struct Arrival {
    /// Worker number (1-based, one per spawn).
    pub worker: u32,
    /// The worker's frame counter.
    pub index: u64,
    /// When the frame was usable by the output, in ms since the run started.
    pub t_ms: f64,
    /// The worker's draw + readback time.
    pub render_ms: f64,
    /// The worker's copy-into-slot time.
    pub write_ms: f64,
}

/// A worker's "device and pipeline ready" message.
#[derive(Clone, Copy, Debug)]
pub struct Ready {
    /// Worker number.
    pub worker: u32,
    /// When it arrived, ms since the run started.
    pub t_ms: f64,
    /// Instance, adapter and device creation in the worker.
    pub gpu_ms: f64,
    /// Pipeline build in the worker.
    pub scene_ms: f64,
}

/// A worker was started.
#[derive(Clone, Copy, Debug)]
pub struct Spawn {
    /// Worker number.
    pub worker: u32,
    /// Just before the process was created, ms since the run started.
    pub t_ms: f64,
}

/// A worker was killed (SIGKILL / `TerminateProcess`) and reaped.
#[derive(Clone, Copy, Debug)]
pub struct Kill {
    /// Worker number.
    pub worker: u32,
    /// Just before the kill call, ms since the run started.
    pub t_ms: f64,
    /// False for the baseline worker, which is stopped at the end of the baseline phase and is not one of the kills.
    pub counted: bool,
}

/// Everything that happened, in ms since the run started.
#[derive(Debug, Default)]
pub struct Timeline {
    /// Accepted frames.
    pub arrivals: Vec<Arrival>,
    /// Ready messages.
    pub readies: Vec<Ready>,
    /// Spawns.
    pub spawns: Vec<Spawn>,
    /// Kills.
    pub kills: Vec<Kill>,
    /// Frames the host refused (a torn or stale slot).
    pub rejected: u64,
    /// When the baseline phase ended and the kill phase began.
    pub boundary_ms: Option<f64>,
}

/// The timeline plus the clock and the wake-up for waiters.
pub struct Shared {
    epoch: Instant,
    timeline: Mutex<Timeline>,
    changed: Condvar,
}

impl Shared {
    /// A fresh timeline whose zero is now.
    #[must_use]
    pub fn new() -> Self {
        Self { epoch: Instant::now(), timeline: Mutex::new(Timeline::default()), changed: Condvar::new() }
    }

    /// Milliseconds since the run started.
    #[must_use]
    pub fn now_ms(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64() * 1000.0
    }

    fn lock(&self) -> MutexGuard<'_, Timeline> {
        // A panicking reader thread poisons the lock; the data is still a valid log, so keep going and let the join report.
        self.timeline.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn update(&self, change: impl FnOnce(&mut Timeline)) {
        change(&mut self.lock());
        self.changed.notify_all();
    }

    /// Waits until `done` holds or `timeout` passes; returns whether it held.
    fn wait_for(&self, timeout: Duration, done: impl Fn(&Timeline) -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        let mut guard = self.lock();
        while !done(&guard) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            guard = self.changed.wait_timeout(guard, left).unwrap_or_else(std::sync::PoisonError::into_inner).0;
        }
        true
    }

    /// Takes the finished timeline. Call after every reader has been joined.
    pub fn take(&self) -> Timeline {
        std::mem::take(&mut *self.lock())
    }
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

/// What the loop does.
pub struct SuperviseConfig {
    /// Kills to perform.
    pub kills: u32,
    /// Seconds one worker runs undisturbed before the kills start (0 skips the phase).
    pub baseline: Duration,
    /// When, after a spawn, each kill falls.
    pub schedule: KillSchedule,
}

/// Called on the reader thread for each frame message; stores the frame for the output. Returns whether it was accepted.
pub type OnFrame = Arc<dyn Fn(u32, &Msg) -> bool + Send + Sync>;

/// A running worker. Dropping it kills and reaps the process, so no path leaves one behind.
struct Worker {
    number: u32,
    child: Child,
    reader: Option<JoinHandle<Result<(), Error>>>,
}

impl Worker {
    fn start(
        worker: u32,
        spawn: &mut impl FnMut(u32) -> std::io::Result<Child>,
        shared: &Arc<Shared>,
        on_frame: &OnFrame,
    ) -> Result<Self, Error> {
        let t_ms = shared.now_ms();
        let mut child = spawn(worker).map_err(Error::io("spawning the worker"))?;
        shared.update(|t| t.spawns.push(Spawn { worker, t_ms }));
        let Some(stdout) = child.stdout.take() else {
            // Reap the worker before reporting: it has no way to talk to us.
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Config("the worker was started without a piped stdout".into()));
        };
        let (shared, on_frame) = (Arc::clone(shared), Arc::clone(on_frame));
        let reader = thread::spawn(move || read_loop(stdout, worker, &shared, &on_frame));
        Ok(Self { number: worker, child, reader: Some(reader) })
    }

    /// SIGKILLs the worker, reaps it and waits for its reader to see the pipe close.
    fn stop(&mut self, shared: &Shared, counted: bool) -> Result<(), Error> {
        let t_ms = shared.now_ms();
        self.child.kill().map_err(Error::io("killing the worker"))?;
        self.child.wait().map_err(Error::io("reaping the worker"))?;
        shared.update(|t| t.kills.push(Kill { worker: self.number, t_ms, counted }));
        match self.reader.take() {
            Some(reader) => reader.join().map_err(|_| Error::Panicked)?,
            None => Ok(()),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Teardown on an error path: a failed kill means the process is already gone, and there is nothing else to try.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reads messages until the pipe closes. A closed pipe is the normal end of a killed worker.
fn read_loop(mut out: ChildStdout, worker: u32, shared: &Shared, on_frame: &OnFrame) -> Result<(), Error> {
    let mut bytes = [0u8; MSG_BYTES];
    loop {
        match out.read_exact(&mut bytes) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(Error::io("reading the worker's pipe")(e)),
        }
        let msg = Msg::decode(&bytes)?;
        match msg {
            Msg::Ready { gpu_ns, scene_ns } => {
                let t_ms = shared.now_ms();
                shared.update(|t| t.readies.push(Ready { worker, t_ms, gpu_ms: ms(gpu_ns), scene_ms: ms(scene_ns) }));
            }
            Msg::Frame { index, render_ns, write_ns, .. } => {
                let accepted = on_frame(worker, &msg);
                let t_ms = shared.now_ms();
                shared.update(|t| {
                    if accepted {
                        t.arrivals.push(Arrival {
                            worker,
                            index,
                            t_ms,
                            render_ms: ms(render_ns),
                            write_ms: ms(write_ns),
                        });
                    } else {
                        t.rejected += 1;
                    }
                });
            }
        }
    }
}

#[allow(clippy::cast_precision_loss)] // nanosecond counts far below 2^53
fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

/// Runs the baseline phase, then `config.kills` kill/respawn cycles, then one probe worker. `on_boundary` runs when the baseline ends.
///
/// # Errors
/// Fails when a worker cannot be started, produces no frame within a minute, or cannot be killed.
pub fn run(
    shared: &Arc<Shared>,
    mut config: SuperviseConfig,
    mut spawn: impl FnMut(u32) -> std::io::Result<Child>,
    on_frame: &OnFrame,
    on_boundary: impl FnOnce(),
) -> Result<(), Error> {
    let mut number = 0;
    if !config.baseline.is_zero() {
        number += 1;
        let mut worker = Worker::start(number, &mut spawn, shared, on_frame)?;
        wait_first_frame(shared, number)?;
        thread::sleep(config.baseline);
        worker.stop(shared, false)?;
        let t_ms = shared.now_ms();
        shared.update(|t| t.boundary_ms = Some(t_ms));
        on_boundary();
    }
    for _ in 0..config.kills {
        number += 1;
        let spawned = Instant::now();
        let mut worker = Worker::start(number, &mut spawn, shared, on_frame)?;
        thread::sleep(config.schedule.next_delay().saturating_sub(spawned.elapsed()));
        worker.stop(shared, true)?;
    }
    // One last worker, left alone until its first frame, so the final kill has a "first new frame" to measure to.
    number += 1;
    let mut worker = Worker::start(number, &mut spawn, shared, on_frame)?;
    wait_first_frame(shared, number)?;
    worker.stop(shared, false)
}

fn wait_first_frame(shared: &Shared, worker: u32) -> Result<(), Error> {
    if shared.wait_for(FIRST_FRAME_TIMEOUT, |t| t.arrivals.iter().any(|a| a.worker == worker)) {
        Ok(())
    } else {
        Err(Error::NoFrames { worker, secs: FIRST_FRAME_TIMEOUT.as_secs() })
    }
}
