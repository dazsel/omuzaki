//! How the supervisor launches a worker: as a process ([`process::ProcessLauncher`]) or as a thread
//! ([`thread::ThreadLauncher`]).
pub mod process;
pub mod thread;

use super::policy::WorkerId;

/// How an instance ended.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ExitInfo {
    /// Its exit code; `None` when a signal killed it (or a hung thread was abandoned).
    pub code: Option<i32>,
}

/// Why an instance didn't start.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    /// The launcher has nothing for this worker.
    #[error("no launch command for worker {0:?}")]
    Unknown(WorkerId),
    /// The OS refused.
    #[error("the worker didn't start: {0}")]
    Spawn(#[from] std::io::Error),
}

/// Starts instances of workers.
pub trait WorkerLauncher: Send + Sync {
    /// Starts an instance of worker `id`.
    ///
    /// # Errors
    /// [`LaunchError`] when it can't.
    fn spawn(&self, id: WorkerId) -> Result<Box<dyn WorkerHandle>, LaunchError>;
}

/// One running instance.
pub trait WorkerHandle: Send {
    /// Its heartbeat counter, which the monitor compares.
    fn heartbeat(&self) -> u64;
    /// `Some` once it has ended (crash, kill or exit); it is reaped by then.
    fn try_wait(&mut self) -> Option<ExitInfo>;
    /// Kills it (SIGKILL, `TerminateProcess`); never waits.
    fn kill(&mut self);
    /// Its process id, for a process.
    fn pid(&self) -> Option<u32>;
}
