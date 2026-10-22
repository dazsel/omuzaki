//! The adapter's connection thread: validate the token, open the socket, subscribe to every topic the token allows
//! within the ten seconds Twitch gives, then turn each notification into an automation event and publish it.
//!
//! - **Reconnects.** `session_reconnect` moves to the new URL and keeps the subscriptions; the old connection keeps
//!   delivering until the new one says welcome. A dropped connection, a close or a missed keepalive starts over at
//!   the base URL with fresh subscriptions, after a backoff (1 s doubling to 30 s, reset by a healthy session).
//! - **The token is checked** when the adapter starts, before each fresh start, hourly while connected (as Twitch
//!   asks) and at once when Twitch revokes an authorisation. A token Twitch refuses stops the adapter with an error
//!   saying to store a new one; a check that can't reach Twitch is tried again.
//! - **Nothing is lost to a slow show.** A redeem, a sub or a raid the inbox can't take now (`Full`) is kept in a
//!   buffer of [`PRODUCER_BUFFER`] and retried on a backoff from 100 ms to 1 s, in order (design D9); past the
//!   buffer the oldest is dropped and counted with `producer_dropped`. Chat, which the inbox may shed, is not kept.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_core::time::{Clock, Seconds};
use omuzaki_live_api::event::{EventSink, LiveEvent, PublishError, find_event_kind};
use omuzaki_live_api::limits::{PRODUCER_BUFFER, PRODUCER_RETRY_MAX_SECONDS, PRODUCER_RETRY_SECONDS};
use omuzaki_secrets_api::value::SecretValue;
use schemars::JsonSchema;
use serde::Serialize;
use tungstenite::{Message, WebSocket};

use crate::error::TwitchError;
use crate::eventsub::{Frame, TOPICS, Topic, normalise, parse};
use crate::helix::{Helix, TokenInfo};
use crate::socket::{self, Stream, timed_out};

/// How long Twitch gives a new session to say welcome.
const WELCOME_WAIT: Duration = Duration::from_secs(10);
/// Slack on top of the keepalive time before a quiet connection counts as dead.
const KEEPALIVE_GRACE: Duration = Duration::from_secs(3);
/// The first wait before reconnecting, doubled up to [`MAX_BACKOFF`].
const FIRST_BACKOFF: Duration = Duration::from_secs(1);
/// The longest wait before reconnecting.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// How often the token is checked again while connected: Twitch asks apps to validate their tokens hourly, and an
/// expired or revoked token then stops the adapter with a clear error.
const REVALIDATE: Duration = Duration::from_secs(3600);
/// How soon a check that couldn't reach Twitch is tried again.
const RECHECK: Duration = Duration::from_secs(60);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A subscription the token doesn't allow, and the scopes that would.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Skipped {
    /// The subscription type.
    pub kind: String,
    /// The automation event it would give.
    pub event: String,
    /// Any one of these scopes allows it.
    pub needs: Vec<String>,
}

/// What `twitch.status` reports. Never the token.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct TwitchStatus {
    /// Whether a session is open now.
    pub connected: bool,
    /// The account the token belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login: Option<String>,
    /// The automation events the open subscriptions give.
    pub events: Vec<String>,
    /// The subscriptions the token's scopes don't allow.
    pub skipped: Vec<Skipped>,
    /// Subscriptions Twitch refused or later revoked, with its reason.
    pub refused: Vec<String>,
    /// Events received and published.
    pub received: u64,
    /// Non-droppable events dropped because the show couldn't take them in time.
    pub lost: u64,
    /// How many times the connection was opened again.
    pub reconnects: u64,
    /// The last problem, if the connection isn't healthy.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where the adapter connects. Twitch's own addresses in the app; a test server's in tests.
#[derive(Clone, Debug)]
pub struct Endpoints {
    /// The `EventSub` WebSocket.
    pub socket: String,
    /// Helix and the OAuth service.
    pub helix: Helix,
}

/// A running adapter: its thread and what it reports.
pub(crate) struct Adapter {
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<TwitchStatus>>,
    thread: Option<JoinHandle<()>>,
}

impl Adapter {
    /// Starts the connection thread. The token was read from the secret store by the caller and lives only here.
    pub(crate) fn start(
        endpoints: Endpoints,
        token: SecretValue,
        sink: Arc<dyn EventSink>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, TwitchError> {
        let stop = Arc::new(AtomicBool::new(false));
        let status = Arc::new(Mutex::new(TwitchStatus::default()));
        let worker = Worker {
            endpoints,
            token,
            sink,
            clock,
            stop: Arc::clone(&stop),
            status: Arc::clone(&status),
            pending: VecDeque::new(),
            retry_at: None,
            retry_wait: Duration::from_secs_f64(PRODUCER_RETRY_SECONDS),
            next_check: Instant::now(),
        };
        let thread = std::thread::Builder::new()
            .name("twitch-eventsub".to_owned())
            .spawn(move || worker.run())
            .map_err(|e| TwitchError::Network(format!("the connection thread didn't start: {e}")))?;
        Ok(Self { stop, status, thread: Some(thread) })
    }

    /// What it reports now.
    pub(crate) fn status(&self) -> TwitchStatus {
        lock(&self.status).clone()
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// How a session ended.
enum Ended {
    /// Move to this URL; the subscriptions carry over. The old connection delivers until the new one is open.
    Moved(String, Box<WebSocket<Stream>>),
    /// Start over at the base URL.
    Dropped(String),
    /// The token no longer works.
    Refused(String),
    /// Asked to stop.
    Stopped,
}

struct Worker {
    endpoints: Endpoints,
    token: SecretValue,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
    stop: Arc<AtomicBool>,
    status: Arc<Mutex<TwitchStatus>>,
    pending: VecDeque<LiveEvent>,
    retry_at: Option<Instant>,
    retry_wait: Duration,
    /// When the token is checked next.
    next_check: Instant,
}

impl Worker {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn report(&self, change: impl FnOnce(&mut TwitchStatus)) {
        change(&mut lock(&self.status));
    }

    /// Waits up to `wait`, waking early to stop.
    fn pause(&self, wait: Duration) {
        let until = Instant::now() + wait;
        while !self.stopping() && Instant::now() < until {
            std::thread::sleep(socket::READ_TICK.min(until.saturating_duration_since(Instant::now())));
        }
    }

    /// Checks the token with Twitch, and books the next check.
    fn check(&mut self) -> Result<TokenInfo, TwitchError> {
        let info = self.endpoints.helix.validate(self.token.expose())?;
        let expires = (info.expires_in > 0).then(|| Duration::from_secs(info.expires_in.saturating_add(1)));
        self.next_check = Instant::now() + expires.map_or(REVALIDATE, |e| e.min(REVALIDATE));
        Ok(info)
    }

    /// Stops for good: the token no longer works, and only a person with a new one can fix that.
    fn refused(&self, why: &str) {
        self.report(|s| {
            s.connected = false;
            s.error = Some(format!("{why}; store a new token and run twitch.connect"));
        });
    }

    /// Takes in what a (new) token check says: the account and the subscriptions its scopes allow.
    fn accept(&self, info: &TokenInfo) -> Vec<&'static Topic> {
        let (allowed, skipped): (Vec<&Topic>, Vec<&Topic>) = TOPICS.iter().partition(|t| t.allowed(&info.scopes));
        self.report(|s| {
            s.login = Some(info.login.clone());
            s.skipped = skipped
                .iter()
                .map(|t| Skipped {
                    kind: t.kind.to_owned(),
                    event: t.event.to_owned(),
                    needs: t.scopes.iter().map(|s| (*s).to_owned()).collect(),
                })
                .collect();
        });
        allowed
    }

    fn run(mut self) {
        let mut backoff = FIRST_BACKOFF;
        // A token Twitch refuses ends the adapter; anything else (no network yet, Twitch down) is tried again.
        let mut info = loop {
            match self.check() {
                Ok(info) => break info,
                Err(TwitchError::Token(why)) => return self.refused(&why),
                Err(e) => {
                    self.report(|s| s.error = Some(e.to_string()));
                    self.pause(backoff);
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    if self.stopping() {
                        return;
                    }
                }
            }
        };
        let mut allowed = self.accept(&info);
        backoff = FIRST_BACKOFF;
        let mut url = self.endpoints.socket.clone();
        let mut old = None;
        let mut opened = false;
        while !self.stopping() {
            if opened {
                self.report(|s| s.reconnects += 1);
            }
            opened = true;
            let healthy_since = Instant::now();
            let subscribe = old.is_none();
            match self.session(&url, subscribe, &info, &allowed, old.take()) {
                Ended::Stopped => break,
                Ended::Refused(why) => return self.refused(&why),
                Ended::Moved(next, socket) => {
                    url = next;
                    old = Some(socket);
                }
                Ended::Dropped(why) => {
                    self.report(|s| {
                        s.connected = false;
                        s.error = Some(why);
                    });
                    if healthy_since.elapsed() > MAX_BACKOFF {
                        backoff = FIRST_BACKOFF;
                    }
                    self.pause(backoff);
                    backoff = (backoff * 2).min(MAX_BACKOFF);
                    url = self.endpoints.socket.clone();
                    // Starting over makes fresh subscriptions: check the token first, so an expired one stops here
                    // instead of being refused topic by topic on every reconnect.
                    match self.check() {
                        Ok(fresh) => {
                            allowed = self.accept(&fresh);
                            info = fresh;
                        }
                        Err(TwitchError::Token(why)) => return self.refused(&why),
                        Err(_) => {}
                    }
                }
            }
        }
        self.report(|s| s.connected = false);
    }

    /// One connection, from the handshake to its end. `old` is the connection Twitch asked to move from: it keeps
    /// delivering until the new one says welcome, then closes (the inbox drops an event both deliver).
    fn session(
        &mut self,
        url: &str,
        subscribe: bool,
        info: &TokenInfo,
        topics: &[&Topic],
        mut old: Option<Box<WebSocket<Stream>>>,
    ) -> Ended {
        let mut socket = match socket::connect(url) {
            Ok(socket) => socket,
            Err(e) => return Ended::Dropped(e.to_string()),
        };
        let deadline = Instant::now() + WELCOME_WAIT;
        let (session, keepalive) = loop {
            if self.stopping() {
                return Ended::Stopped;
            }
            if Instant::now() > deadline {
                return Ended::Dropped("Twitch didn't open the session in time".to_owned());
            }
            if let Some(previous) = old.as_mut() {
                match read(previous) {
                    // A second move before the first lands has nowhere to go but the address already being opened.
                    Ok(Some(frame)) => drop(self.take(frame)),
                    Ok(None) => {}
                    Err(_) => old = None,
                }
            }
            match read(&mut socket) {
                Ok(Some(Frame::Welcome { session, keepalive })) => break (session, keepalive),
                Ok(Some(frame)) => drop(self.take(frame)),
                Ok(None) => {}
                Err(why) => return Ended::Dropped(why),
            }
        };
        if let Some(mut previous) = old.take() {
            let _ = previous.close(None);
            let _ = previous.flush();
        }
        if subscribe {
            let mut events = Vec::new();
            let mut refused = Vec::new();
            for topic in topics {
                match self.endpoints.helix.subscribe(self.token.expose(), info, topic, &session) {
                    Ok(()) => events.push(topic.event.to_owned()),
                    Err(e) => refused.push(e.to_string()),
                }
            }
            self.report(|s| {
                s.events = events;
                s.refused = refused;
            });
        }
        self.report(|s| {
            s.connected = true;
            s.error = None;
        });
        let quiet = Duration::from_secs(keepalive) + KEEPALIVE_GRACE;
        let mut heard = Instant::now();
        loop {
            self.flush();
            if self.stopping() {
                let _ = socket.close(None);
                return Ended::Stopped;
            }
            if Instant::now() >= self.next_check {
                match self.check() {
                    Ok(_) => {}
                    Err(TwitchError::Token(why)) => {
                        let _ = socket.close(None);
                        return Ended::Refused(why);
                    }
                    Err(_) => self.next_check = Instant::now() + RECHECK,
                }
            }
            if heard.elapsed() > quiet {
                return Ended::Dropped("Twitch went quiet past the keepalive time".to_owned());
            }
            let frame = match read(&mut socket) {
                Ok(Some(frame)) => frame,
                Ok(None) => continue,
                Err(why) => return Ended::Dropped(why),
            };
            heard = Instant::now();
            if let Some(next) = self.take(frame) {
                return Ended::Moved(next, Box::new(socket));
            }
        }
    }

    /// Acts on one message; the address to move to when Twitch asks for a move.
    fn take(&mut self, frame: Frame) -> Option<String> {
        match frame {
            Frame::Notification { message_id, kind, event } => {
                if let Some(live) = normalise(&kind, &event, &message_id, Seconds(self.clock.now().0)) {
                    self.report(|s| s.received += 1);
                    self.publish(live);
                }
                None
            }
            Frame::Reconnect { url } if !url.is_empty() => Some(url),
            Frame::Revocation { kind, status } => {
                // A revoked authorisation usually means the token is gone: check it now rather than in an hour.
                if status == "authorization_revoked" {
                    self.next_check = Instant::now();
                }
                let event = TOPICS.iter().find(|t| t.kind == kind).map(|t| t.event);
                self.report(|s| {
                    s.events.retain(|e| Some(e.as_str()) != event);
                    s.refused.push(format!("Twitch ended the {kind} subscription: {status}"));
                });
                None
            }
            Frame::Welcome { .. } | Frame::Keepalive | Frame::Reconnect { .. } | Frame::Other(_) => None,
        }
    }

    /// Publishes an event, keeping a non-droppable one the inbox can't take now.
    fn publish(&mut self, event: LiveEvent) {
        if !self.pending.is_empty() {
            return self.keep(event);
        }
        // Anything but `Full` is final: accepted, or refused for a payload no retry fixes.
        if let Err(PublishError::Full) = self.sink.publish(event.clone()) {
            self.keep(event);
        }
    }

    fn keep(&mut self, event: LiveEvent) {
        if find_event_kind(event.kind.as_str()).is_some_and(|k| k.droppable) {
            return;
        }
        if self.pending.len() >= PRODUCER_BUFFER {
            self.pending.pop_front();
            self.sink.producer_dropped(1);
            self.report(|s| s.lost += 1);
        }
        self.pending.push_back(event);
        self.retry_at.get_or_insert_with(|| Instant::now() + self.retry_wait);
    }

    /// Retries the kept events in order, when their backoff is up.
    fn flush(&mut self) {
        if self.retry_at.is_none_or(|at| Instant::now() < at) {
            return;
        }
        while let Some(event) = self.pending.front() {
            match self.sink.publish(event.clone()) {
                Err(PublishError::Full) => {
                    self.retry_wait = (self.retry_wait * 2).min(Duration::from_secs_f64(PRODUCER_RETRY_MAX_SECONDS));
                    self.retry_at = Some(Instant::now() + self.retry_wait);
                    return;
                }
                Ok(()) | Err(_) => {
                    self.pending.pop_front();
                }
            }
        }
        self.retry_at = None;
        self.retry_wait = Duration::from_secs_f64(PRODUCER_RETRY_SECONDS);
    }
}

/// The next message, `None` when the read timed out; `Err` with a reason when the connection ended.
fn read(socket: &mut WebSocket<Stream>) -> Result<Option<Frame>, String> {
    match socket.read() {
        Ok(Message::Text(text)) => {
            parse(text.as_str()).map(Some).map_err(|e| format!("Twitch sent something unreadable: {e}"))
        }
        Ok(Message::Close(frame)) => Err(match frame {
            Some(f) => format!("Twitch closed the connection ({}: {})", u16::from(f.code), f.reason),
            None => "Twitch closed the connection".to_owned(),
        }),
        Ok(_) => Ok(None),
        Err(e) if timed_out(&e) => Ok(None),
        Err(e) => Err(format!("the connection to Twitch broke: {e}")),
    }
}
