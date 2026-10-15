//! Spike: the renderer in a killable worker process.
//!
//! A **host** process owns the output and holds the last complete frame; a **worker** process renders the test picture
//! and posts `(slot, frame index)` over a pipe. The host kills the worker at seeded random moments, respawns it and
//! measures what the output saw. Linux runs the CPU lane (frames in shared memory, lavapipe renderer); Windows runs the
//! DX12 lane (a shared texture and a shared fence, then Spout), which is compile-checked only so far.

pub mod ack;
pub mod analysis;
pub mod options;
pub mod pacing;
pub mod protocol;
pub mod schedule;
pub mod supervise;

#[cfg(target_os = "linux")]
pub mod cpu_host;
#[cfg(target_os = "linux")]
pub mod cpu_worker;
#[cfg(target_os = "linux")]
pub mod proc_stats;
#[cfg(target_os = "linux")]
pub mod shm;
#[cfg(windows)]
pub mod win;

/// Why a host or worker run failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file, pipe or process call failed.
    #[error("{what}: {source}")]
    Io {
        /// What was being done.
        what: String,
        /// The OS error.
        source: std::io::Error,
    },
    /// No GPU.
    #[error(transparent)]
    Gpu(#[from] spout_out::gpu::GpuError),
    /// The scene could not be made or read back.
    #[error(transparent)]
    Scene(#[from] spout_out::scene::SceneError),
    /// The worker sent something that is not a message.
    #[error(transparent)]
    Protocol(#[from] protocol::ProtocolError),
    /// A worker produced no frame in time.
    #[error("worker {worker} produced no frame within {secs} s")]
    NoFrames {
        /// The worker's generation number.
        worker: u32,
        /// The wait that ran out.
        secs: u64,
    },
    /// A worker ended by itself, which is a renderer failure and not one of the scheduled kills.
    #[error("worker {worker} exited by itself {at_ms:.0} ms into the run: {reason}")]
    WorkerExited {
        /// The worker's number.
        worker: u32,
        /// When its pipe closed, ms since the run started.
        at_ms: f64,
        /// How the process ended.
        reason: String,
    },
    /// A bad command line.
    #[error("{0}")]
    Config(String),
    /// A helper thread panicked.
    #[error("a helper thread panicked")]
    Panicked,
    /// A Direct3D call failed.
    #[cfg(windows)]
    #[error("{what}: {source}")]
    Win32 {
        /// The call that failed.
        what: &'static str,
        /// The HRESULT error.
        source: windows::core::Error,
    },
}

impl Error {
    /// Wraps an I/O error with what was being done.
    pub fn io(what: impl Into<String>) -> impl FnOnce(std::io::Error) -> Self {
        move |source| Self::Io { what: what.into(), source }
    }
}
