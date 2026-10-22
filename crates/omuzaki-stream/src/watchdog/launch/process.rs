//! Workers as processes (`docs/design/supervisor.md` §4.2): the control channel is a socketpair handed to the child
//! as its stdin; stdout and stderr are plain log pipes the host reads with fixed limits, so nothing a library prints
//! can touch the protocol.
//!
//! The child is started with `--generation <n> --host-pid <pid>` after its own arguments. A reader thread checks its
//! `Hello` (kind, protocol version, generation), stores its beats, hands every other message to the worker's [`Peer`],
//! and wakes the supervisor on `Hello`, on a fault and when the channel ends: the channel's end is the crash signal.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use omuzaki_stream_api::supervisor::{WorkerId, WorkerKind};

use super::{LaunchError, Peer, Waker, WorkerHandle, WorkerLauncher};

/// A program, its arguments and its environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerCommand {
    /// The program.
    pub program: PathBuf,
    /// Its arguments, before the launcher's `--generation` and `--host-pid`.
    pub args: Vec<OsString>,
    /// Variables set for it, on every OS.
    pub env: Vec<(OsString, OsString)>,
}

impl WorkerCommand {
    /// `program` with `args` and no extra environment.
    pub fn new(program: impl Into<PathBuf>, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        Self { program: program.into(), args: args.into_iter().map(Into::into).collect(), env: Vec::new() }
    }

    /// With `key` set to `value` in its environment.
    #[must_use]
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }
}

/// Launches generations of one process worker.
pub struct ProcessLauncher {
    command: WorkerCommand,
    kind: WorkerKind,
    peer: Arc<dyn Peer>,
}

impl std::fmt::Debug for ProcessLauncher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProcessLauncher")
            .field("command", &self.command)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl ProcessLauncher {
    /// Runs `command` as a worker of `kind`, its generations made and heard by `peer`.
    pub fn new(command: WorkerCommand, kind: WorkerKind, peer: Arc<dyn Peer>) -> Self {
        Self { command, kind, peer }
    }
}

impl WorkerLauncher for ProcessLauncher {
    fn kind(&self) -> WorkerKind {
        self.kind
    }

    fn spawn(&self, id: WorkerId, generation: u32, waker: Waker) -> Result<Box<dyn WorkerHandle>, LaunchError> {
        let start = self.peer.prepare(generation)?;
        if start.params.kind() != self.kind {
            return Err(LaunchError::permanent(format!(
                "a {:?} worker was given {:?} parameters",
                self.kind,
                start.params.kind()
            )));
        }
        #[cfg(unix)]
        {
            unix::spawn(&self.command, self.kind, Arc::clone(&self.peer), id, generation, start, waker)
        }
        #[cfg(not(unix))]
        {
            let _ = (id, waker, start);
            Err(LaunchError::permanent(
                "process workers need the Windows control pipe, which is not built yet (docs/design/supervisor.md §13)",
            ))
        }
    }
}

/// The host reads a worker's stdout and stderr with fixed limits (§4.2): lines cut at 4 KiB, at most 100 lines a
/// second logged, one 8 KiB buffer per reader.
// Used by the Unix launcher only until the Windows control pipe is built.
#[cfg_attr(not(unix), allow(dead_code))]
mod logs {
    use std::io::Read;
    use std::time::{Duration, Instant};

    /// The longest line logged; the rest is discarded up to its newline and counted.
    pub(super) const MAX_LINE: usize = 4 * 1024;
    /// Lines logged per second per stream; the rest are only counted.
    pub(super) const LINES_PER_SECOND: u32 = 100;
    const BUFFER: usize = 8 * 1024;

    /// Reads `stream` to its end, logging at debug level within the limits.
    pub(super) fn drain(mut stream: impl Read, worker: &str, which: &'static str) {
        let mut buf = [0_u8; BUFFER];
        let mut line = Vec::with_capacity(MAX_LINE);
        let (mut cut, mut skipped) = (false, 0_u64);
        let (mut window, mut logged) = (Instant::now(), 0_u32);
        let mut emit = |line: &[u8], skipped: &mut u64| {
            if window.elapsed() >= Duration::from_secs(1) {
                (window, logged) = (Instant::now(), 0);
            }
            if logged < LINES_PER_SECOND {
                logged += 1;
                tracing::debug!(worker, stream = which, "{}", String::from_utf8_lossy(line));
            } else {
                *skipped += 1;
            }
        };
        loop {
            let n = match stream.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            for &byte in &buf[..n] {
                if byte == b'\n' {
                    emit(&line, &mut skipped);
                    line.clear();
                    cut = false;
                } else if line.len() < MAX_LINE {
                    line.push(byte);
                } else if !cut {
                    cut = true;
                    skipped += 1;
                }
            }
        }
        if !line.is_empty() {
            emit(&line, &mut skipped);
        }
        if skipped > 0 {
            tracing::debug!(worker, stream = which, skipped, "lines cut or over the rate limit");
        }
    }
}

#[cfg(unix)]
mod unix {
    use std::os::fd::OwnedFd;
    use std::os::unix::net::UnixStream;
    use std::process::{Child, Command, Stdio};
    use std::sync::{Arc, Mutex, PoisonError};

    use omuzaki_stream_api::error::StreamError;
    use omuzaki_stream_api::supervisor::{WorkerId, WorkerKind};
    use omuzaki_stream_api::worker::{ChannelHandle, HostMsg, PROTOCOL_VERSION, WorkerMsg, read_msg, write_msg_fds};

    use super::super::{ControlSend, ExitInfo, Fault, LaunchError, Peer, Start, Wake, WakeKind, Waker, WorkerHandle};
    use super::{WorkerCommand, logs};
    use crate::watchdog::policy::HeartbeatCell;

    /// The write side of a socketpair.
    struct SocketControl(Mutex<UnixStream>);

    impl ControlSend for SocketControl {
        fn send(&self, msg: &HostMsg, handles: &[&ChannelHandle]) -> Result<(), LaunchError> {
            let fds: Vec<_> = handles.iter().map(|h| h.fd()).collect();
            if !fds.is_empty() && !matches!(msg, HostMsg::Start { .. } | HostMsg::Attach { .. }) {
                return Err(LaunchError::Protocol { what: "descriptors on a message that carries none".into() });
            }
            let sock = self.0.lock().unwrap_or_else(PoisonError::into_inner);
            write_msg_fds(&sock, msg, &fds).map_err(|e| match e {
                StreamError::Io(_) => LaunchError::Closed,
                other => LaunchError::Protocol { what: other.to_string() },
            })
        }
    }

    pub(super) fn spawn(
        command: &WorkerCommand,
        kind: WorkerKind,
        peer: Arc<dyn Peer>,
        id: WorkerId,
        generation: u32,
        start: Start,
        waker: Waker,
    ) -> Result<Box<dyn WorkerHandle>, LaunchError> {
        let io = |what: &'static str| move |e: std::io::Error| LaunchError::spawn(format!("{what}: {e}"));
        let (host, child_end) = UnixStream::pair().map_err(io("socketpair"))?;
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .arg("--generation")
            .arg(generation.to_string())
            .arg("--host-pid")
            .arg(std::process::id().to_string())
            .envs(command.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::from(OwnedFd::from(child_end)))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(io("spawn"))?;
        let name = format!("{kind:?}-{}-g{generation}", id.index());
        if let Some(out) = child.stdout.take() {
            let name = name.clone();
            let _ = std::thread::Builder::new()
                .name(format!("log-{name}"))
                .spawn(move || logs::drain(out, &name, "stdout"));
        }
        if let Some(err) = child.stderr.take() {
            let name = name.clone();
            let _ = std::thread::Builder::new()
                .name(format!("log-{name}"))
                .spawn(move || logs::drain(err, &name, "stderr"));
        }
        let cell = HeartbeatCell::new();
        let fault = Arc::new(Mutex::new(None));
        let reader = host.try_clone().map_err(io("socket clone"))?;
        let ctx = Reader { id, generation, kind, cell: cell.clone(), fault: Arc::clone(&fault), peer, waker };
        if let Err(e) = std::thread::Builder::new().name(format!("ctl-{name}")).spawn(move || ctx.run(reader)) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(io("reader thread")(e));
        }
        Ok(Box::new(ProcessHandle {
            child,
            control: Arc::new(SocketControl(Mutex::new(host))),
            cell,
            fault,
            start: Some(start),
            exited: None,
        }))
    }

    /// The reader thread's context.
    struct Reader {
        id: WorkerId,
        generation: u32,
        kind: WorkerKind,
        cell: HeartbeatCell,
        fault: Arc<Mutex<Option<Fault>>>,
        peer: Arc<dyn Peer>,
        waker: Waker,
    }

    impl Reader {
        fn wake(&self, kind: WakeKind) {
            self.waker.wake(Wake { id: self.id, generation: self.generation, kind });
        }

        fn set_fault(&self, fault: Fault) {
            let mut slot = self.fault.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.is_none() {
                *slot = Some(fault);
            }
        }

        /// Checks `Hello`: `None` when it is good.
        fn hello_fault(&self, msg: &WorkerMsg) -> Option<Fault> {
            let permanent = |what: String| Some(Fault::Launch { what, permanent: true });
            match msg {
                WorkerMsg::Hello { kind, version, generation, .. } => {
                    if *version != PROTOCOL_VERSION {
                        permanent(format!("protocol version {version}, the host speaks {PROTOCOL_VERSION}"))
                    } else if *generation != self.generation {
                        permanent(format!("generation {generation}, expected {}", self.generation))
                    } else if *kind != self.kind {
                        permanent(format!("a {kind:?} worker where a {:?} was started", self.kind))
                    } else {
                        None
                    }
                }
                WorkerMsg::Gone { reason, .. } => Some(Fault::Launch { what: reason.clone(), permanent: false }),
                other => Some(Fault::Protocol(format!("{other:?} before Hello"))),
            }
        }

        fn run(self, mut sock: UnixStream) {
            let mut buf = Vec::new();
            let mut hello = false;
            loop {
                match read_msg::<_, WorkerMsg>(&mut sock, &mut buf) {
                    Ok(Some(msg)) if !hello => {
                        if let Some(fault) = self.hello_fault(&msg) {
                            self.set_fault(fault);
                            self.wake(WakeKind::Fault);
                            break;
                        }
                        hello = true;
                        self.wake(WakeKind::Hello);
                    }
                    Ok(Some(WorkerMsg::Beat { progress, work, .. })) => {
                        self.cell.set_work(work);
                        self.cell.set(progress);
                    }
                    Ok(Some(WorkerMsg::Gone { reason, .. })) => {
                        self.set_fault(Fault::Launch { what: reason.clone(), permanent: false });
                        self.peer.message(self.generation, WorkerMsg::gone(reason));
                    }
                    Ok(Some(msg)) => self.peer.message(self.generation, msg),
                    Ok(None) | Err(StreamError::Io(_)) => {
                        if !hello {
                            let what = "the control channel ended before Hello".to_owned();
                            self.set_fault(Fault::Launch { what, permanent: false });
                        }
                        break;
                    }
                    Err(e) => {
                        self.set_fault(Fault::Protocol(e.to_string()));
                        self.wake(WakeKind::Fault);
                        break;
                    }
                }
            }
            self.wake(WakeKind::Ended);
        }
    }

    /// A running child.
    struct ProcessHandle {
        child: Child,
        control: Arc<SocketControl>,
        cell: HeartbeatCell,
        fault: Arc<Mutex<Option<Fault>>>,
        start: Option<Start>,
        exited: Option<ExitInfo>,
    }

    impl WorkerHandle for ProcessHandle {
        fn heartbeat(&self) -> &HeartbeatCell {
            &self.cell
        }

        fn try_wait(&mut self) -> Option<ExitInfo> {
            if self.exited.is_none()
                && let Ok(Some(status)) = self.child.try_wait()
            {
                self.exited = Some(ExitInfo { code: status.code() });
            }
            self.exited
        }

        fn kill(&mut self) {
            if self.exited.is_none() {
                // An error means it has already gone, which is what was asked.
                let _ = self.child.kill();
            }
        }

        fn needs_hello(&self) -> bool {
            true
        }

        fn start(&mut self) -> Result<(), LaunchError> {
            let Some(start) = self.start.take() else { return Ok(()) };
            let handles: Vec<&ChannelHandle> = start.handles.iter().collect();
            self.control.send(&HostMsg::start(start.channels.clone(), start.params.clone()), &handles)
        }

        fn control(&self) -> Arc<dyn ControlSend> {
            Arc::clone(&self.control) as Arc<dyn ControlSend>
        }

        fn pid(&self) -> Option<u32> {
            Some(self.child.id())
        }

        fn revocable(&self) -> bool {
            false
        }

        fn fault(&self) -> Option<Fault> {
            self.fault.lock().unwrap_or_else(PoisonError::into_inner).clone()
        }
    }

    impl Drop for ProcessHandle {
        fn drop(&mut self) {
            // Never leave a zombie: kill a live child and reap it on a thread of its own, so a child stuck in the kernel
            // never blocks the supervisor; the reaper ends when the child does.
            if self.exited.is_none() && !matches!(self.child.try_wait(), Ok(Some(_))) {
                let _ = self.child.kill();
                let pid = self.child.id();
                if !matches!(self.child.try_wait(), Ok(Some(_))) {
                    // A `Child` can't be moved out of `&mut self`, so the reaper waits on its pid.
                    let _ = std::thread::Builder::new().name(format!("reap-{pid}")).spawn(move || reap(pid));
                }
            }
        }
    }

    /// Waits for child `pid` to end and reaps it.
    fn reap(pid: u32) {
        let Some(pid) = i32::try_from(pid).ok().and_then(rustix::process::Pid::from_raw) else { return };
        let _ = rustix::process::waitpid(Some(pid), rustix::process::WaitOptions::empty());
    }
}
