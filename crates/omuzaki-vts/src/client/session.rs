//! The bridge thread: connect, authenticate, set up, then feed frames and send triggers until the session ends.

use std::collections::{HashMap, VecDeque};
use std::io::ErrorKind;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tungstenite::protocol::WebSocket;
use tungstenite::{Error as WsError, Message as WsMessage};

use super::{BACKOFF, BridgeStatus, ClientConfig, Command, Connected, ModelInfo, StatusWatch, TokenStore, discover};
use crate::clock::Clock;
use crate::hotkeys::{HotkeyLimiter, Target, VtsMapping};
use crate::inject::{Batches, ChannelMap, Injector, Pacer, Snapshot};
use crate::limits::FadeTime;
use crate::protocol::{
    ApiError, AuthenticationRequest, AuthenticationResponse, AuthenticationTokenRequest, AuthenticationTokenResponse,
    CurrentModelRequest, CurrentModelResponse, Envelope, ErrorKind as VtsError, EventSubscriptionRequest,
    ExpressionActivationRequest, HotkeyTriggerRequest, HotkeysInCurrentModelRequest, HotkeysInCurrentModelResponse,
    MODEL_LOADED_EVENT, Message, ModelLoadedEvent, ParameterCreationRequest, StatisticsRequest, StatisticsResponse,
};

/// How long a request waits for its answer.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the popup may wait for the person.
const APPROVAL_TIMEOUT: Duration = Duration::from_secs(300);
/// How long the TCP connect and the WebSocket handshake may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// How long one socket read waits while connected: the loop's tick.
const READ_TICK: Duration = Duration::from_millis(1);
/// How long the idle loop waits for a command.
const IDLE_TICK: Duration = Duration::from_millis(5);
/// How often the model is asked for when VTS would not take the event subscription.
const MODEL_POLL: Duration = Duration::from_secs(2);

type Ws = WebSocket<TcpStream>;

/// How a session ended.
enum End {
    /// VTS could not be reached, or the connection dropped: retry on the backoff.
    Lost,
    /// No stored token works and no person asked: wait for a person's connect.
    NeedsPerson,
    /// The person revoked access.
    Revoked(Vec<String>),
    /// The person denied the popup.
    Denied,
    /// A person disconnected.
    Disconnected,
    /// Something else; a person's connect tries again.
    Failed(String),
    /// The bridge is stopping.
    Stopped,
}

/// What a request in flight was, to act on its answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pending {
    Inject,
    Hotkey,
    Hotkeys,
    Model,
    Other,
}

pub(super) struct Worker {
    config: ClientConfig,
    tokens: Arc<dyn TokenStore>,
    clock: Arc<dyn Clock>,
    rx: Receiver<Command>,
    watch: StatusWatch,
    injector: Arc<Injector>,
    creations: Vec<ParameterCreationRequest>,
    mapping: VtsMapping,
    attempt: u32,
    next_try: Option<Duration>,
    interactive: bool,
    next_id: u64,
    /// Commands that arrived while a request waited, handled after it.
    deferred: VecDeque<Command>,
}

/// One live connection's state.
struct Live {
    ws: Ws,
    status: Connected,
    pending: HashMap<String, Pending>,
    pacer: Pacer,
    batches: Batches,
    snap: Snapshot,
    fresh: bool,
    limiter: HotkeyLimiter,
    /// Expression files switched on, with their fades.
    expressions: Vec<(String, FadeTime)>,
    /// When to ask for the model again, when VTS would not take the event subscription.
    next_model_poll: Option<Duration>,
}

impl Worker {
    pub(super) fn new(
        config: ClientConfig,
        tokens: Arc<dyn TokenStore>,
        clock: Arc<dyn Clock>,
        rx: Receiver<Command>,
        watch: StatusWatch,
        injector: Arc<Injector>,
        map: &ChannelMap,
    ) -> Self {
        Self {
            config,
            tokens,
            clock,
            rx,
            watch,
            injector,
            creations: map.creations(),
            mapping: VtsMapping::default(),
            attempt: 0,
            next_try: None,
            interactive: false,
            next_id: 0,
            deferred: VecDeque::new(),
        }
    }

    pub(super) fn run(mut self) {
        if self.tokens.get(&self.config.instance).is_some() {
            self.next_try = Some(self.clock.now());
            self.watch.set(BridgeStatus::Connecting { attempt: 0 });
        }
        loop {
            self.watch.poll();
            let command = match self.deferred.pop_front() {
                Some(command) => Some(command),
                None => match self.rx.recv_timeout(IDLE_TICK) {
                    Ok(command) => Some(command),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
            };
            match command {
                Some(Command::Stop) => return,
                Some(Command::Connect) => {
                    (self.interactive, self.attempt) = (true, 0);
                    self.next_try = Some(self.clock.now());
                }
                Some(Command::Disconnect) => {
                    self.next_try = None;
                    self.watch.set(BridgeStatus::Disconnected);
                }
                Some(Command::Mapping(mapping)) => self.mapping = mapping,
                // Nothing to trigger into while disconnected.
                Some(Command::Trigger(_) | Command::Release(_)) | None => {}
            }
            if self.next_try.is_some_and(|at| self.clock.now() >= at) {
                self.next_try = None;
                let interactive = std::mem::take(&mut self.interactive);
                let end = self.session(interactive);
                if self.settle(end) {
                    return;
                }
            }
        }
    }

    /// Publishes how the session ended and plans the next try; true when the bridge is stopping.
    fn settle(&mut self, end: End) -> bool {
        let status = match end {
            End::Stopped => return true,
            End::Lost => {
                let wait = BACKOFF[usize::try_from(self.attempt).unwrap_or(usize::MAX).min(BACKOFF.len() - 1)];
                self.attempt = self.attempt.saturating_add(1);
                self.next_try = Some(self.clock.now() + wait);
                BridgeStatus::Connecting { attempt: self.attempt }
            }
            End::NeedsPerson | End::Disconnected => BridgeStatus::Disconnected,
            End::Revoked(expressions_may_be_on) => {
                self.tokens.delete(&self.config.instance);
                BridgeStatus::Revoked { expressions_may_be_on }
            }
            End::Denied => BridgeStatus::Denied,
            End::Failed(reason) => BridgeStatus::Failed { reason },
        };
        self.watch.set(status);
        false
    }

    fn session(&mut self, interactive: bool) -> End {
        self.watch.set(BridgeStatus::Connecting { attempt: self.attempt });
        let port = self.config.port.unwrap_or_else(|| discover(&self.config.instance));
        let Some(mut ws) = open(&self.config.host, port) else { return End::Lost };
        if let Err(end) = self.authenticate(&mut ws, interactive) {
            close(ws);
            return end;
        }
        let live = match self.set_up(ws) {
            Ok(live) => live,
            Err(end) => return end,
        };
        self.attempt = 0;
        self.connected(live)
    }

    fn authenticate(&mut self, ws: &mut Ws, interactive: bool) -> Result<(), End> {
        let instance = self.config.instance.clone();
        if let Some(token) = self.tokens.get(&instance) {
            let answer: AuthenticationResponse =
                self.call(ws, AuthenticationRequest::omuzaki(token), REQUEST_TIMEOUT)?.map_err(|e| fail(&e))?;
            if answer.authenticated {
                return Ok(());
            }
            // Revoked in VTS, or never valid: never used again.
            self.tokens.delete(&instance);
            if !interactive {
                return Err(End::Revoked(Vec::new()));
            }
        } else if !interactive {
            return Err(End::NeedsPerson);
        }
        self.watch.set(BridgeStatus::AwaitingApproval);
        let token: AuthenticationTokenResponse =
            match self.call(ws, AuthenticationTokenRequest::omuzaki(), APPROVAL_TIMEOUT)? {
                Ok(token) => token,
                Err(e) if e.kind() == VtsError::NotAuthenticated => return Err(End::Denied),
                Err(e) => return Err(fail(&e)),
            };
        self.tokens.put(&instance, &token.authentication_token);
        let answer: AuthenticationResponse = self
            .call(ws, AuthenticationRequest::omuzaki(token.authentication_token), REQUEST_TIMEOUT)?
            .map_err(|e| fail(&e))?;
        if answer.authenticated {
            Ok(())
        } else {
            Err(End::Failed(format!("VTube Studio refused the new token: {}", answer.reason)))
        }
    }

    fn set_up(&mut self, mut ws: Ws) -> Result<Live, End> {
        let step = |worker: &mut Self, ws: &mut Ws| -> Result<_, End> {
            let stats: StatisticsResponse =
                worker.call(ws, StatisticsRequest {}, REQUEST_TIMEOUT)?.map_err(|e| fail(&e))?;
            let model: CurrentModelResponse =
                worker.call(ws, CurrentModelRequest {}, REQUEST_TIMEOUT)?.map_err(|e| fail(&e))?;
            // VTS without event support answers with an error; the bridge then asks for the model every 2 s.
            let subscribed =
                worker.call::<_, Value>(ws, EventSubscriptionRequest::model_loaded(true), REQUEST_TIMEOUT)?.is_ok();
            let hotkeys: HotkeysInCurrentModelResponse =
                worker.call(ws, HotkeysInCurrentModelRequest::default(), REQUEST_TIMEOUT)?.map_err(|e| fail(&e))?;
            for creation in worker.creations.clone() {
                let created: Result<Value, ApiError> = worker.call(ws, creation, REQUEST_TIMEOUT)?;
                created.map_err(|e| fail(&e))?;
            }
            Ok((stats, model, subscribed, hotkeys))
        };
        let (stats, model, subscribed, hotkeys) = match step(self, &mut ws) {
            Ok(done) => done,
            Err(end) => {
                close(ws);
                return Err(end);
            }
        };
        let limiter = HotkeyLimiter::new(stats.framerate);
        let connected = Connected {
            vts_version: stats.v_tube_studio_version,
            model: model_info(model.model_loaded, model.model_id, model.model_name),
            fps: stats.framerate,
            conflicts: Vec::new(),
            hotkeys: hotkeys.available_hotkeys,
            hotkeys_dropped: 0,
            expressions_on: Vec::new(),
        };
        self.watch.set(BridgeStatus::Connected(connected.clone()));
        Ok(Live {
            ws,
            status: connected,
            pending: HashMap::new(),
            pacer: Pacer::default(),
            batches: Batches::default(),
            snap: Snapshot::default(),
            fresh: false,
            limiter,
            expressions: Vec::new(),
            next_model_poll: (!subscribed).then(|| self.clock.now() + MODEL_POLL),
        })
    }

    fn connected(&mut self, mut live: Live) -> End {
        let mut due = Vec::with_capacity(crate::limits::HOTKEY_QUEUE);
        loop {
            self.watch.poll();
            // Answers and events first, so a revoke stops the next send.
            loop {
                match read(&mut live.ws) {
                    Ok(Some(text)) => {
                        if let Some(end) = self.incoming(&mut live, &text) {
                            close(live.ws);
                            return end;
                        }
                    }
                    Ok(None) => break,
                    Err(()) => return End::Lost,
                }
            }
            while let Some(command) = self.deferred.pop_front().or_else(|| self.rx.try_recv().ok()) {
                match command {
                    Command::Stop => {
                        self.switch_off(&mut live);
                        close(live.ws);
                        return End::Stopped;
                    }
                    Command::Disconnect => {
                        self.switch_off(&mut live);
                        close(live.ws);
                        return End::Disconnected;
                    }
                    Command::Connect => {}
                    Command::Mapping(mapping) => self.mapping = mapping,
                    Command::Trigger(id) => self.trigger(&mut live, &id),
                    Command::Release(id) => self.release(&mut live, &id),
                }
            }
            if matches!(self.rx.try_recv(), Err(TryRecvError::Disconnected)) && self.deferred.is_empty() {
                self.switch_off(&mut live);
                close(live.ws);
                return End::Stopped;
            }
            let now = self.clock.now();
            live.fresh |= self.injector.read(&mut live.snap);
            if live.pacer.due(now, live.fresh).is_some() {
                for request in live.batches.requests(self.injector.params(), &live.snap) {
                    if !self.send(&mut live, request, Pending::Inject) {
                        return End::Lost;
                    }
                }
                live.pacer.sent(now);
                live.fresh = false;
            }
            live.limiter.due(now, &mut due);
            for hotkey in due.drain(..) {
                let request = HotkeyTriggerRequest { hotkey_id: hotkey, item_instance_id: None };
                if !self.send(&mut live, request, Pending::Hotkey) {
                    return End::Lost;
                }
            }
            if live.next_model_poll.is_some_and(|at| now >= at) {
                live.next_model_poll = Some(now + MODEL_POLL);
                if !self.send(&mut live, CurrentModelRequest {}, Pending::Model) {
                    return End::Lost;
                }
            }
            self.publish(&mut live);
        }
    }

    /// Acts on one message from VTS; `Some` ends the session.
    fn incoming(&mut self, live: &mut Live, text: &str) -> Option<End> {
        let Ok(envelope) = serde_json::from_str::<Envelope<Value>>(text) else { return None };
        let pending = live.pending.remove(&envelope.request_id).unwrap_or(Pending::Other);
        if envelope.is_error() {
            let error: ApiError = serde_json::from_value(envelope.data).ok()?;
            match (error.kind(), pending) {
                (VtsError::NotAuthenticated, _) => {
                    // Clean degrade: nothing more is sent; what was switched on can't be switched off now.
                    let on = live.expressions.iter().map(|(file, _)| file.clone()).collect();
                    return Some(End::Revoked(on));
                }
                (VtsError::ParamOwnedByOtherPlugin, Pending::Inject)
                    if live.batches.owned(self.injector.params(), &error.message) =>
                {
                    // Resend at once in the new modes, so the parameter never lapses.
                    live.pacer.reset();
                    live.fresh = true;
                }
                _ => {}
            }
            return None;
        }
        if envelope.message_type == MODEL_LOADED_EVENT {
            let event: ModelLoadedEvent = serde_json::from_value(envelope.data).ok()?;
            self.model_changed(live, model_info(event.model_loaded, event.model_id, event.model_name));
            return None;
        }
        match pending {
            Pending::Hotkeys => {
                if let Ok(hotkeys) = serde_json::from_value::<HotkeysInCurrentModelResponse>(envelope.data) {
                    live.status.hotkeys = hotkeys.available_hotkeys;
                }
            }
            Pending::Model => {
                if let Ok(model) = serde_json::from_value::<CurrentModelResponse>(envelope.data) {
                    let info = model_info(model.model_loaded, model.model_id, model.model_name);
                    if info != live.status.model {
                        self.model_changed(live, info);
                    }
                }
            }
            Pending::Inject | Pending::Hotkey | Pending::Other => {}
        }
        None
    }

    fn model_changed(&mut self, live: &mut Live, model: Option<ModelInfo>) {
        // The old model's expressions went with it; its hotkeys too.
        live.expressions.clear();
        live.limiter.clear();
        live.status.model = model;
        live.status.hotkeys.clear();
        if live.status.model.is_some() {
            // Errors surface on the next loop as a lost connection.
            self.send(live, HotkeysInCurrentModelRequest::default(), Pending::Hotkeys);
        }
    }

    fn trigger(&mut self, live: &mut Live, omuzaki: &str) {
        let Some(model) = live.status.model.as_ref() else { return };
        match self.mapping.resolve(&model.id, omuzaki).cloned() {
            Some(Target::Hotkey { id, .. }) => live.limiter.request(id),
            Some(Target::Expression { file, fade_s }) => {
                let fade = FadeTime::new(fade_s);
                let request =
                    ExpressionActivationRequest { expression_file: file.clone(), fade_time: fade, active: true };
                if self.send(live, request, Pending::Other) && !live.expressions.iter().any(|(f, _)| *f == file) {
                    live.expressions.push((file, fade));
                }
            }
            None => {}
        }
    }

    fn release(&mut self, live: &mut Live, omuzaki: &str) {
        let Some(model) = live.status.model.as_ref() else { return };
        let Some(Target::Expression { file, .. }) = self.mapping.resolve(&model.id, omuzaki).cloned() else { return };
        if let Some(at) = live.expressions.iter().position(|(f, _)| *f == file) {
            let (file, fade) = live.expressions.remove(at);
            let request = ExpressionActivationRequest { expression_file: file, fade_time: fade, active: false };
            self.send(live, request, Pending::Other);
        }
    }

    /// Switches off every expression the bridge switched on, waiting for each answer so the last one lands before
    /// the socket closes.
    fn switch_off(&mut self, live: &mut Live) {
        for (file, fade) in std::mem::take(&mut live.expressions) {
            let request = ExpressionActivationRequest { expression_file: file, fade_time: fade, active: false };
            // A refusal or a dropped connection leaves nothing more to do.
            drop(self.call::<_, Value>(&mut live.ws, request, REQUEST_TIMEOUT));
        }
    }

    fn publish(&self, live: &mut Live) {
        live.status.conflicts = live.batches.conflicts(self.injector.params());
        live.status.hotkeys_dropped = live.limiter.dropped();
        live.status.expressions_on = live.expressions.iter().map(|(f, _)| f.clone()).collect();
        self.watch.set(BridgeStatus::Connected(live.status.clone()));
    }

    fn request_id(&mut self) -> String {
        self.next_id += 1;
        format!("omz-{}", self.next_id)
    }

    /// Sends `data` without waiting; its answer is matched by id in [`incoming`](Self::incoming). False when the
    /// connection is gone.
    fn send<T: Message + Serialize>(&mut self, live: &mut Live, data: T, pending: Pending) -> bool {
        let id = self.request_id();
        let Ok(text) = serde_json::to_string(&Envelope::new(id.clone(), data)) else { return true };
        if live.ws.send(WsMessage::text(text)).is_err() {
            return false;
        }
        self.watch.sent_one();
        live.pending.insert(id, pending);
        true
    }

    /// Sends `data` and waits up to `timeout` for its answer: `Ok(Err)` is VTS's refusal, `Err` ends the session
    /// (a dropped connection, a timeout, or a person's stop or disconnect while waiting).
    fn call<T: Message + Serialize, R: DeserializeOwned>(
        &mut self,
        ws: &mut Ws,
        data: T,
        timeout: Duration,
    ) -> Result<Result<R, ApiError>, End> {
        let id = self.request_id();
        let text = serde_json::to_string(&Envelope::new(id.clone(), data)).map_err(|e| End::Failed(e.to_string()))?;
        ws.send(WsMessage::text(text)).map_err(|_| End::Lost)?;
        self.watch.sent_one();
        let deadline = Instant::now() + timeout;
        loop {
            match self.rx.try_recv() {
                Ok(Command::Stop) | Err(TryRecvError::Disconnected) => return Err(End::Stopped),
                Ok(Command::Disconnect) => return Err(End::Disconnected),
                Ok(other) => self.deferred.push_back(other),
                Err(TryRecvError::Empty) => {}
            }
            match read(ws) {
                Ok(Some(text)) => {
                    let Ok(envelope) = serde_json::from_str::<Envelope<Value>>(&text) else { continue };
                    if envelope.request_id != id {
                        continue;
                    }
                    if envelope.is_error() {
                        let error = serde_json::from_value(envelope.data).map_err(|e| End::Failed(e.to_string()))?;
                        return Ok(Err(error));
                    }
                    return serde_json::from_value(envelope.data).map(Ok).map_err(|e| End::Failed(e.to_string()));
                }
                Ok(None) if Instant::now() >= deadline => {
                    return Err(End::Failed(format!("VTube Studio did not answer within {} s", timeout.as_secs())));
                }
                Ok(None) => {}
                Err(()) => return Err(End::Lost),
            }
        }
    }
}

fn fail(error: &ApiError) -> End {
    End::Failed(error.to_string())
}

fn model_info(loaded: bool, id: String, name: String) -> Option<ModelInfo> {
    loaded.then_some(ModelInfo { id, name })
}

/// Opens the WebSocket to VTS on this computer.
fn open(host: &str, port: u16) -> Option<Ws> {
    let addr: SocketAddr =
        (host.trim_matches(['[', ']']), port).to_socket_addrs().ok()?.find(|a| a.ip().is_loopback())?;
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).ok()?;
    stream.set_nodelay(true).ok()?;
    stream.set_read_timeout(Some(CONNECT_TIMEOUT)).ok()?;
    let url = format!("ws://{addr}");
    let (ws, _) = tungstenite::client(url.as_str(), stream).ok()?;
    ws.get_ref().set_read_timeout(Some(READ_TICK)).ok()?;
    Some(ws)
}

/// One text message if one arrives within the read tick; `Err` when the connection is gone.
fn read(ws: &mut Ws) -> Result<Option<String>, ()> {
    match ws.read() {
        Ok(WsMessage::Text(text)) => Ok(Some(text.as_str().to_owned())),
        Ok(WsMessage::Close(_)) => Err(()),
        Ok(_) => Ok(None),
        Err(WsError::Io(e)) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => Ok(None),
        Err(_) => Err(()),
    }
}

fn close(mut ws: Ws) {
    // Best effort: the socket closes when it drops either way.
    drop(ws.close(None));
    drop(ws.flush());
}
