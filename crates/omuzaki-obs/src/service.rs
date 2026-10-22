//! [`ObsService`]: the app's one connection to OBS, shared as a service so the ops, the UI and automations use the same
//! link. It keeps the connection up: when OBS goes away (closed, restarted, the network dropped), it reconnects after
//! 1, 2, 4, 8 and 16 s, then every 30 s, until it's back or a person disconnects. A refused password is never retried
//! (retrying burns OBS's attention and the person's patience), and neither is a session a person ended from OBS's
//! WebSocket settings (`SessionInvalidated`): only `obs.connect` again does that.
//!
//! The password is held only by the reconnecting thread, as a [`SecretValue`], for its handshakes; it never reaches a
//! status, an error or a log.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_secrets_api::value::SecretValue;
use schemars::JsonSchema;
use serde::Serialize;

use crate::client::{ClientConfig, EventHub, EventStream, ObsClient, ObsError};
use crate::limits::{CloseCode, DEFAULT_PORT};
use crate::protocol::RequestType;

/// Where OBS listens by default: this machine.
pub const DEFAULT_HOST: &str = "127.0.0.1";
/// The waits before each reconnect, in backoff units; the last repeats.
pub const BACKOFF_STEPS: [u32; 6] = [1, 2, 4, 8, 16, 30];
/// The most reconnect waits [`ObsService::retries`] keeps.
const RETRY_LOG: usize = 64;
/// How often the reconnecting thread looks at its connection.
const WATCH: Duration = Duration::from_millis(20);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The wait before reconnect attempt `attempt` (from 0), in units: 1, 2, 4, 8, 16, then 30.
#[must_use]
pub fn backoff(attempt: usize) -> u32 {
    BACKOFF_STEPS[attempt.min(BACKOFF_STEPS.len() - 1)]
}

/// The connection's state, as `obs.status` and the UI show it. Never the password.
#[derive(Clone, Debug, Default, PartialEq, Serialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ObsStatus {
    /// No connection, and none wanted.
    #[default]
    Disconnected,
    /// Opening a connection.
    Connecting,
    /// Connected.
    Connected {
        /// The server plugin's version.
        websocket_version: String,
        /// The negotiated RPC version.
        rpc_version: u32,
        /// Where.
        host: String,
        /// The port.
        port: u16,
        /// Whether the link crosses the network unencrypted (a remote OBS).
        unencrypted: bool,
    },
    /// OBS went away; the next attempt comes after a wait.
    Retrying {
        /// Which attempt is next, from 1.
        attempt: usize,
        /// The wait before it, in seconds.
        wait_s: f64,
        /// Why the connection was lost.
        reason: String,
    },
    /// OBS refused the password (or none is stored); not retried.
    AuthFailed {
        /// Why.
        reason: String,
    },
    /// Connecting failed for a reason no wait fixes.
    Failed {
        /// Why.
        reason: String,
    },
}

/// The reconnecting thread of one wanted connection.
struct Session {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What the service and its reconnecting thread share.
#[derive(Default)]
struct Shared {
    client: Mutex<Option<Arc<ObsClient>>>,
    status: Mutex<ObsStatus>,
    retries: Mutex<Vec<Duration>>,
}

/// The connection to OBS, if any. Cheap to share: the app holds it in an `Arc`.
pub struct ObsService {
    shared: Arc<Shared>,
    hub: Arc<EventHub>,
    session: Mutex<Option<Session>>,
    unit: Duration,
    /// Held through a whole connect or disconnect, so two at once (a panel and an agent) can't leave the reconnecting
    /// thread of one watching the client of the other.
    changing: Mutex<()>,
}

impl std::fmt::Debug for ObsService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObsService").field("status", &self.status()).finish_non_exhaustive()
    }
}

impl Default for ObsService {
    fn default() -> Self {
        Self::new()
    }
}

impl ObsService {
    /// A service with no connection, reconnecting on the 1-second backoff unit.
    #[must_use]
    pub fn new() -> Self {
        Self::with_backoff_unit(Duration::from_secs(1))
    }

    /// A service whose reconnect waits are [`BACKOFF_STEPS`] times `unit` (tests use milliseconds).
    #[must_use]
    pub fn with_backoff_unit(unit: Duration) -> Self {
        Self {
            shared: Arc::default(),
            hub: Arc::new(EventHub::new()),
            session: Mutex::new(None),
            unit,
            changing: Mutex::new(()),
        }
    }

    /// Connects as `config` says, replacing any connection there was, and keeps the connection up from then on.
    ///
    /// # Errors
    /// As [`ObsClient::open`] for the first attempt; nothing is kept when it fails.
    pub fn connect(&self, config: ClientConfig, password: Option<SecretValue>) -> Result<Arc<ObsClient>, ObsError> {
        let _changing = lock(&self.changing);
        self.close();
        *lock(&self.shared.status) = ObsStatus::Connecting;
        let opened = ObsClient::open(&config, password.as_ref().map(SecretValue::expose), Arc::clone(&self.hub));
        let client = match opened {
            Ok(client) => Arc::new(client),
            Err(e) => {
                *lock(&self.shared.status) = failed(&e);
                return Err(e);
            }
        };
        *lock(&self.shared.status) = connected(&config, &client);
        *lock(&self.shared.client) = Some(Arc::clone(&client));
        lock(&self.shared.retries).clear();
        let stop = Arc::new(AtomicBool::new(false));
        let keeper = Keeper {
            config,
            password,
            shared: Arc::clone(&self.shared),
            hub: Arc::clone(&self.hub),
            stop: Arc::clone(&stop),
            unit: self.unit,
        };
        let thread = std::thread::Builder::new()
            .name("omuzaki-obs-reconnect".into())
            .spawn(move || keeper.run())
            .map_err(|e| ObsError::Closed(Some(format!("the reconnecting thread could not start: {e}"))))?;
        *lock(&self.session) = Some(Session { stop, thread: Some(thread) });
        Ok(client)
    }

    /// Closes the connection and stops reconnecting; `false` when there was none.
    pub fn disconnect(&self) -> bool {
        let _changing = lock(&self.changing);
        self.close()
    }

    fn close(&self) -> bool {
        let session = lock(&self.session).take();
        let had = session.is_some();
        drop(session);
        let client = lock(&self.shared.client).take();
        *lock(&self.shared.status) = ObsStatus::Disconnected;
        had || client.is_some()
    }

    /// The connection's state now.
    #[must_use]
    pub fn status(&self) -> ObsStatus {
        lock(&self.shared.status).clone()
    }

    /// The reconnect waits so far, oldest first (at most 64), for the status page and tests.
    #[must_use]
    pub fn retries(&self) -> Vec<Duration> {
        lock(&self.shared.retries).clone()
    }

    /// The open connection, if it is still connected.
    #[must_use]
    pub fn client(&self) -> Option<Arc<ObsClient>> {
        lock(&self.shared.client).as_ref().filter(|c| c.is_connected()).cloned()
    }

    /// Sends `request` on the open connection.
    ///
    /// # Errors
    /// [`ObsError::Closed`] when there is no open connection; otherwise as [`ObsClient::request`].
    pub fn request<R: RequestType>(&self, request: &R) -> Result<R::Response, ObsError> {
        let client = self.client().ok_or_else(|| ObsError::Closed(self.not_connected()))?;
        client.request(request)
    }

    /// Events from every connection this service makes, from now on.
    #[must_use]
    pub fn subscribe(&self) -> EventStream {
        self.hub.subscribe()
    }

    /// Why there's no connection, for an error.
    fn not_connected(&self) -> Option<String> {
        match self.status() {
            ObsStatus::Retrying { reason, .. } | ObsStatus::AuthFailed { reason } | ObsStatus::Failed { reason } => {
                Some(reason)
            }
            ObsStatus::Connecting => Some("still connecting".into()),
            _ => None,
        }
    }
}

fn connected(config: &ClientConfig, client: &ObsClient) -> ObsStatus {
    ObsStatus::Connected {
        websocket_version: client.info().websocket_version.clone(),
        rpc_version: client.info().rpc_version,
        host: config.host.clone(),
        port: config.port,
        unencrypted: client.unencrypted(),
    }
}

fn failed(e: &ObsError) -> ObsStatus {
    match e {
        ObsError::Auth(reason) => ObsStatus::AuthFailed { reason: reason.clone() },
        other => ObsStatus::Failed { reason: other.to_string() },
    }
}

/// The reconnecting thread.
struct Keeper {
    config: ClientConfig,
    password: Option<SecretValue>,
    shared: Arc<Shared>,
    hub: Arc<EventHub>,
    stop: Arc<AtomicBool>,
    unit: Duration,
}

impl Keeper {
    fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// Waits up to `wait`; `false` when asked to stop.
    fn pause(&self, wait: Duration) -> bool {
        let until = Instant::now() + wait;
        while Instant::now() < until {
            if self.stopping() {
                return false;
            }
            std::thread::sleep(WATCH.min(until.saturating_duration_since(Instant::now())));
        }
        !self.stopping()
    }

    fn run(self) {
        loop {
            // Watch the open connection until it drops.
            let reason = loop {
                if !self.pause(WATCH) {
                    return;
                }
                let client = lock(&self.shared.client).clone();
                match client {
                    Some(c) if c.is_connected() => {}
                    Some(c) if c.closed_by() == Some(CloseCode::SessionInvalidated) => {
                        *lock(&self.shared.status) = ObsStatus::Failed {
                            reason: "a person ended this connection in OBS (Tools, WebSocket Server Settings); \
                                     obs.connect connects again"
                                .into(),
                        };
                        return;
                    }
                    Some(c) => break c.close_reason().unwrap_or_else(|| "OBS closed the connection".into()),
                    None => return,
                }
            };
            if !self.reconnect(reason) {
                return;
            }
        }
    }

    /// Tries until connected (`true`), or until the password is refused or a stop (`false`).
    fn reconnect(&self, mut reason: String) -> bool {
        for attempt in 0.. {
            let wait = self.unit * backoff(attempt);
            {
                let mut retries = lock(&self.shared.retries);
                if retries.len() >= RETRY_LOG {
                    retries.remove(0);
                }
                retries.push(wait);
            }
            *lock(&self.shared.status) =
                ObsStatus::Retrying { attempt: attempt + 1, wait_s: wait.as_secs_f64(), reason: reason.clone() };
            if !self.pause(wait) {
                return false;
            }
            *lock(&self.shared.status) = ObsStatus::Connecting;
            let password = self.password.as_ref().map(SecretValue::expose);
            match ObsClient::open(&self.config, password, Arc::clone(&self.hub)) {
                Ok(client) => {
                    let client = Arc::new(client);
                    *lock(&self.shared.status) = connected(&self.config, &client);
                    *lock(&self.shared.client) = Some(client);
                    return true;
                }
                Err(e @ ObsError::Auth(_)) => {
                    *lock(&self.shared.status) = failed(&e);
                    return false;
                }
                Err(e) => reason = e.to_string(),
            }
        }
        false
    }
}

/// The default target, for answers and dry runs.
#[must_use]
pub fn default_target() -> (String, u16) {
    (DEFAULT_HOST.to_owned(), DEFAULT_PORT)
}
