//! Spawns the worker, kills it at seeded moments, respawns it, and keeps the timeline the analysis reads.
//!
//! Platform-neutral: how a frame reaches the output is the caller's `on_frame` hook, so the Linux lane (shared memory)
//! and the Windows lane (shared texture) run the same kill loop.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, ChildStdout};
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

/// Why a worker was started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The undisturbed first worker (per-frame costs).
    Baseline,
    /// A worker left alone until its first frame, then stopped: an uncensored cold start.
    Cold,
    /// A worker the kill schedule may stop at any moment, startup included (its start-up samples are censored).
    Kill,
    /// The last worker, left alone until its first frame so the final kill has a frame to measure to.
    Probe,
}

/// A worker was started.
#[derive(Clone, Copy, Debug)]
pub struct Spawn {
    /// Worker number.
    pub worker: u32,
    /// Why it was started.
    pub phase: Phase,
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
    /// Workers whose pipe closed (or failed) and when, ms since the run started. A killed worker appears here too.
    pub exits: Vec<(u32, f64)>,
    /// When the baseline phase ended.
    pub boundary_ms: Option<f64>,
    /// When the kill phase began (after the baseline and the cold starts).
    pub kill_start_ms: Option<f64>,
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
    /// Workers started and stopped one at a time, with no kill before the first frame, for uncensored cold-start timings.
    pub cold_starts: u32,
    /// When, after a spawn, each kill falls.
    pub schedule: KillSchedule,
}

/// Phase boundaries the caller samples its own counters at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mark {
    /// The baseline worker has been stopped.
    BaselineEnd,
    /// The kills are about to start.
    KillStart,
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
        phase: Phase,
        spawn: &mut impl FnMut(u32) -> std::io::Result<Child>,
        shared: &Arc<Shared>,
        on_frame: &OnFrame,
    ) -> Result<Self, Error> {
        let t_ms = shared.now_ms();
        let mut child = spawn(worker).map_err(Error::io("spawning the worker"))?;
        shared.update(|t| t.spawns.push(Spawn { worker, phase, t_ms }));
        let (Some(stdout), Some(stdin)) = (child.stdout.take(), child.stdin.take()) else {
            // Reap the worker before reporting: it has no way to talk to us. A failed teardown is reported with the cause.
            let teardown = child.kill().and_then(|()| child.wait());
            return Err(match teardown {
                Ok(_) => Error::Config("the worker was started without piped stdin and stdout".into()),
                Err(e) => Error::io("stopping a worker that was started without pipes")(e),
            });
        };
        let (shared, on_frame) = (Arc::clone(shared), Arc::clone(on_frame));
        let reader = thread::spawn(move || read_loop(stdout, stdin, worker, &shared, &on_frame));
        Ok(Self { number: worker, child, reader: Some(reader) })
    }

    /// The error for a worker whose pipe closed without our killing it: when, and how the process ended.
    fn exited_early(&mut self, shared: &Shared) -> Error {
        let after_ms = shared.lock().exits.iter().find(|e| e.0 == self.number).map_or(0.0, |e| e.1);
        let reason = match self.child.try_wait() {
            Ok(Some(status)) => status.to_string(),
            Ok(None) => "its pipe closed but the process is still running".to_owned(),
            Err(e) => format!("its status could not be read: {e}"),
        };
        Error::WorkerExited { worker: self.number, at_ms: after_ms, reason }
    }

    fn has_exited(&self, shared: &Shared) -> bool {
        shared.lock().exits.iter().any(|e| e.0 == self.number)
    }

    /// Sleeps up to `duration`, failing at once (with the real exit time and reason) if the worker dies on its own.
    fn wait_or_exit(&mut self, shared: &Shared, duration: Duration) -> Result<(), Error> {
        let number = self.number;
        if shared.wait_for(duration, |t| t.exits.iter().any(|e| e.0 == number)) {
            return Err(self.exited_early(shared));
        }
        Ok(())
    }

    /// Waits for the worker's first accepted frame; a worker that dies first fails the run with its exit.
    fn await_first_frame(&mut self, shared: &Shared) -> Result<(), Error> {
        let number = self.number;
        shared.wait_for(FIRST_FRAME_TIMEOUT, |t| {
            t.arrivals.iter().any(|a| a.worker == number) || t.exits.iter().any(|e| e.0 == number)
        });
        if shared.lock().arrivals.iter().any(|a| a.worker == number) {
            Ok(())
        } else if self.has_exited(shared) {
            Err(self.exited_early(shared))
        } else {
            Err(Error::NoFrames { worker: number, secs: FIRST_FRAME_TIMEOUT.as_secs() })
        }
    }

    /// SIGKILLs the worker, reaps it and waits for its reader to see the pipe close.
    ///
    /// A worker that ended by itself in the moment between the schedule firing and the kill is a crash, not an injected
    /// kill: it is caught if its pipe had already closed, or, after the fact, if the status the reap returns is not our signal.
    fn stop(&mut self, shared: &Shared, counted: bool) -> Result<(), Error> {
        let t_ms = shared.now_ms();
        if self.has_exited(shared) {
            return Err(self.exited_early(shared));
        }
        self.child.kill().map_err(Error::io("killing the worker"))?;
        let status = self.child.wait().map_err(Error::io("reaping the worker"))?;
        if !killed_by_us(status) {
            // Its reader ends by itself now that the process is gone.
            return Err(Error::WorkerExited { worker: self.number, at_ms: t_ms, reason: status.to_string() });
        }
        shared.update(|t| t.kills.push(Kill { worker: self.number, t_ms, counted }));
        match self.reader.take() {
            Some(reader) => reader.join().map_err(|_| Error::Panicked)?,
            None => Ok(()),
        }
    }
}

/// Whether `status` is the end our SIGKILL causes. Windows `TerminateProcess` leaves no signal to tell it from an exit code,
/// so there the pipe check in `stop` is the only guard.
fn killed_by_us(status: std::process::ExitStatus) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        status.signal() == Some(9)
    }
    #[cfg(not(unix))]
    {
        let _ = status;
        true
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Teardown on an error path: a failed kill means the process is already gone, and there is nothing else to try.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Reads messages until the pipe closes, handing each slot back to the worker once the host has read it. A closed pipe is
/// the normal end of a killed worker; `wait_or_exit` tells that apart from a worker that died by itself.
fn read_loop(out: ChildStdout, ack: ChildStdin, worker: u32, shared: &Shared, on_frame: &OnFrame) -> Result<(), Error> {
    let result = read_messages(out, ack, worker, shared, on_frame);
    let t_ms = shared.now_ms();
    shared.update(|t| t.exits.push((worker, t_ms)));
    result
}

fn read_messages(
    mut out: ChildStdout,
    mut ack: ChildStdin,
    worker: u32,
    shared: &Shared,
    on_frame: &OnFrame,
) -> Result<(), Error> {
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
            Msg::Frame { slot, index, render_ns, write_ns } => {
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
                // The slot is the worker's again, accepted or not. A broken pipe means the worker is already gone.
                match ack.write_all(&crate::ack::encode(slot)).and_then(|()| ack.flush()) {
                    Ok(()) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => return Ok(()),
                    Err(e) => return Err(Error::io("acknowledging a slot")(e)),
                }
            }
        }
    }
}

#[allow(clippy::cast_precision_loss)] // nanosecond counts far below 2^53
fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

/// Runs the baseline phase, the cold starts, `config.kills` kill/respawn cycles, then one probe worker. `mark` is called
/// at each phase boundary.
///
/// # Errors
/// Fails when a worker cannot be started, dies by itself (the error names when and how), produces no frame within a
/// minute, or cannot be killed.
pub fn run(
    shared: &Arc<Shared>,
    mut config: SuperviseConfig,
    mut spawn: impl FnMut(u32) -> std::io::Result<Child>,
    on_frame: &OnFrame,
    mut mark: impl FnMut(Mark),
) -> Result<(), Error> {
    let mut number = 0;
    if !config.baseline.is_zero() {
        number += 1;
        let mut worker = Worker::start(number, Phase::Baseline, &mut spawn, shared, on_frame)?;
        worker.await_first_frame(shared)?;
        worker.wait_or_exit(shared, config.baseline)?;
        worker.stop(shared, false)?;
        let t_ms = shared.now_ms();
        shared.update(|t| t.boundary_ms = Some(t_ms));
        mark(Mark::BaselineEnd);
    }
    for _ in 0..config.cold_starts {
        number += 1;
        let mut worker = Worker::start(number, Phase::Cold, &mut spawn, shared, on_frame)?;
        worker.await_first_frame(shared)?;
        worker.stop(shared, false)?;
    }
    let t_ms = shared.now_ms();
    shared.update(|t| t.kill_start_ms = Some(t_ms));
    mark(Mark::KillStart);
    for _ in 0..config.kills {
        number += 1;
        let spawned = Instant::now();
        let mut worker = Worker::start(number, Phase::Kill, &mut spawn, shared, on_frame)?;
        worker.wait_or_exit(shared, config.schedule.next_delay().saturating_sub(spawned.elapsed()))?;
        worker.stop(shared, true)?;
    }
    number += 1;
    let mut worker = Worker::start(number, Phase::Probe, &mut spawn, shared, on_frame)?;
    worker.await_first_frame(shared)?;
    worker.stop(shared, false)
}

#[cfg(all(test, unix))]
mod tests {
    use std::process::{Command, Stdio};

    use super::*;

    #[test]
    fn a_worker_that_dies_by_itself_fails_the_run_with_its_exit() {
        let shared = Arc::new(Shared::new());
        let config = SuperviseConfig {
            kills: 1,
            baseline: Duration::ZERO,
            cold_starts: 0,
            // A kill 5 s out; the worker exits after 0.2 s, so the run must notice long before.
            schedule: KillSchedule::new(1, 5000, 5000),
        };
        let spawn = |_| {
            Command::new("sh").args(["-c", "sleep 0.2; exit 3"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()
        };
        let on_frame: OnFrame = Arc::new(|_, _| true);
        let started = Instant::now();
        let err = run(&shared, config, spawn, &on_frame, |_| {}).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(3), "noticed after {:?}", started.elapsed());
        match err {
            Error::WorkerExited { worker: 1, reason, .. } => assert!(reason.contains('3'), "{reason}"),
            other => panic!("wrong error: {other}"),
        }
        assert!(shared.take().kills.is_empty(), "an early exit must not be recorded as an injected kill");
    }

    #[test]
    fn a_worker_that_exited_just_before_the_kill_is_a_crash_not_a_kill() {
        let shared = Arc::new(Shared::new());
        let mut spawn =
            |_| Command::new("sh").args(["-c", "exit 4"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn();
        let on_frame: OnFrame = Arc::new(|_, _| true);
        let mut worker = Worker::start(1, Phase::Kill, &mut spawn, &shared, &on_frame).unwrap();
        // Let it exit by itself, then stop it as the schedule would, without having noticed the exit first.
        std::thread::sleep(Duration::from_millis(300));
        assert!(matches!(worker.stop(&shared, true), Err(Error::WorkerExited { worker: 1, .. })));
        assert!(shared.take().kills.is_empty());
    }

    #[test]
    fn a_status_that_is_not_our_signal_is_not_a_kill() {
        use std::os::unix::process::ExitStatusExt;
        assert!(killed_by_us(std::process::ExitStatus::from_raw(9)));
        assert!(!killed_by_us(std::process::ExitStatus::from_raw(4 << 8)));
        assert!(!killed_by_us(std::process::ExitStatus::from_raw(11)));
    }
}
