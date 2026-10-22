//! [`FakeObs`]: an OBS remote-control server for tests, on a loopback port (spec v3 §39.1 rule 6: a fake for every
//! connector). Every BR-19 test runs against it, so none needs OBS, an account or the network.
//!
//! It speaks the protocol as the document describes it:
//! - the handshake: `Hello` (with a challenge and salt from the seeded generator when a password is set), `Identify`
//!   checked with [`auth_response`], then `Identified`, or a close with `AuthenticationFailed` (4009);
//! - scenes and their items, the program scene;
//! - the stream, record and replay-buffer state machines, which emit their `...StateChanged` events step by step;
//! - `StartRecord`/`StopRecord` and `SaveReplayBuffer` producing paths under a fake folder;
//! - events only to clients whose `eventSubscriptions` include the category.
//!
//! Faults: [`drop_connection`](FakeObs::drop_connection), [`down`](FakeObs::down) and [`up`](FakeObs::up) (the
//! server going away and coming back on the same port), [`delay`](FakeObs::delay) and [`fail`](FakeObs::fail).
//! [`received`](FakeObs::received) and every internal list are bounded.
//!
//! Built for tests only (`cfg(test)` or the `testing` feature).

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode as WsCloseCode;
use tungstenite::{Message as WsMessage, Utf8Bytes, WebSocket};

use crate::auth::auth_response;
use crate::limits::{
    CloseCode, EventSubscription, STATUS_OUTPUT_NOT_RUNNING, STATUS_OUTPUT_RUNNING, STATUS_RESOURCE_NOT_FOUND,
};
use crate::protocol::{
    AuthChallenge, Frame, Hello, Identified, Message, ObsEvent, OutputState, RequestStatus, ResponseMsg,
};

#[cfg(test)]
mod tests;

/// The most entries any list the fake keeps holds.
pub const MAX_KEPT: usize = 1000;
/// The folder the fake's recordings and replays are "written" to.
pub const FAKE_FOLDER: &str = "/fake/obs";
/// The version the fake reports.
pub const FAKE_VERSION: &str = "5.5.0";
/// How often a connection thread looks for events to send and faults to act on.
const TICK: Duration = Duration::from_millis(5);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A scene item: a source placed in a scene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeItem {
    /// The item's id within its scene.
    pub id: i64,
    /// The source's name.
    pub source: String,
    /// Whether it's visible.
    pub enabled: bool,
}

/// A scene and its items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeScene {
    /// The scene's name.
    pub name: String,
    /// Its items, top first.
    pub items: Vec<FakeItem>,
}

impl FakeScene {
    /// A scene with visible items named `sources`, numbered from 1.
    #[must_use]
    pub fn new(name: &str, sources: &[&str]) -> Self {
        let items = (1..)
            .zip(sources)
            .map(|(id, source)| FakeItem { id, source: (*source).to_owned(), enabled: true })
            .collect();
        Self { name: name.to_owned(), items }
    }
}

/// How the fake starts.
#[derive(Clone, Debug)]
pub struct FakeConfig {
    /// The server password; `None` asks for none.
    pub password: Option<String>,
    /// The seed of the challenge and salt.
    pub seed: u64,
    /// The scenes, in the order a person sees them (top first). The first is on program.
    pub scenes: Vec<FakeScene>,
}

impl Default for FakeConfig {
    /// No password, seed 1, and three scenes: `Main` (Camera, Avatar, Alerts), `Starting soon` (Countdown) and `BRB`
    /// (Loop).
    fn default() -> Self {
        Self {
            password: None,
            seed: 1,
            scenes: vec![
                FakeScene::new("Main", &["Camera", "Avatar", "Alerts"]),
                FakeScene::new("Starting soon", &["Countdown"]),
                FakeScene::new("BRB", &["Loop"]),
            ],
        }
    }
}

/// One request the fake received.
#[derive(Clone, Debug, PartialEq)]
pub struct Received {
    /// Its `requestType`.
    pub request_type: String,
    /// Its `requestData`, or `null`.
    pub data: Value,
}

/// An output's state, as the fake's state machines hold it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Output {
    /// Not running.
    #[default]
    Stopped,
    /// Running.
    Started,
}

/// What the fake holds now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FakeState {
    /// The scenes and their items.
    pub scenes: Vec<FakeScene>,
    /// The program scene.
    pub program: String,
    /// The stream.
    pub stream: Output,
    /// The recording.
    pub record: Output,
    /// The replay buffer.
    pub replay: Output,
    /// The last replay saved.
    pub last_replay: Option<String>,
    /// Open client connections.
    pub clients: usize,
}

/// A client connection's handle, for faults and events.
struct Client {
    subscriptions: EventSubscription,
    outbox: VecDeque<ObsEvent>,
    close: bool,
}

struct Shared {
    state: FakeState,
    received: VecDeque<Received>,
    delays: HashMap<String, Duration>,
    failures: HashMap<String, (u32, String)>,
    clients: HashMap<u64, Client>,
    files: u64,
    rng: u64,
}

impl Shared {
    /// The next number of the seeded generator (xorshift64*).
    fn random(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn random_b64(&mut self) -> String {
        let bytes: Vec<u8> = (0..4).flat_map(|_| self.random().to_le_bytes()).collect();
        STANDARD.encode(bytes)
    }

    /// Queues `event` for every client subscribed to its category.
    fn emit(&mut self, event: &ObsEvent) {
        for client in self.clients.values_mut() {
            if client.subscriptions.contains(event.intent()) && client.outbox.len() < MAX_KEPT {
                client.outbox.push_back(event.clone());
            }
        }
    }

    fn file(&mut self, kind: &str) -> String {
        self.files += 1;
        format!("{FAKE_FOLDER}/{kind}-{}.mkv", self.files)
    }

    fn scene(&self, name: &str) -> Option<&FakeScene> {
        self.state.scenes.iter().find(|s| s.name == name)
    }
}

/// A fake OBS on a loopback port.
pub struct FakeObs {
    addr: SocketAddr,
    shared: Arc<Mutex<Shared>>,
    password: Option<String>,
    stop: Arc<AtomicBool>,
    accept: Option<JoinHandle<()>>,
    next_client: Arc<AtomicU64>,
}

impl std::fmt::Debug for FakeObs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeObs").field("addr", &self.addr).finish_non_exhaustive()
    }
}

impl FakeObs {
    /// Starts the fake on `127.0.0.1` and a free port.
    ///
    /// # Panics
    /// When no loopback port can be bound: a test can't run without one.
    #[must_use]
    #[allow(clippy::expect_used)] // a test fixture: no port means no test
    pub fn start(config: FakeConfig) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a loopback port for the fake OBS");
        let addr = listener.local_addr().expect("the fake OBS's address");
        let program = config.scenes.first().map(|s| s.name.clone()).unwrap_or_default();
        let shared = Arc::new(Mutex::new(Shared {
            state: FakeState {
                scenes: config.scenes,
                program,
                stream: Output::Stopped,
                record: Output::Stopped,
                replay: Output::Stopped,
                last_replay: None,
                clients: 0,
            },
            received: VecDeque::new(),
            delays: HashMap::new(),
            failures: HashMap::new(),
            clients: HashMap::new(),
            files: 0,
            rng: config.seed.max(1),
        }));
        let mut fake = Self {
            addr,
            shared,
            password: config.password,
            stop: Arc::new(AtomicBool::new(false)),
            accept: None,
            next_client: Arc::new(AtomicU64::new(1)),
        };
        fake.listen(listener);
        fake
    }

    fn listen(&mut self, listener: TcpListener) {
        self.stop.store(false, Ordering::Release);
        let (shared, stop, password, next) =
            (Arc::clone(&self.shared), Arc::clone(&self.stop), self.password.clone(), Arc::clone(&self.next_client));
        self.accept = Some(std::thread::spawn(move || {
            for stream in listener.incoming() {
                if stop.load(Ordering::Acquire) {
                    return;
                }
                let Ok(stream) = stream else { continue };
                let (shared, stop, password) = (Arc::clone(&shared), Arc::clone(&stop), password.clone());
                let id = next.fetch_add(1, Ordering::Relaxed);
                std::thread::spawn(move || serve(stream, id, &shared, &stop, password.as_deref()));
            }
        }));
    }

    /// The port it listens on.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// A copy of what it holds now.
    #[must_use]
    pub fn state(&self) -> FakeState {
        let shared = lock(&self.shared);
        let mut state = shared.state.clone();
        state.clients = shared.clients.len();
        state
    }

    /// Switches the program scene as a person in OBS would, telling subscribed clients.
    pub fn set_program_scene(&self, name: &str) {
        let mut shared = lock(&self.shared);
        name.clone_into(&mut shared.state.program);
        shared.emit(&ObsEvent::CurrentProgramSceneChanged { scene_name: name.to_owned() });
    }

    /// Closes every client's connection (the server stays up).
    pub fn drop_connection(&self) {
        for client in lock(&self.shared).clients.values_mut() {
            client.close = true;
        }
    }

    /// The server goes away: it stops listening and closes every connection.
    pub fn down(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.drop_connection();
        // Wakes the accept loop so it sees the stop flag.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
    }

    /// The server comes back on the same port.
    ///
    /// # Panics
    /// When the port can't be bound again.
    #[allow(clippy::expect_used)] // a test fixture
    pub fn up(&mut self) {
        let listener = (0..100)
            .find_map(|_| {
                TcpListener::bind(self.addr).ok().or_else(|| {
                    std::thread::sleep(Duration::from_millis(10));
                    None
                })
            })
            .expect("the fake OBS's port, again");
        self.listen(listener);
    }

    /// Goes down and comes straight back up.
    pub fn restart(&mut self) {
        self.down();
        self.up();
    }

    /// Answers `request_type` only after `by`.
    pub fn delay(&self, request_type: &str, by: Duration) {
        lock(&self.shared).delays.insert(request_type.to_owned(), by);
    }

    /// Fails `request_type` with this status.
    pub fn fail(&self, request_type: &str, code: u32, comment: &str) {
        lock(&self.shared).failures.insert(request_type.to_owned(), (code, comment.to_owned()));
    }

    /// The requests received, oldest first (at most [`MAX_KEPT`]).
    #[must_use]
    pub fn received(&self) -> Vec<Received> {
        lock(&self.shared).received.iter().cloned().collect()
    }

    /// How many requests of `request_type` arrived.
    #[must_use]
    pub fn count(&self, request_type: &str) -> usize {
        lock(&self.shared).received.iter().filter(|r| r.request_type == request_type).count()
    }
}

impl Drop for FakeObs {
    fn drop(&mut self) {
        self.down();
    }
}

type Socket = WebSocket<TcpStream>;

fn send(ws: &mut Socket, msg: &Message) -> bool {
    let Ok(frame) = msg.to_frame() else { return false };
    let Ok(text) = serde_json::to_string(&frame) else { return false };
    ws.send(WsMessage::Text(Utf8Bytes::from(text))).is_ok()
}

/// The next message; `Ok(None)` on a read timeout, `Err` when the connection ended.
fn read(ws: &mut Socket) -> Result<Option<Message>, ()> {
    match ws.read() {
        Ok(WsMessage::Text(text)) => {
            let frame: Frame = serde_json::from_str(text.as_str()).map_err(|_| ())?;
            Message::from_frame(&frame).map(Some).map_err(|_| ())
        }
        Ok(WsMessage::Close(_)) => Err(()),
        Ok(_) => Ok(None),
        Err(tungstenite::Error::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
        Err(_) => Err(()),
    }
}

fn close(ws: &mut Socket, code: CloseCode, reason: &str) {
    let _ = ws.close(Some(CloseFrame { code: WsCloseCode::from(code.as_u16()), reason: reason.to_owned().into() }));
    let _ = ws.flush();
}

/// One client connection, from the handshake to its end.
fn serve(stream: TcpStream, id: u64, shared: &Mutex<Shared>, stop: &AtomicBool, password: Option<&str>) {
    let _ = stream.set_nodelay(true);
    let Ok(mut ws) = tungstenite::accept(stream) else { return };
    let challenge = password.map(|_| {
        let mut s = lock(shared);
        (s.random_b64(), s.random_b64())
    });
    let auth = challenge.as_ref().map(|(c, salt)| AuthChallenge::new(c.clone(), salt.clone()));
    if !send(&mut ws, &Message::Hello(Hello::new(FAKE_VERSION, auth))) {
        return;
    }
    let identify = loop {
        match read(&mut ws) {
            Ok(Some(Message::Identify(identify))) => break identify,
            Ok(_) => {}
            Err(()) => return,
        }
    };
    if let (Some(password), Some((challenge, salt))) = (password, &challenge)
        && identify.authentication.as_deref() != Some(auth_response(password, salt, challenge).as_str())
    {
        close(&mut ws, CloseCode::AuthenticationFailed, "Authentication failed.");
        return;
    }
    let subscriptions = EventSubscription(identify.event_subscriptions.unwrap_or(EventSubscription::ALL.0));
    lock(shared).clients.insert(id, Client { subscriptions, outbox: VecDeque::new(), close: false });
    if !send(&mut ws, &Message::Identified(Identified::new(1))) {
        lock(shared).clients.remove(&id);
        return;
    }
    let _ = ws.get_ref().set_read_timeout(Some(TICK));
    // Answers waiting out a delay: when they're due, and the message.
    let mut later: Vec<(std::time::Instant, Message)> = Vec::new();
    loop {
        let (events, closing) = {
            let mut s = lock(shared);
            let Some(client) = s.clients.get_mut(&id) else { break };
            (client.outbox.drain(..).collect::<Vec<_>>(), client.close || stop.load(Ordering::Acquire))
        };
        if closing {
            close(&mut ws, CloseCode::UnknownReason, "the server is going away");
            break;
        }
        let now = std::time::Instant::now();
        let (due, waiting): (Vec<_>, Vec<_>) = later.into_iter().partition(|(at, _)| *at <= now);
        later = waiting;
        let mut ok = due.into_iter().all(|(_, m)| send(&mut ws, &m));
        ok = ok && events.iter().all(|e| e.to_msg().is_ok_and(|m| send(&mut ws, &Message::Event(m))));
        if !ok {
            break;
        }
        match read(&mut ws) {
            Ok(Some(Message::Request(req))) => {
                let data = req.request_data.clone().unwrap_or(Value::Null);
                let (status, answer, delay) = {
                    let mut s = lock(shared);
                    if s.received.len() >= MAX_KEPT {
                        s.received.pop_front();
                    }
                    s.received.push_back(Received { request_type: req.request_type.clone(), data: data.clone() });
                    let delay = s.delays.get(&req.request_type).copied();
                    let (status, answer) = match s.failures.get(&req.request_type).cloned() {
                        Some((code, comment)) => (RequestStatus::failure(code, Some(comment)), None),
                        None => handle(&mut s, &req.request_type, &data),
                    };
                    (status, answer, delay)
                };
                let Ok(mut response) = ResponseMsg::new::<crate::protocol::GetVersion>(req.request_id, status, None)
                else {
                    break;
                };
                response.request_type = req.request_type;
                response.response_data = answer;
                let message = Message::RequestResponse(response);
                match delay {
                    Some(by) => later.push((now + by, message)),
                    None => {
                        if !send(&mut ws, &message) {
                            break;
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(()) => break,
        }
    }
    lock(shared).clients.remove(&id);
}

fn not_found(what: &str) -> (RequestStatus, Option<Value>) {
    (RequestStatus::failure(STATUS_RESOURCE_NOT_FOUND, Some(format!("No source was found by the name of `{what}`."))), None)
}

fn ok(data: Value) -> (RequestStatus, Option<Value>) {
    (RequestStatus::success(), Some(data))
}

fn done() -> (RequestStatus, Option<Value>) {
    (RequestStatus::success(), None)
}

/// Which output a request is about, and its event.
#[derive(Clone, Copy)]
enum Which {
    Stream,
    Record,
    Replay,
}

/// Steps an output through starting and started (or stopping and stopped), emitting each state.
fn step(s: &mut Shared, which: Which, start: bool) -> Result<Option<String>, (RequestStatus, Option<Value>)> {
    let current = match which {
        Which::Stream => s.state.stream,
        Which::Record => s.state.record,
        Which::Replay => s.state.replay,
    };
    match (current, start) {
        (Output::Started, true) => {
            return Err((RequestStatus::failure(STATUS_OUTPUT_RUNNING, Some("The output is already running.".into())), None));
        }
        (Output::Stopped, false) => {
            return Err((RequestStatus::failure(STATUS_OUTPUT_NOT_RUNNING, Some("The output is not running.".into())), None));
        }
        _ => {}
    }
    let (middle, end) =
        if start { (OutputState::Starting, OutputState::Started) } else { (OutputState::Stopping, OutputState::Stopped) };
    let path = (matches!(which, Which::Record) && !start).then(|| s.file("record"));
    for (state, active) in [(middle, !start), (end, start)] {
        let event = match which {
            Which::Stream => ObsEvent::StreamStateChanged { output_active: active, output_state: state },
            Which::Record => ObsEvent::RecordStateChanged {
                output_active: active,
                output_state: state,
                output_path: (state == OutputState::Stopped).then(|| path.clone()).flatten(),
            },
            Which::Replay => ObsEvent::ReplayBufferStateChanged { output_active: active, output_state: state },
        };
        s.emit(&event);
    }
    let new = if start { Output::Started } else { Output::Stopped };
    match which {
        Which::Stream => s.state.stream = new,
        Which::Record => s.state.record = new,
        Which::Replay => s.state.replay = new,
    }
    Ok(path)
}

/// The fake's answer to one request.
fn handle(s: &mut Shared, request: &str, data: &Value) -> (RequestStatus, Option<Value>) {
    let text = |key: &str| data[key].as_str().unwrap_or_default().to_owned();
    match request {
        "GetVersion" => ok(json!({
            "obsVersion": "31.0.0", "obsWebSocketVersion": FAKE_VERSION, "rpcVersion": 1,
            "availableRequests": [], "supportedImageFormats": ["png"], "platform": "fake", "platformDescription": "FakeObs",
        })),
        "GetSceneList" => {
            // OBS lists scenes bottom-up.
            let scenes: Vec<Value> = s
                .state
                .scenes
                .iter()
                .rev()
                .enumerate()
                .map(|(i, scene)| json!({ "sceneName": scene.name, "sceneIndex": i }))
                .collect();
            ok(json!({ "currentProgramSceneName": s.state.program, "currentPreviewSceneName": null, "scenes": scenes }))
        }
        "GetCurrentProgramScene" => ok(json!({ "sceneName": s.state.program, "sceneUuid": "fake-uuid" })),
        "SetCurrentProgramScene" => {
            let name = text("sceneName");
            if s.scene(&name).is_none() {
                return not_found(&name);
            }
            s.state.program.clone_from(&name);
            s.emit(&ObsEvent::CurrentProgramSceneChanged { scene_name: name });
            done()
        }
        "GetSceneItemList" | "GetSceneItemId" | "SetSceneItemEnabled" => items(s, request, data),
        "GetStreamStatus" => ok(json!({
            "outputActive": s.state.stream == Output::Started, "outputReconnecting": false,
            "outputTimecode": "00:00:00.000", "outputDuration": 0, "outputCongestion": 0.0, "outputBytes": 0,
            "outputSkippedFrames": 0, "outputTotalFrames": 0,
        })),
        "GetRecordStatus" => ok(json!({
            "outputActive": s.state.record == Output::Started, "outputPaused": false,
            "outputTimecode": "00:00:00.000", "outputDuration": 0, "outputBytes": 0,
        })),
        "GetReplayBufferStatus" => ok(json!({ "outputActive": s.state.replay == Output::Started })),
        "StartStream" | "StopStream" => match step(s, Which::Stream, request == "StartStream") {
            Ok(_) => done(),
            Err(e) => e,
        },
        "StartRecord" => match step(s, Which::Record, true) {
            Ok(_) => done(),
            Err(e) => e,
        },
        "StopRecord" => match step(s, Which::Record, false) {
            Ok(path) => ok(json!({ "outputPath": path.unwrap_or_default() })),
            Err(e) => e,
        },
        "StartReplayBuffer" | "StopReplayBuffer" => match step(s, Which::Replay, request == "StartReplayBuffer") {
            Ok(_) => done(),
            Err(e) => e,
        },
        "SaveReplayBuffer" => {
            if s.state.replay != Output::Started {
                return (
                    RequestStatus::failure(STATUS_OUTPUT_NOT_RUNNING, Some("The replay buffer is not active.".into())),
                    None,
                );
            }
            let path = s.file("replay");
            s.state.last_replay = Some(path.clone());
            s.emit(&ObsEvent::ReplayBufferSaved { saved_replay_path: path });
            done()
        }
        "GetLastReplayBufferReplay" => match s.state.last_replay.clone() {
            Some(path) => ok(json!({ "savedReplayPath": path })),
            None => (RequestStatus::failure(STATUS_RESOURCE_NOT_FOUND, Some("No replay saved yet.".into())), None),
        },
        _ => (RequestStatus::failure(204, Some(format!("unknown request type {request}"))), None),
    }
}

/// The fake's answer to a scene-item request.
fn items(s: &mut Shared, request: &str, data: &Value) -> (RequestStatus, Option<Value>) {
    let text = |key: &str| data[key].as_str().unwrap_or_default().to_owned();
    match request {
        "GetSceneItemList" => {
            let name = text("sceneName");
            let Some(scene) = s.scene(&name) else { return not_found(&name) };
            let items: Vec<Value> = scene
                .items
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    json!({ "sceneItemId": item.id, "sourceName": item.source, "sceneItemEnabled": item.enabled,
                            "sceneItemIndex": scene.items.len() - 1 - i })
                })
                .collect();
            ok(json!({ "sceneItems": items }))
        }
        "GetSceneItemId" => {
            let (name, source) = (text("sceneName"), text("sourceName"));
            let Some(scene) = s.scene(&name) else { return not_found(&name) };
            match scene.items.iter().find(|i| i.source == source) {
                Some(item) => ok(json!({ "sceneItemId": item.id })),
                None => not_found(&source),
            }
        }
        "SetSceneItemEnabled" => {
            let name = text("sceneName");
            let id = data["sceneItemId"].as_i64().unwrap_or_default();
            let enabled = data["sceneItemEnabled"].as_bool().unwrap_or_default();
            let Some(scene) = s.state.scenes.iter_mut().find(|sc| sc.name == name) else { return not_found(&name) };
            let Some(item) = scene.items.iter_mut().find(|i| i.id == id) else { return not_found(&id.to_string()) };
            item.enabled = enabled;
            s.emit(&ObsEvent::SceneItemEnableStateChanged {
                scene_name: name,
                scene_item_id: id,
                scene_item_enabled: enabled,
            });
            done()
        }
        _ => (RequestStatus::failure(204, Some(format!("unknown request type {request}"))), None),
    }
}
