//! [`ObsClient`]: one connection to OBS Studio's remote-control server (obs-websocket v5), on a thread of its own.
//!
//! [`ObsClient::open`] opens the WebSocket, answers the `Hello` (with the password's challenge when the server asks)
//! and waits for `Identified`. A connection thread then owns the socket: it sends queued requests, matches each
//! response to its request by id (in whatever order the answers come), and publishes events to an [`EventHub`].
//! [`ObsClient::request`] blocks the caller until its answer arrives or the request timeout passes; a late answer is
//! dropped. When the server closes or the network drops, every waiting request fails with [`ObsError::Closed`] and
//! [`ObsClient::is_connected`] turns false; [`ObsService`](crate::service::ObsService) reconnects.
//!
//! Loopback by default: a host other than this computer is [`ObsError::RemoteNotAllowed`] before any socket opens,
//! unless [`ClientConfig::allow_remote`] (a person's setting, for OBS on another PC; the link is then unencrypted, so the
//! challenge answer is its only protection).
//!
//! The password is used once per handshake and never stored, logged or put in an error.

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tungstenite::protocol::WebSocket;
use tungstenite::{Message as WsMessage, Utf8Bytes};

use crate::auth::auth_response;
use crate::limits::{CloseCode, EventSubscription};
use crate::protocol::{Frame, Identify, Message, ObsEvent, ProtocolError, RequestMsg, RequestType, ResponseMsg};

/// How long a request waits for its answer.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// How long connecting (TCP, the WebSocket handshake and identification) may take.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// How often the connection thread looks for requests to send while no message arrives.
const POLL: Duration = Duration::from_millis(10);
/// The events the client subscribes to: general (exit), scenes, scene items and outputs (stream, record, replay).
pub const SUBSCRIPTIONS: EventSubscription = EventSubscription(
    EventSubscription::GENERAL.0
        | EventSubscription::SCENES.0
        | EventSubscription::SCENE_ITEMS.0
        | EventSubscription::OUTPUTS.0,
);
/// The events one subscriber keeps unread; past it the oldest go and are counted.
pub const EVENT_BACKLOG: usize = 256;

/// Where and how to connect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientConfig {
    /// The machine OBS runs on. Default `127.0.0.1`.
    pub host: String,
    /// The server's port. Default 4455.
    pub port: u16,
    /// Whether a host other than this computer may be reached (unencrypted). Default false.
    pub allow_remote: bool,
    /// How long a request waits for its answer. Default [`REQUEST_TIMEOUT`].
    pub request_timeout: Duration,
    /// The events asked for. Default [`SUBSCRIPTIONS`].
    pub subscriptions: EventSubscription,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            host: crate::service::DEFAULT_HOST.to_owned(),
            port: crate::limits::DEFAULT_PORT,
            allow_remote: false,
            request_timeout: REQUEST_TIMEOUT,
            subscriptions: SUBSCRIPTIONS,
        }
    }
}

impl ClientConfig {
    /// The default settings for `host:port`.
    #[must_use]
    pub fn at(host: &str, port: u16) -> Self {
        Self { host: host.to_owned(), port, ..Self::default() }
    }

    /// Whether `host` is this computer.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        let host = self.host.trim_matches(['[', ']']);
        host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }
}

/// One subscriber's events: at most [`EVENT_BACKLOG`] unread, the oldest dropped and counted past that.
#[derive(Debug)]
pub struct EventStream {
    queue: Arc<EventQueue>,
}

#[derive(Debug, Default)]
struct EventQueue {
    state: Mutex<(VecDeque<ObsEvent>, u64)>,
    ready: Condvar,
}

impl EventStream {
    /// The next event, waiting up to `timeout`.
    #[must_use]
    pub fn recv_timeout(&self, timeout: Duration) -> Option<ObsEvent> {
        let guard = lock(&self.queue.state);
        let (mut guard, _) = self
            .queue
            .ready
            .wait_timeout_while(guard, timeout, |(q, _)| q.is_empty())
            .unwrap_or_else(PoisonError::into_inner);
        guard.0.pop_front()
    }

    /// The next event if one is waiting.
    #[must_use]
    pub fn try_recv(&self) -> Option<ObsEvent> {
        lock(&self.queue.state).0.pop_front()
    }

    /// How many events were dropped because this subscriber fell behind.
    #[must_use]
    pub fn lagged(&self) -> u64 {
        lock(&self.queue.state).1
    }
}

/// Where a connection's events go: every live [`EventStream`]. A service keeps one hub across reconnects, so a
/// subscriber hears the new connection's events too.
#[derive(Debug, Default)]
pub struct EventHub {
    subscribers: Mutex<Vec<Weak<EventQueue>>>,
}

impl EventHub {
    /// A hub with no subscribers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A stream of the events published from now on.
    #[must_use]
    pub fn subscribe(&self) -> EventStream {
        let queue = Arc::new(EventQueue::default());
        lock(&self.subscribers).push(Arc::downgrade(&queue));
        EventStream { queue }
    }

    /// Hands `event` to every live subscriber.
    pub fn publish(&self, event: &ObsEvent) {
        lock(&self.subscribers).retain(|weak| {
            let Some(queue) = weak.upgrade() else { return false };
            let mut state = lock(&queue.state);
            if state.0.len() >= EVENT_BACKLOG {
                state.0.pop_front();
                state.1 += 1;
            }
            state.0.push_back(event.clone());
            queue.ready.notify_all();
            true
        });
    }
}

/// Why talking to OBS failed. No variant carries the password or anything derived from it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ObsError {
    /// The host isn't this computer and remote connections aren't allowed (Settings, Connectors, OBS).
    #[error("OBS at {0} is on another computer; allow remote connections in the OBS settings first")]
    RemoteNotAllowed(String),
    /// OBS could not be reached: not running, the remote-control server off, or a wrong port.
    #[error("could not reach OBS at {addr}: {reason}")]
    Unreachable {
        /// Where we tried.
        addr: String,
        /// What the network said.
        reason: String,
    },
    /// The server wants a password and none was given, or the one given was wrong.
    #[error("OBS refused the connection: {0}")]
    Auth(String),
    /// The server sent something this client cannot read.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),
    /// OBS answered a request with a failure.
    #[error("OBS refused {request}: {comment} (code {code})")]
    Refused {
        /// The request.
        request: String,
        /// The status code.
        code: u32,
        /// OBS's reason.
        comment: String,
    },
    /// No answer within [`REQUEST_TIMEOUT`].
    #[error("OBS did not answer {0} in time")]
    Timeout(String),
    /// The connection is closed.
    #[error("the connection to OBS is closed{}", .0.as_deref().map(|r| format!(": {r}")).unwrap_or_default())]
    Closed(Option<String>),
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A request on its way to the connection thread.
struct Pending {
    msg: RequestMsg,
    reply: Sender<Result<ResponseMsg, ObsError>>,
}

/// What the connection learnt at identification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInfo {
    /// The server plugin's version, from `Hello`.
    pub websocket_version: String,
    /// The negotiated RPC version.
    pub rpc_version: u32,
}

/// One live connection to OBS.
pub struct ObsClient {
    requests: Sender<Pending>,
    hub: Arc<EventHub>,
    request_timeout: Duration,
    unencrypted: bool,
    connected: Arc<AtomicBool>,
    close_reason: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    next_id: AtomicU64,
    info: ServerInfo,
    addr: String,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for ObsClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObsClient").field("addr", &self.addr).field("info", &self.info).finish_non_exhaustive()
    }
}

type Socket = WebSocket<TcpStream>;

fn send_frame(ws: &mut Socket, msg: &Message) -> Result<(), ObsError> {
    let frame = msg.to_frame().map_err(|e| ObsError::Closed(Some(format!("a message could not be encoded: {e}"))))?;
    let text = serde_json::to_string(&frame).map_err(|e| ObsError::Closed(Some(e.to_string())))?;
    ws.send(WsMessage::Text(Utf8Bytes::from(text))).map_err(|e| ObsError::Closed(Some(e.to_string())))
}

/// Reads the next typed message; `Ok(None)` when the read timed out.
fn read_message(ws: &mut Socket) -> Result<Option<Message>, ObsError> {
    match ws.read() {
        Ok(WsMessage::Text(text)) => {
            let frame: Frame = serde_json::from_str(text.as_str())
                .map_err(|_| ObsError::Protocol(ProtocolError::BadData { op: u8::MAX }))?;
            Ok(Some(Message::from_frame(&frame)?))
        }
        Ok(WsMessage::Close(frame)) => {
            let reason = frame.map(|f| {
                let code = CloseCode::from_u16(u16::from(f.code));
                format!("{code:?} ({})", u16::from(f.code))
            });
            Err(close_error(reason))
        }
        Ok(_) => Ok(None),
        Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
        Err(e) => Err(ObsError::Closed(Some(e.to_string()))),
    }
}

fn close_error(reason: Option<String>) -> ObsError {
    match reason {
        Some(r) if r.starts_with("AuthenticationFailed") => ObsError::Auth("the password is wrong".into()),
        other => ObsError::Closed(other),
    }
}

fn resolve(host: &str, port: u16) -> Result<SocketAddr, ObsError> {
    let addr = format!("{host}:{port}");
    addr.to_socket_addrs()
        .map_err(|e| ObsError::Unreachable { addr: addr.clone(), reason: e.to_string() })?
        .next()
        .ok_or_else(|| ObsError::Unreachable { addr, reason: "the host name resolves to nothing".into() })
}

impl ObsClient {
    /// Connects to OBS at `host:port` with the default settings and a hub of its own.
    ///
    /// # Errors
    /// As [`open`](Self::open).
    pub fn connect(host: &str, port: u16, password: Option<&str>) -> Result<Self, ObsError> {
        Self::open(&ClientConfig::at(host, port), password, Arc::new(EventHub::new()))
    }

    /// Connects as `config` says, identifying with `password` if the server asks for one, publishing events to
    /// `hub`.
    ///
    /// # Errors
    /// [`ObsError::RemoteNotAllowed`] for another computer without `allow_remote`; [`ObsError::Unreachable`] when
    /// nothing answers; [`ObsError::Auth`] when a password is needed and missing or wrong; [`ObsError::Protocol`] or
    /// [`ObsError::Closed`] when the handshake goes wrong.
    pub fn open(config: &ClientConfig, password: Option<&str>, hub: Arc<EventHub>) -> Result<Self, ObsError> {
        let (host, port) = (config.host.as_str(), config.port);
        let loopback = config.is_loopback();
        if !loopback && !config.allow_remote {
            return Err(ObsError::RemoteNotAllowed(format!("{host}:{port}")));
        }
        let addr = resolve(host, port)?;
        let unreachable = |reason: String| ObsError::Unreachable { addr: addr.to_string(), reason };
        let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| unreachable(e.to_string()))?;
        stream.set_read_timeout(Some(CONNECT_TIMEOUT)).map_err(|e| unreachable(e.to_string()))?;
        stream.set_nodelay(true).map_err(|e| unreachable(e.to_string()))?;
        let url = format!("ws://{host}:{port}/");
        let (mut ws, _) = tungstenite::client(url.as_str(), stream).map_err(|e| unreachable(e.to_string()))?;
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        let next = |ws: &mut Socket| -> Result<Message, ObsError> {
            loop {
                if let Some(m) = read_message(ws)? {
                    return Ok(m);
                }
                if Instant::now() > deadline {
                    return Err(ObsError::Timeout("the handshake".into()));
                }
            }
        };
        let Message::Hello(hello) = next(&mut ws)? else {
            return Err(ObsError::Closed(Some("the server did not say hello".into())));
        };
        let mut identify = Identify::new().with_subscriptions(config.subscriptions);
        if let Some(challenge) = &hello.authentication {
            let Some(password) = password else {
                return Err(ObsError::Auth("OBS asks for a password and none is stored".into()));
            };
            identify = identify.with_authentication(auth_response(password, &challenge.salt, &challenge.challenge));
        }
        send_frame(&mut ws, &Message::Identify(identify))?;
        let rpc_version = loop {
            // Events cannot come before identification; anything else is ignored until it does.
            if let Message::Identified(identified) = next(&mut ws)? {
                break identified.negotiated_rpc_version;
            }
        };
        ws.get_ref().set_read_timeout(Some(POLL)).map_err(|e| unreachable(e.to_string()))?;
        let info = ServerInfo { websocket_version: hello.obs_web_socket_version, rpc_version };
        let (requests, queue) = channel::<Pending>();
        let connected = Arc::new(AtomicBool::new(true));
        let close_reason = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            ws,
            queue,
            hub: Arc::clone(&hub),
            connected: Arc::clone(&connected),
            close_reason: Arc::clone(&close_reason),
            stop: Arc::clone(&stop),
            waiting: HashMap::new(),
        };
        let thread = std::thread::Builder::new()
            .name("omuzaki-obs".into())
            .spawn(move || worker.run())
            .map_err(|e| ObsError::Closed(Some(format!("the connection thread could not start: {e}"))))?;
        Ok(Self {
            requests,
            hub,
            request_timeout: config.request_timeout,
            unencrypted: !loopback,
            connected,
            close_reason,
            stop,
            next_id: AtomicU64::new(1),
            info,
            addr: addr.to_string(),
            thread: Some(thread),
        })
    }

    /// What the server said about itself.
    #[must_use]
    pub fn info(&self) -> &ServerInfo {
        &self.info
    }

    /// Where the connection goes.
    #[must_use]
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Whether the connection is still open.
    #[must_use]
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// Why the connection closed, once it has.
    #[must_use]
    pub fn close_reason(&self) -> Option<String> {
        lock(&self.close_reason).clone()
    }

    /// Whether the link crosses the network unencrypted (a remote OBS).
    #[must_use]
    pub fn unencrypted(&self) -> bool {
        self.unencrypted
    }

    /// A stream of the events OBS sends from now on.
    #[must_use]
    pub fn subscribe(&self) -> EventStream {
        self.hub.subscribe()
    }

    /// Sends `request` and waits for its answer.
    ///
    /// # Errors
    /// [`ObsError::Refused`] when OBS says no; [`ObsError::Timeout`]; [`ObsError::Closed`] when the connection is gone;
    /// [`ObsError::Protocol`] when the answer cannot be read.
    pub fn request<R: RequestType>(&self, request: &R) -> Result<R::Response, ObsError> {
        if !self.is_connected() {
            return Err(ObsError::Closed(self.close_reason()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        let msg = RequestMsg::new(id, request).map_err(|e| ObsError::Closed(Some(e.to_string())))?;
        let (reply, answer) = channel();
        self.requests.send(Pending { msg, reply }).map_err(|_| ObsError::Closed(self.close_reason()))?;
        let response = match answer.recv_timeout(self.request_timeout) {
            Ok(result) => result?,
            Err(RecvTimeoutError::Timeout) => return Err(ObsError::Timeout(R::NAME.into())),
            Err(RecvTimeoutError::Disconnected) => return Err(ObsError::Closed(self.close_reason())),
        };
        if !response.request_status.result {
            return Err(ObsError::Refused {
                request: R::NAME.into(),
                code: response.request_status.code,
                comment: response.request_status.comment.clone().unwrap_or_default(),
            });
        }
        Ok(response.data::<R>()?)
    }
}

impl Drop for ObsClient {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The connection thread's state.
struct Worker {
    ws: Socket,
    queue: Receiver<Pending>,
    hub: Arc<EventHub>,
    connected: Arc<AtomicBool>,
    close_reason: Arc<Mutex<Option<String>>>,
    stop: Arc<AtomicBool>,
    waiting: HashMap<String, Sender<Result<ResponseMsg, ObsError>>>,
}

impl Worker {
    fn run(mut self) {
        let end = loop {
            if self.stop.load(Ordering::Acquire) {
                let _ = self.ws.close(None);
                // Lets the close handshake go out; errors here only mean the server went first.
                let _ = self.ws.flush();
                break None;
            }
            if let Err(e) = self.send_queued() {
                break Some(e);
            }
            match read_message(&mut self.ws) {
                Ok(Some(message)) => self.dispatch(message),
                Ok(None) => {}
                Err(e) => break Some(e),
            }
        };
        let reason = end.map(|e| e.to_string());
        lock(&self.close_reason).clone_from(&reason);
        self.connected.store(false, Ordering::Release);
        for (_, reply) in self.waiting.drain() {
            let _ = reply.send(Err(ObsError::Closed(reason.clone())));
        }
        while let Ok(pending) = self.queue.try_recv() {
            let _ = pending.reply.send(Err(ObsError::Closed(reason.clone())));
        }
    }

    fn send_queued(&mut self) -> Result<(), ObsError> {
        while let Ok(pending) = self.queue.try_recv() {
            let id = pending.msg.request_id.clone();
            match send_frame(&mut self.ws, &Message::Request(pending.msg)) {
                Ok(()) => {
                    self.waiting.insert(id, pending.reply);
                }
                Err(e) => {
                    let _ = pending.reply.send(Err(e.clone()));
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    fn dispatch(&mut self, message: Message) {
        match message {
            Message::RequestResponse(response) => {
                if let Some(reply) = self.waiting.remove(&response.request_id) {
                    let _ = reply.send(Ok(response));
                }
            }
            Message::Event(event) => {
                // A malformed known event is dropped rather than closing the connection.
                if let Ok(event) = ObsEvent::from_msg(&event) {
                    self.hub.publish(&event);
                }
            }
            _ => {}
        }
    }
}
