//! Workers as threads: a closure per worker. A panic is a crash. A thread can't be killed, so a hung one is
//! abandoned (its stop flag is set, it is counted) and replaced.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};

use super::{ExitInfo, LaunchError, WorkerHandle, WorkerLauncher};
use crate::watchdog::policy::{HeartbeatCell, WorkerId};

/// The exit code of a closure that returned.
const RETURNED: i32 = 0;
/// The exit code of a closure that panicked (Rust's own for a panicking main).
const PANICKED: i32 = 101;

/// What a thread worker sees.
#[derive(Clone, Debug)]
pub struct ThreadWorker {
    heartbeat: HeartbeatCell,
    stop: Arc<AtomicBool>,
}

impl ThreadWorker {
    /// One beat.
    pub fn beat(&self) {
        self.heartbeat.beat();
    }

    /// Whether the supervisor wants it to end (it was abandoned or the supervisor is stopping).
    pub fn should_stop(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
}

/// A thread worker's body.
pub type WorkerFn = Arc<dyn Fn(&ThreadWorker) + Send + Sync>;

/// Launches each worker by running its closure on a new thread.
#[derive(Clone, Default)]
pub struct ThreadLauncher {
    bodies: BTreeMap<WorkerId, WorkerFn>,
    abandoned: Arc<AtomicU64>,
}

impl std::fmt::Debug for ThreadLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadLauncher").field("workers", &self.bodies.len()).finish_non_exhaustive()
    }
}

impl ThreadLauncher {
    /// No workers yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// With worker `id` running `body`.
    #[must_use]
    pub fn with(mut self, id: WorkerId, body: impl Fn(&ThreadWorker) + Send + Sync + 'static) -> Self {
        self.bodies.insert(id, Arc::new(body));
        self
    }

    /// Hung threads abandoned so far.
    pub fn abandoned(&self) -> u64 {
        self.abandoned.load(Ordering::Acquire)
    }
}

/// Marks the thread ended, with a panic's code when it unwinds.
struct Ended {
    done: Arc<AtomicBool>,
    code: Arc<AtomicI32>,
}

impl Drop for Ended {
    fn drop(&mut self) {
        let code = if std::thread::panicking() { PANICKED } else { RETURNED };
        self.code.store(code, Ordering::Release);
        self.done.store(true, Ordering::Release);
    }
}

impl WorkerLauncher for ThreadLauncher {
    fn spawn(&self, id: WorkerId) -> Result<Box<dyn WorkerHandle>, LaunchError> {
        let body = Arc::clone(self.bodies.get(&id).ok_or(LaunchError::Unknown(id))?);
        let worker = ThreadWorker { heartbeat: HeartbeatCell::new(), stop: Arc::new(AtomicBool::new(false)) };
        let (done, code) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicI32::new(RETURNED)));
        let ended = Ended { done: Arc::clone(&done), code: Arc::clone(&code) };
        let inside = worker.clone();
        std::thread::Builder::new().name(format!("worker-{}", id.0)).spawn(move || {
            let _ended = ended;
            body(&inside);
        })?;
        Ok(Box::new(ThreadHandle {
            worker,
            done,
            code,
            abandoned: false,
            launcher_abandoned: Arc::clone(&self.abandoned),
        }))
    }
}

/// A running thread worker.
struct ThreadHandle {
    worker: ThreadWorker,
    done: Arc<AtomicBool>,
    code: Arc<AtomicI32>,
    abandoned: bool,
    launcher_abandoned: Arc<AtomicU64>,
}

impl WorkerHandle for ThreadHandle {
    fn heartbeat(&self) -> u64 {
        self.worker.heartbeat.read()
    }

    fn try_wait(&mut self) -> Option<ExitInfo> {
        if self.abandoned {
            return Some(ExitInfo { code: None });
        }
        self.done.load(Ordering::Acquire).then(|| ExitInfo { code: Some(self.code.load(Ordering::Acquire)) })
    }

    fn kill(&mut self) {
        if self.abandoned || self.done.load(Ordering::Acquire) {
            return;
        }
        self.worker.stop.store(true, Ordering::Release);
        self.abandoned = true;
        self.launcher_abandoned.fetch_add(1, Ordering::AcqRel);
    }

    fn pid(&self) -> Option<u32> {
        None
    }
}

impl Drop for ThreadHandle {
    fn drop(&mut self) {
        // Whatever ends the handle asks the thread to end too.
        self.worker.stop.store(true, Ordering::Release);
    }
}
