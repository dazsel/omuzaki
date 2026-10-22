//! Workers as async tasks (the studio's platform adapters, `docs/design/supervisor.md` §2, §15.4): an async body per
//! worker on the studio's runtime. A panic is a crash. `kill` aborts the task, which ends at its next `.await`; a task
//! stuck in blocking code never reaches one, so it is abandoned after `exit_wait` (counted and replaced), which is safe
//! because it holds nothing exclusive: its generation's messages and objects are its own.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::FutureExt as _;
use omuzaki_stream_api::supervisor::{WorkerId, WorkerKind};
use omuzaki_stream_api::worker::{ChannelHandle, HostMsg};
use tokio::runtime::Handle;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;

use super::{ControlSend, ExitInfo, Fault, LaunchError, Peer, Start, Waker, WorkerHandle, WorkerLauncher};
use crate::watchdog::policy::HeartbeatCell;

/// The exit code of a body that returned or was aborted.
const RETURNED: i32 = 0;
/// The exit code of a body that panicked (as a thread's).
const PANICKED: i32 = 101;

/// What a task worker sees.
#[derive(Debug)]
pub struct TaskWorker {
    heartbeat: HeartbeatCell,
    inbox: UnboundedReceiver<HostMsg>,
    generation: u32,
}

impl TaskWorker {
    /// One beat.
    pub fn beat(&self) {
        self.heartbeat.beat();
    }

    /// Sets the work counter.
    pub fn set_work(&self, work: u64) {
        self.heartbeat.set_work(work);
    }

    /// Its generation.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// The next message from the host; `None` once the host has gone.
    pub async fn recv(&mut self) -> Option<HostMsg> {
        self.inbox.recv().await
    }
}

/// A task worker's body.
pub type TaskFn = Arc<dyn Fn(TaskWorker) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// Launches generations of one task worker on a runtime.
pub struct TaskLauncher {
    kind: WorkerKind,
    runtime: Handle,
    body: TaskFn,
    peer: Option<Arc<dyn Peer>>,
}

impl std::fmt::Debug for TaskLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskLauncher").field("kind", &self.kind).finish_non_exhaustive()
    }
}

impl TaskLauncher {
    /// Runs the future `body` makes on `runtime` for each generation of a worker of `kind`.
    pub fn new<F>(kind: WorkerKind, runtime: Handle, body: impl Fn(TaskWorker) -> F + Send + Sync + 'static) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let body: TaskFn = Arc::new(move |worker| Box::pin(body(worker)));
        Self { kind, runtime, body, peer: None }
    }

    /// With each generation's `Start` made by `peer` (sent to the body as its first message; the adapter's
    /// parameters arrive as a value, never over a channel).
    #[must_use]
    pub fn with_peer(mut self, peer: Arc<dyn Peer>) -> Self {
        self.peer = Some(peer);
        self
    }
}

/// A task's inbox, as a control channel.
struct InboxControl(Mutex<UnboundedSender<HostMsg>>);

impl ControlSend for InboxControl {
    fn send(&self, msg: &HostMsg, _handles: &[&ChannelHandle]) -> Result<(), LaunchError> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).send(msg.clone()).map_err(|_| LaunchError::Closed)
    }
}

impl WorkerLauncher for TaskLauncher {
    fn kind(&self) -> WorkerKind {
        self.kind
    }

    fn spawn(&self, _id: WorkerId, generation: u32, _waker: Waker) -> Result<Box<dyn WorkerHandle>, LaunchError> {
        let start = self.peer.as_ref().map(|peer| peer.prepare(generation)).transpose()?;
        let (tx, inbox) = mpsc::unbounded_channel();
        let heartbeat = HeartbeatCell::new();
        let worker = TaskWorker { heartbeat: heartbeat.clone(), inbox, generation };
        let task = self.runtime.spawn((self.body)(worker));
        Ok(Box::new(TaskHandle {
            heartbeat,
            task,
            ended: None,
            control: Arc::new(InboxControl(Mutex::new(tx))),
            start,
        }))
    }
}

/// A running task worker.
struct TaskHandle {
    heartbeat: HeartbeatCell,
    task: JoinHandle<()>,
    ended: Option<ExitInfo>,
    control: Arc<InboxControl>,
    start: Option<Start>,
}

impl WorkerHandle for TaskHandle {
    fn heartbeat(&self) -> &HeartbeatCell {
        &self.heartbeat
    }

    fn try_wait(&mut self) -> Option<ExitInfo> {
        if self.ended.is_none() && self.task.is_finished() {
            let code = match (&mut self.task).now_or_never() {
                Some(Err(e)) if e.is_panic() => PANICKED,
                _ => RETURNED,
            };
            self.ended = Some(ExitInfo { code: Some(code) });
        }
        self.ended
    }

    fn kill(&mut self) {
        self.task.abort();
    }

    fn needs_hello(&self) -> bool {
        false
    }

    fn start(&mut self) -> Result<(), LaunchError> {
        match self.start.take() {
            Some(start) => self.control.send(&HostMsg::start(start.channels, start.params), &[]),
            None => Ok(()),
        }
    }

    fn control(&self) -> Arc<dyn ControlSend> {
        Arc::clone(&self.control) as Arc<dyn ControlSend>
    }

    fn pid(&self) -> Option<u32> {
        None
    }

    fn revocable(&self) -> bool {
        true
    }

    fn fault(&self) -> Option<Fault> {
        None
    }
}

impl Drop for TaskHandle {
    fn drop(&mut self) {
        // Whatever ends the handle ends the task too, at its next `.await`.
        self.task.abort();
    }
}
