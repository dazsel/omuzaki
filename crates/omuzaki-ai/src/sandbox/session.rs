//! The registry's side of a worker: starting it, its watchdog, cold requests, and [`SandboxedSession`].

use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use omuzaki_ai_api::{
    FileSource, LoadError, ModelRef, NoProgress, PinnedFile, Probe, Provider, RunError, Session, SessionInfo,
    Signature, TensorBuf, TensorRef,
};

use super::limits::{self, Contained, MEMORY_ENV};
use super::wire::{self, Control, Kind, MAGIC, broken, read_frame, write_control, write_frame};
use super::{LOAD_LIMIT, LoadSpec, Model, START_LIMIT, SandboxSettings};

/// How much a worker may print before [`MAGIC`] (a test binary's harness banner) before it counts as broken.
const MAX_PREAMBLE: usize = 64 * 1024;

/// What the watchdog thread and the worker's owner share.
struct Watch {
    child: Mutex<Child>,
    base: Instant,
    /// When to kill the worker, in nanoseconds since `base`, plus one; 0 while nothing is timed.
    deadline: AtomicU64,
    /// The watchdog killed the worker.
    fired: AtomicBool,
    stop: AtomicBool,
}

impl Watch {
    fn kill(&self) {
        let mut child = self.child.lock().unwrap_or_else(PoisonError::into_inner);
        // Already gone is fine: the point is that it doesn't run.
        let _ = child.kill();
    }

    /// The watchdog: sleeps until the deadline, and kills the worker if it is still armed by then.
    fn run(&self) {
        while !self.stop.load(Ordering::Acquire) {
            let deadline = self.deadline.load(Ordering::Acquire);
            if deadline == 0 {
                thread::park();
                continue;
            }
            let now = nanos(self.base.elapsed());
            if now + 1 >= deadline {
                if self.deadline.compare_exchange(deadline, 0, Ordering::AcqRel, Ordering::Acquire).is_ok() {
                    self.fired.store(true, Ordering::Release);
                    self.kill();
                }
                continue;
            }
            thread::park_timeout(Duration::from_nanos(deadline - 1 - now));
        }
    }
}

fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX - 1)
}

/// One running worker process and its pipe.
struct Worker {
    watch: Arc<Watch>,
    watchdog: Option<JoinHandle<()>>,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    buf: Vec<u8>,
    _contained: Contained,
}

impl Worker {
    /// Starts a worker and waits for it to say it is ready.
    fn start(settings: &SandboxSettings) -> Result<Self, LoadError> {
        if cfg!(target_family = "wasm") {
            return Err(LoadError::Refused(
                "models you add run in a separate worker process, which the web player can't start".into(),
            ));
        }
        let name = settings.program.file_name().map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let mut command = Command::new(&settings.program);
        command
            .args(&settings.args)
            .envs(settings.env.iter().map(|(k, v)| (k, v)))
            .env(MEMORY_ENV, settings.memory_mib.to_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        let mut child = command
            .spawn()
            .map_err(|e| LoadError::Runtime(format!("the model worker ({name}) couldn't be started: {e}")))?;
        let contained = limits::contain(&child, settings.memory_mib);
        let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err(LoadError::Runtime("the model worker started without its pipes".into()));
        };
        let watch = Arc::new(Watch {
            child: Mutex::new(child),
            base: Instant::now(),
            deadline: AtomicU64::new(0),
            fired: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let watched = Arc::clone(&watch);
        let watchdog = thread::Builder::new().name("model worker watchdog".into()).spawn(move || watched.run());
        let watchdog = match watchdog {
            Ok(handle) => handle,
            Err(e) => {
                watch.kill();
                return Err(LoadError::Runtime(format!("couldn't start the model worker's watchdog: {e}")));
            }
        };
        let mut worker = Self {
            watch,
            watchdog: Some(watchdog),
            stdin: BufWriter::new(stdin),
            stdout: BufReader::new(stdout),
            buf: Vec::new(),
            _contained: contained,
        };
        worker.arm(START_LIMIT);
        let greeted = worker.await_magic();
        worker.disarm();
        greeted.map_err(|e| worker.failure("starting", START_LIMIT, &e))?;
        Ok(worker)
    }

    /// Skips whatever comes before [`MAGIC`].
    fn await_magic(&mut self) -> io::Result<()> {
        let mut seen = 0usize;
        let mut matched = 0usize;
        while matched < MAGIC.len() {
            let byte = match self.stdout.fill_buf()? {
                [] => return Err(io::ErrorKind::UnexpectedEof.into()),
                [b, ..] => *b,
            };
            self.stdout.consume(1);
            seen += 1;
            matched = if byte == MAGIC[matched] { matched + 1 } else { usize::from(byte == MAGIC[0]) };
            if seen > MAX_PREAMBLE {
                return Err(broken("the worker never said it was ready"));
            }
        }
        Ok(())
    }

    /// Times the next exchange: past `limit` the watchdog kills the worker.
    fn arm(&self, limit: Duration) {
        let deadline = nanos(self.watch.base.elapsed() + limit).saturating_add(1);
        self.watch.deadline.store(deadline, Ordering::Release);
        if let Some(watchdog) = &self.watchdog {
            watchdog.thread().unpark();
        }
    }

    fn disarm(&self) {
        self.watch.deadline.store(0, Ordering::Release);
    }

    fn fired(&self) -> bool {
        self.watch.fired.load(Ordering::Acquire)
    }

    /// The error for an exchange that failed while `doing`.
    fn failure(&self, doing: &str, limit: Duration, e: &io::Error) -> LoadError {
        self.watch.kill();
        if self.fired() {
            LoadError::Runtime(format!("the model worker took longer than {} s {doing} the model", limit.as_secs()))
        } else if e.kind() == io::ErrorKind::InvalidData {
            LoadError::Runtime(format!("the model worker broke the protocol {doing} the model: {e}"))
        } else {
            LoadError::Runtime(format!("the model worker crashed {doing} the model"))
        }
    }

    /// Sends a cold request and returns the answer, fetching the native libraries the worker asks for on the way.
    fn request(&mut self, message: &Control, natives: &dyn FileSource, doing: &str) -> Result<Control, LoadError> {
        self.arm(LOAD_LIMIT);
        let answer = self.exchange(message, natives);
        self.disarm();
        answer.map_err(|e| self.failure(doing, LOAD_LIMIT, &e))
    }

    fn exchange(&mut self, message: &Control, natives: &dyn FileSource) -> io::Result<Control> {
        write_control(&mut self.stdin, message)?;
        self.stdin.flush()?;
        loop {
            match read_frame(&mut self.stdout, &mut self.buf)? {
                Some(Kind::Control) => {}
                Some(other) => return Err(broken(format!("a {other:?} frame where an answer was expected"))),
                None => return Err(io::ErrorKind::UnexpectedEof.into()),
            }
            let Control::Fetch { urls, sha256, size } = wire::control(&self.buf)? else {
                return wire::control(&self.buf);
            };
            // A download is the registry's time, not the worker's.
            self.disarm();
            let result = natives.get(&PinnedFile::new(urls, sha256, size), &NoProgress).map_err(|e| e.to_string());
            self.arm(LOAD_LIMIT);
            write_control(&mut self.stdin, &Control::Fetched { result })?;
            self.stdin.flush()?;
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.watch.stop.store(true, Ordering::Release);
        self.watch.kill();
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.thread().unpark();
            let _ = watchdog.join();
        }
        // Reaps it, so no zombie is left; it was killed above, so this doesn't wait long.
        let _ = self.watch.child.lock().unwrap_or_else(PoisonError::into_inner).wait();
    }
}

/// Probes `model` in a worker of its own.
pub(super) fn probe(
    settings: &SandboxSettings,
    model: &Model<'_>,
    natives: &dyn FileSource,
) -> Result<Probe, LoadError> {
    let mut worker = Worker::start(settings)?;
    match worker.request(&Control::Probe { job: model.job() }, natives, "probing")? {
        Control::Probed { signature, opset } => Ok(Probe { signature, opset }),
        Control::Failed { error } => Err(error.into()),
        other => Err(LoadError::Runtime(format!("the model worker answered a probe with {other:?}"))),
    }
}

/// A model running in its own worker process, as a plain [`Session`].
pub struct SandboxedSession {
    worker: Worker,
    signature: Signature,
    info: SessionInfo,
    stall: Duration,
    /// The worker is gone: every run is `Crashed` from now on.
    dead: bool,
    /// Where each input tensor is encoded; it only grows.
    scratch: Vec<u8>,
}

impl SandboxedSession {
    /// Loads `model` in a worker of its own.
    pub(super) fn load(
        settings: &SandboxSettings,
        model: &Model<'_>,
        spec: LoadSpec<'_>,
        natives: &dyn FileSource,
    ) -> Result<Self, LoadError> {
        let mut worker = Worker::start(settings)?;
        let request =
            Control::Load { job: model.job(), slot: spec.slot.clone(), io: spec.io.clone(), options: spec.options };
        let provider = match worker.request(&request, natives, "loading")? {
            Control::Loaded { provider } => provider,
            Control::Failed { error } => return Err(error.into()),
            other => return Err(LoadError::Runtime(format!("the model worker answered a load with {other:?}"))),
        };
        Ok(Self {
            worker,
            signature: spec.slot.clone(),
            info: info(model.model, provider),
            stall: spec.stall,
            dead: false,
            scratch: Vec::new(),
        })
    }

    /// One run over the pipe. An I/O error means the worker is gone or stalled.
    fn exchange(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> io::Result<Result<(), RunError>> {
        let w = &mut self.worker;
        write_frame(&mut w.stdin, Kind::Run, &wire::count(inputs.len()))?;
        for input in inputs {
            wire::encode_tensor(input, &mut self.scratch)?;
            write_frame(&mut w.stdin, Kind::Tensor, &self.scratch)?;
        }
        w.stdin.flush()?;
        match read_frame(&mut w.stdout, &mut w.buf)? {
            Some(Kind::Ran) => {
                if wire::read_count(&w.buf)? != outputs.len() {
                    return Err(broken("a run answered with the wrong number of outputs"));
                }
                for out in outputs.iter_mut() {
                    if read_frame(&mut w.stdout, &mut w.buf)? != Some(Kind::Tensor) {
                        return Err(broken("a run answered without its outputs"));
                    }
                    wire::decode_tensor(&w.buf, out)?;
                }
                Ok(Ok(()))
            }
            Some(Kind::Control) => match wire::control(&w.buf)? {
                Control::RunFailed { error } => Ok(Err(error.into())),
                other => Err(broken(format!("a run answered with {other:?}"))),
            },
            Some(other) => Err(broken(format!("a run answered with a {other:?} frame"))),
            None => Err(io::ErrorKind::UnexpectedEof.into()),
        }
    }

    /// True while the worker process runs. A worker that crashed, stalled or was killed is gone.
    pub fn worker_running(&self) -> bool {
        let mut child = self.worker.watch.child.lock().unwrap_or_else(PoisonError::into_inner);
        matches!(child.try_wait(), Ok(None))
    }

    fn die(&mut self) {
        self.dead = true;
        self.worker.watch.kill();
    }
}

fn info(model: &ModelRef, provider: Provider) -> SessionInfo {
    SessionInfo { model: model.clone(), provider, sandboxed: true, fallback_for: None }
}

impl Session for SandboxedSession {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.signature.check_inputs(inputs)?;
        self.signature.check_outputs(outputs)?;
        if self.dead {
            return Err(RunError::Crashed);
        }
        self.worker.arm(self.stall);
        let result = self.exchange(inputs, outputs);
        self.worker.disarm();
        match result {
            Ok(Err(RunError::Crashed)) | Err(_) => {
                self.die();
                Err(RunError::Crashed)
            }
            Ok(other) if self.worker.fired() => {
                // The answer came, but the deadline had passed and the worker is already gone.
                drop(other);
                self.die();
                Err(RunError::Crashed)
            }
            Ok(other) => other,
        }
    }

    fn reset(&mut self) {
        if self.dead {
            return;
        }
        let sent = write_frame(&mut self.worker.stdin, Kind::Reset, &[]).and_then(|()| self.worker.stdin.flush());
        if sent.is_err() {
            self.die();
        }
    }

    fn info(&self) -> SessionInfo {
        self.info.clone()
    }
}
