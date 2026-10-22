//! Workers as threads (tests, and the interim in-process encoder of record.md D11): a closure per worker. A panic is
//! a crash. A thread can't be killed: `kill` asks it to stop, and one that has not ended after `exit_wait` is
//! abandoned by the supervisor (counted and replaced). That is safe only because it holds nothing exclusive: it gets
//! its own generation's messages and objects, which no one reads any more once it is abandoned (quarantined).

use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use omuzaki_stream_api::supervisor::{WorkerId, WorkerKind};
use omuzaki_stream_api::worker::{ChannelHandle, HostMsg};

use super::{ControlSend, ExitInfo, Fault, LaunchError, Peer, Start, Waker, WorkerHandle, WorkerLauncher};
use crate::watchdog::policy::HeartbeatCell;

/// The exit code of a closure that returned.
const RETURNED: i32 = 0;
/// The exit code of a closure that panicked (Rust's own for a panicking main).
const PANICKED: i32 = 101;

/// What a thread worker sees.
#[derive(Debug)]
pub struct ThreadWorker {
    heartbeat: HeartbeatCell,
    stop: Arc<AtomicBool>,
    inbox: Receiver<HostMsg>,
    generation: u32,
}

impl ThreadWorker {
    /// One beat.
    pub fn beat(&self) {
        self.heartbeat.beat();
    }

    /// Sets the work counter.
    pub fn set_work(&self, work: u64) {
        self.heartbeat.set_work(work);
    }

    /// Whether the supervisor wants it to end (it was killed, abandoned, or the supervisor is stopping).
    pub fn should_stop(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Its generation.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// The next message from the host, waiting at most `timeout`; `None` on a timeout or once the host has gone.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<HostMsg> {
        self.inbox.recv_timeout(timeout).ok()
    }
}

/// A thread worker's body.
pub type WorkerFn = Arc<dyn Fn(&ThreadWorker) + Send + Sync>;

/// Launches generations of one thread worker.
pub struct ThreadLauncher {
    kind: WorkerKind,
    body: WorkerFn,
    peer: Option<Arc<dyn Peer>>,
}

impl std::fmt::Debug for ThreadLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ThreadLauncher").field("kind", &self.kind).finish_non_exhaustive()
    }
}

impl ThreadLauncher {
    /// Runs `body` on a new thread for each generation of a worker of `kind`.
    pub fn new(kind: WorkerKind, body: impl Fn(&ThreadWorker) + Send + Sync + 'static) -> Self {
        Self { kind, body: Arc::new(body), peer: None }
    }

    /// With each generation's `Start` made by `peer` (sent to the body as its first message).
    #[must_use]
    pub fn with_peer(mut self, peer: Arc<dyn Peer>) -> Self {
        self.peer = Some(peer);
        self
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

/// A thread's inbox, as a control channel.
struct InboxControl(Mutex<Sender<HostMsg>>);

impl ControlSend for InboxControl {
    fn send(&self, msg: &HostMsg, _handles: &[&ChannelHandle]) -> Result<(), LaunchError> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).send(msg.clone()).map_err(|_| LaunchError::Closed)
    }
}

impl WorkerLauncher for ThreadLauncher {
    fn kind(&self) -> WorkerKind {
        self.kind
    }

    fn spawn(&self, id: WorkerId, generation: u32, _waker: Waker) -> Result<Box<dyn WorkerHandle>, LaunchError> {
        let start = self.peer.as_ref().map(|peer| peer.prepare(generation)).transpose()?;
        let (tx, inbox) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let heartbeat = HeartbeatCell::new();
        let worker = ThreadWorker { heartbeat: heartbeat.clone(), stop: Arc::clone(&stop), inbox, generation };
        let (done, code) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicI32::new(RETURNED)));
        let ended = Ended { done: Arc::clone(&done), code: Arc::clone(&code) };
        let body = Arc::clone(&self.body);
        std::thread::Builder::new()
            .name(format!("worker-{}-g{generation}", id.index()))
            .spawn(move || {
                let _ended = ended;
                body(&worker);
            })
            .map_err(|e| LaunchError::spawn(e.to_string()))?;
        Ok(Box::new(ThreadHandle {
            heartbeat,
            stop,
            done,
            code,
            control: Arc::new(InboxControl(Mutex::new(tx))),
            start,
        }))
    }
}

/// A running thread worker.
struct ThreadHandle {
    heartbeat: HeartbeatCell,
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    code: Arc<AtomicI32>,
    control: Arc<InboxControl>,
    start: Option<Start>,
}

impl WorkerHandle for ThreadHandle {
    fn heartbeat(&self) -> &HeartbeatCell {
        &self.heartbeat
    }

    fn try_wait(&mut self) -> Option<ExitInfo> {
        self.done.load(Ordering::Acquire).then(|| ExitInfo { code: Some(self.code.load(Ordering::Acquire)) })
    }

    fn kill(&mut self) {
        self.stop.store(true, Ordering::Release);
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

impl Drop for ThreadHandle {
    fn drop(&mut self) {
        // Whatever ends the handle asks the thread to end too.
        self.stop.store(true, Ordering::Release);
    }
}
