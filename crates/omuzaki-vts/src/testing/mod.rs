//! An in-process mock `VTube Studio` server for tests, behind the `testing` feature (never in the default build).
//!
//! [`MockVts`] binds `127.0.0.1` on an ephemeral port and speaks the subset the bridge uses (authentication, model,
//! parameters, hotkeys, expressions), enforcing the documented limits so a test catches misuse: 100 custom
//! parameters per plugin, a 1 s keep-alive, one trigger per hotkey every 5 frames, and a queue of 32 hotkeys.
//!
//! Time comes from an injected [`Clock`]. Drive it with a [`FakeClock`] and wait for each reply before advancing it:
//! the server stamps a request with the clock when it handles it, so the reply is the proof it has been stamped.

mod clock;
mod state;
#[cfg(test)]
mod tests;

use std::io::{self, ErrorKind};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{Builder, JoinHandle};

pub use clock::{Clock, FakeClock};
use state::State;
pub use state::{Event, MockHotkey, MockModel, Received};
use tungstenite::Message;

/// How the mock starts. The scripted behaviour can also be changed while it runs, with the methods on [`MockVts`].
#[derive(Clone)]
pub struct MockConfig {
    /// Whether the user approves the token popup (`AuthenticationTokenRequest`).
    pub approve_token: bool,
    /// The model VTS has loaded, if any.
    pub model: Option<MockModel>,
    /// The reported framerate; zero counts as one frame per second.
    pub framerate: u32,
    /// The clock every keep-alive, cooldown and arrival time reads: elapsed time, as for `uptime`.
    pub clock: Arc<dyn Clock>,
    /// Milliseconds since the Unix epoch at the clock's zero: a reply's `timestamp` is this plus the elapsed time.
    pub start_epoch_ms: i64,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            approve_token: true,
            model: None,
            framerate: 60,
            clock: Arc::new(FakeClock::default()),
            start_epoch_ms: 1_700_000_000_000,
        }
    }
}

struct Shared {
    state: Mutex<State>,
    clock: Arc<dyn Clock>,
    /// Connections the server could not set up or serve, for [`MockVts::setup_failures`].
    failures: Mutex<Vec<String>>,
}

impl Shared {
    fn fail(&self, what: String) {
        self.failures.lock().unwrap_or_else(PoisonError::into_inner).push(what);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock leaves plain data behind, so a poisoned lock is still usable.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

struct Conn {
    stream: TcpStream,
    thread: JoinHandle<()>,
}

struct Running {
    addr: SocketAddr,
    stopping: Arc<AtomicBool>,
    acceptor: JoinHandle<()>,
    conns: Arc<Mutex<Vec<Conn>>>,
}

/// A running (or stopped) mock server. Dropping it stops it.
pub struct MockVts {
    shared: Arc<Shared>,
    port: u16,
    running: Option<Running>,
}

impl MockVts {
    /// Binds `127.0.0.1:0` and starts serving.
    ///
    /// # Errors
    /// The I/O error when the socket cannot be bound or a thread cannot be started.
    pub fn start(config: &MockConfig) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            clock: Arc::clone(&config.clock),
            state: Mutex::new(State::new(config)),
            failures: Mutex::new(Vec::new()),
        });
        let mut mock = Self { shared, port: 0, running: None };
        mock.running = Some(mock.bind(0)?);
        mock.port = mock.running.as_ref().map_or(0, |r| r.addr.port());
        Ok(mock)
    }

    fn bind(&self, port: u16) -> io::Result<Running> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))?;
        let addr = listener.local_addr()?;
        let stopping = Arc::new(AtomicBool::new(false));
        let conns = Arc::new(Mutex::new(Vec::new()));
        let (shared, flag, list) = (Arc::clone(&self.shared), Arc::clone(&stopping), Arc::clone(&conns));
        let acceptor = Builder::new()
            .name("vts-mock-accept".into())
            .spawn(move || accept_loop(&listener, &shared, &flag, &list))?;
        Ok(Running { addr, stopping, acceptor, conns })
    }

    /// The port the server listens on; the same after a [`restart`](Self::restart).
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The WebSocket URL a client connects to.
    pub fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }

    /// Stops listening and closes every connection. The scripted state and the records are kept.
    ///
    /// # Errors
    /// The I/O error when a connection cannot be shut down for a reason other than being closed already, or
    /// [`ErrorKind::Other`] when the acceptor or a connection thread panicked.
    pub fn stop(&mut self) -> io::Result<()> {
        let Some(run) = self.running.take() else { return Ok(()) };
        run.stopping.store(true, Ordering::SeqCst);
        // Wakes the blocked `accept`. A refused connect means the acceptor already ended, which is what we want.
        let mut panicked = false;
        if TcpStream::connect(run.addr).is_ok() {
            panicked |= join(run.acceptor);
        }
        let conns = std::mem::take(&mut *run.conns.lock().unwrap_or_else(PoisonError::into_inner));
        let mut result = Ok(());
        for conn in conns {
            match conn.stream.shutdown(Shutdown::Both) {
                Err(e) if e.kind() != ErrorKind::NotConnected => result = Err(e),
                _ => {}
            }
            panicked |= join(conn.thread);
        }
        self.shared.lock().end_sessions();
        if panicked {
            return Err(io::Error::other("a mock server thread panicked"));
        }
        result
    }

    /// Listens again on the same port, with every client unauthenticated (a new connection starts a new session).
    ///
    /// # Errors
    /// [`ErrorKind::AlreadyExists`] when the server is running, or the I/O error from binding the port again.
    pub fn restart(&mut self) -> io::Result<()> {
        if self.running.is_some() {
            return Err(io::Error::new(ErrorKind::AlreadyExists, "the mock server is already running"));
        }
        self.running = Some(self.bind(self.port)?);
        Ok(())
    }

    /// Every request received so far, in arrival order, stamped with the injected clock.
    pub fn received(&self) -> Vec<Received> {
        self.shared.lock().received.clone()
    }

    /// The events recorded so far (keep-alive lapses up to the clock's current time).
    pub fn events(&self) -> Vec<Event> {
        let mut state = self.shared.lock();
        state.settle(self.shared.clock.now());
        state.events.clone()
    }

    /// What went wrong setting up or serving connections (a failed accept, socket clone, thread start or handshake).
    /// Empty in a healthy run, so a test can assert it.
    pub fn setup_failures(&self) -> Vec<String> {
        self.shared.failures.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The value a parameter has now, composed from every plugin's live feeds (`set` blended by weight, plus `add`s).
    pub fn param_value(&self, name: &str) -> Option<f64> {
        self.shared.lock().value_of(name)
    }

    /// The user revokes API access: every token is void and every client's next request fails as not authenticated.
    pub fn revoke(&self) {
        self.shared.lock().revoke();
    }

    /// Another plugin takes `name` in `set` mode: `set` injection into it errors and `add` still works.
    pub fn claim_param(&self, name: &str) {
        self.shared.lock().claimed.insert(name.to_owned());
    }

    /// Answers the next token popups with `approve`.
    pub fn set_approve_token(&self, approve: bool) {
        self.shared.lock().approve_token = approve;
    }

    /// Loads `model` (or unloads with `None`).
    pub fn set_model(&self, model: Option<MockModel>) {
        self.shared.lock().model = model;
    }

    /// Reports `framerate` from now on (zero counts as one); the hotkey cooldown is five frames at this rate.
    pub fn set_framerate(&self, framerate: u32) {
        self.shared.lock().framerate = framerate.max(1);
    }
}

impl Drop for MockVts {
    fn drop(&mut self) {
        // Nowhere to report a shutdown error from a destructor; tests that care call `stop` themselves.
        drop(self.stop());
    }
}

/// Joins `thread`; true when it panicked.
fn join(thread: JoinHandle<()>) -> bool {
    thread.join().is_err()
}

fn accept_loop(listener: &TcpListener, shared: &Arc<Shared>, stopping: &AtomicBool, conns: &Mutex<Vec<Conn>>) {
    for incoming in listener.incoming() {
        let stream = match incoming {
            Ok(stream) => stream,
            Err(e) => {
                shared.fail(format!("accept failed: {e}"));
                break;
            }
        };
        if stopping.load(Ordering::SeqCst) {
            break;
        }
        let handle = match stream.try_clone() {
            Ok(handle) => handle,
            Err(e) => {
                shared.fail(format!("socket clone failed: {e}"));
                continue;
            }
        };
        let worker = Arc::clone(shared);
        match Builder::new().name("vts-mock-conn".into()).spawn(move || serve(&worker, stream)) {
            Ok(thread) => conns.lock().unwrap_or_else(PoisonError::into_inner).push(Conn { stream: handle, thread }),
            Err(e) => shared.fail(format!("connection thread failed to start: {e}")),
        }
    }
}

fn serve(shared: &Shared, stream: TcpStream) {
    let session = shared.lock().open_session();
    match tungstenite::accept(stream) {
        Ok(mut ws) => {
            while let Ok(message) = ws.read() {
                match message {
                    Message::Text(text) => {
                        let now = shared.clock.now();
                        let reply = shared.lock().handle(session, &text, now);
                        // A reply that cannot be built or sent ends the connection: the client sees a closed socket.
                        let sent = reply.is_ok_and(|r| ws.send(Message::text(r)).is_ok());
                        if !sent {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
        Err(e) => shared.fail(format!("WebSocket handshake failed: {e}")),
    }
    shared.lock().close_session(session);
}
