//! How the supervisor launches a worker generation: as a process speaking the worker protocol
//! ([`process::ProcessLauncher`]), as a thread ([`thread::ThreadLauncher`]) or as an async task
//! ([`task::TaskLauncher`]) (`docs/design/supervisor.md` §4.2, §15.4).
//!
//! Every spawn is a new **generation** with fresh shared objects, made by the worker's [`Peer`] (the host-side
//! companion that keeps the host's ends and takes the worker's messages). A process handle reads the worker's
//! messages on a thread of its own and wakes the supervisor at once on `Hello`, a fault and the end of the channel.

use std::sync::Arc;

use omuzaki_stream_api::supervisor::{WorkerId, WorkerKind};
use omuzaki_stream_api::worker::{ChannelHandle, ChannelNames, HostMsg, StartParams, WorkerMsg};

use super::policy::HeartbeatCell;

pub mod process;
pub mod task;
pub mod thread;

/// How an instance ended.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct ExitInfo {
    /// Its exit code; `None` when a signal killed it (or a hung thread was abandoned).
    pub code: Option<i32>,
}

/// Why a generation didn't start, or a message didn't go.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LaunchError {
    /// The OS refused (no executable, out of resources), or the shared objects could not be made. Retrying may help.
    #[error("the worker didn't start: {what}")]
    Spawn {
        /// What failed.
        what: String,
    },
    /// The worker broke the protocol.
    #[error("protocol fault: {what}")]
    Protocol {
        /// What was wrong.
        what: String,
    },
    /// The control channel is closed: the worker has gone.
    #[error("the worker's control channel is closed")]
    Closed,
    /// Retrying cannot help: a protocol version or generation mismatch, a kind that does not match its parameters.
    #[error("the worker can't start: {what}")]
    Permanent {
        /// Why.
        what: String,
    },
}

impl LaunchError {
    /// `Spawn { what }`.
    pub fn spawn(what: impl Into<String>) -> Self {
        Self::Spawn { what: what.into() }
    }

    /// `Permanent { what }`.
    pub fn permanent(what: impl Into<String>) -> Self {
        Self::Permanent { what: what.into() }
    }

    /// Whether retrying cannot help.
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent { .. })
    }
}

/// What a handle's reader found wrong with its instance.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Fault {
    /// A malformed control frame: the instance is killed and restarted with the reason `Protocol`.
    Protocol(String),
    /// The handshake failed (the channel ended before `Hello`, a bad `Hello`, or `Gone`): a failed launch.
    Launch {
        /// What failed.
        what: String,
        /// Retrying cannot help.
        permanent: bool,
    },
}

/// One generation's start: its shared objects (the handles in the order of their `ChannelRef::Fd` positions) and the
/// kind's parameters.
#[derive(Debug)]
pub struct Start {
    /// The objects' roles and references, as `HostMsg::Start` carries them.
    pub channels: ChannelNames,
    /// The host's handles to the objects, in position order; sent with `Start` and then dropped.
    pub handles: Vec<ChannelHandle>,
    /// What the kind needs to start.
    pub params: StartParams,
}

impl Start {
    /// A start with no shared objects.
    pub fn plain(generation: u32, params: StartParams) -> Self {
        Self { channels: ChannelNames::new(generation, Vec::new()), handles: Vec::new(), params }
    }
}

/// The host-side companion of one worker: makes each generation's shared objects (keeping the host's ends) and takes
/// the messages the worker sends besides beats (`Opened`, `Ready`, `Frame`, `SlotFree`, `Gone`). Called on the
/// supervisor thread (`prepare`) and on the handle's reader thread (`message`): never blocks for long.
pub trait Peer: Send + Sync {
    /// The shared objects and parameters of `generation`.
    ///
    /// # Errors
    /// [`LaunchError`] when the objects cannot be made.
    fn prepare(&self, generation: u32) -> Result<Start, LaunchError>;

    /// A message from `generation`.
    fn message(&self, generation: u32, msg: WorkerMsg) {
        let _ = (generation, msg);
    }
}

/// A peer with no shared objects and fixed parameters.
#[derive(Debug)]
pub struct Plain(pub StartParams);

impl Peer for Plain {
    fn prepare(&self, generation: u32) -> Result<Start, LaunchError> {
        Ok(Start::plain(generation, self.0.clone()))
    }
}

/// What a handle's reader tells the supervisor, at once.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum WakeKind {
    /// A good `Hello`: send `Start`.
    Hello,
    /// The reader found a fault: kill the instance.
    Fault,
    /// The control channel ended: the instance is ending.
    Ended,
}

/// A reader's wake-up for the supervisor: which worker, which generation, what.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Wake {
    /// The worker.
    pub id: WorkerId,
    /// Its generation.
    pub generation: u32,
    /// What happened.
    pub kind: WakeKind,
}

/// Wakes the supervisor.
#[derive(Clone)]
pub struct Waker(Arc<dyn Fn(Wake) + Send + Sync>);

impl Waker {
    /// A waker that calls `f`.
    pub fn new(f: impl Fn(Wake) + Send + Sync + 'static) -> Self {
        Self(Arc::new(f))
    }

    /// A waker that does nothing (tests of a handle alone).
    pub fn none() -> Self {
        Self::new(|_| {})
    }

    /// Wakes the supervisor.
    pub fn wake(&self, wake: Wake) {
        (self.0)(wake);
    }
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waker")
    }
}

/// The write side of one generation's control channel. Shared with callers, so a command goes out without a hop
/// through the supervisor thread.
pub trait ControlSend: Send + Sync {
    /// Sends `msg` with `handles` attached (`Start` and `Attach` only; empty for every other message).
    ///
    /// # Errors
    /// [`LaunchError::Closed`] when the worker has gone; [`LaunchError::Protocol`] for handles on a message that
    /// carries none.
    fn send(&self, msg: &HostMsg, handles: &[&ChannelHandle]) -> Result<(), LaunchError>;
}

/// Starts generations of one worker.
pub trait WorkerLauncher: Send + Sync {
    /// The kind it starts.
    fn kind(&self) -> WorkerKind;

    /// Starts `generation` of worker `id`; its reader wakes the supervisor through `waker`.
    ///
    /// # Errors
    /// [`LaunchError`] when it can't.
    fn spawn(&self, id: WorkerId, generation: u32, waker: Waker) -> Result<Box<dyn WorkerHandle>, LaunchError>;
}

/// One running generation.
pub trait WorkerHandle: Send {
    /// Its progress and work counters, set by its reader from `Beat`.
    fn heartbeat(&self) -> &HeartbeatCell;
    /// `Some` once it has ended (crash, kill or exit); it is reaped by then.
    fn try_wait(&mut self) -> Option<ExitInfo>;
    /// Kills it (SIGKILL); a thread or task is asked to stop. Never waits.
    fn kill(&mut self);
    /// Whether it must say `Hello` before `start` (a process); a thread starts at once.
    fn needs_hello(&self) -> bool;
    /// Sends the generation's `Start` with its objects.
    ///
    /// # Errors
    /// [`LaunchError`] when the channel has closed.
    fn start(&mut self) -> Result<(), LaunchError>;
    /// Its control channel's write side.
    fn control(&self) -> Arc<dyn ControlSend>;
    /// Its process id, for a process.
    fn pid(&self) -> Option<u32>;
    /// Whether it can only be abandoned, never killed (threads and tasks).
    fn revocable(&self) -> bool;
    /// What its reader found wrong, if anything.
    fn fault(&self) -> Option<Fault>;
}
