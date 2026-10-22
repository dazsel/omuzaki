//! The client session (#935): one connection to `VTube Studio` on this computer, on a thread of its own.
//!
//! [`VtsClient::spawn`] starts the bridge thread. It finds VTS's port (VTS's UDP broadcast on 47779 when no port is set,
//! else 8001), authenticates with the stable identity ([`crate::PLUGIN_NAME`], [`crate::PLUGIN_DEVELOPER`]) using a
//! stored token first, and only after a person's [`connect`](VtsClient::connect) asks VTS for a new one (the popup in
//! VTS): an automatic reconnect never shows the popup. Then it creates the custom parameters, feeds the
//! [`Injector`]'s frames (`crate::inject`), and sends the hotkeys and expressions the person or an op triggers
//! (`crate::hotkeys`).
//!
//! - **Loopback only:** a host other than this computer is refused at `spawn`; nothing leaves the machine.
//! - **Reconnects** after VTS closes or restarts, waiting 1, 2, 4, 8 and 16 s, then every 30 s.
//! - **Clean degrade on revoke:** when VTS answers that the session is not authenticated, the bridge deletes the stored
//!   token, stops every injection and hotkey at once, publishes [`BridgeStatus::Revoked`] and waits for a person's
//!   `connect`. A denied popup publishes [`BridgeStatus::Denied`] and is not asked again.
//! - **Tokens** live in a [`TokenStore`] (the OS keychain through [`SecretTokens`], or [`MemoryTokens`]); never in
//!   project files, logs, op records or status.

mod session;

use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore, SecretValue};
use serde::Serialize;

use crate::clock::Clock;
use crate::hotkeys::VtsMapping;
use crate::inject::{ChannelMap, Injector};
use crate::limits::{DEFAULT_PORT, DISCOVERY_PORT};
use crate::protocol::Hotkey;

/// How long one discovery listens for VTS's broadcast before falling back to [`DEFAULT_PORT`]. VTS broadcasts every
/// two seconds.
pub const DISCOVERY_WAIT: Duration = Duration::from_millis(2_500);
/// The waits between reconnect attempts; the last repeats.
pub const BACKOFF: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
];

/// Where VTS runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientConfig {
    /// This computer: `127.0.0.1`, `::1` or `localhost`. Anything else is refused.
    pub host: String,
    /// VTS's API port; `None` listens for VTS's broadcast and falls back to 8001.
    pub port: Option<u16>,
    /// Which VTS instance (the token is stored per instance). Default `default`.
    pub instance: String,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self { host: "127.0.0.1".into(), port: None, instance: "default".into() }
    }
}

impl ClientConfig {
    /// Whether `host` is this computer.
    #[must_use]
    pub fn is_loopback(&self) -> bool {
        let host = self.host.trim_matches(['[', ']']);
        host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
    }
}

/// Why the bridge could not start.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ClientError {
    /// The host is not this computer.
    #[error("VTube Studio must run on this computer; `{0}` is not a loopback address")]
    NotLoopback(String),
    /// The bridge thread did not start.
    #[error("the VTube Studio bridge could not start: {0}")]
    Thread(String),
}

/// Where the bridge keeps VTS's authentication token, per instance.
pub trait TokenStore: Send + Sync {
    /// The stored token.
    fn get(&self, instance: &str) -> Option<String>;
    /// Stores a token, replacing any.
    fn put(&self, instance: &str, token: &str);
    /// Forgets the token.
    fn delete(&self, instance: &str);
}

/// Tokens in memory, for tests and for a session that should not remember.
#[derive(Debug, Default)]
pub struct MemoryTokens(Mutex<HashMap<String, String>>);

impl TokenStore for MemoryTokens {
    fn get(&self, instance: &str) -> Option<String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).get(instance).cloned()
    }

    fn put(&self, instance: &str, token: &str) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert(instance.to_owned(), token.to_owned());
    }

    fn delete(&self, instance: &str) {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(instance);
    }
}

/// Tokens in the secret store (the OS keychain), under `vts/<instance>`.
pub struct SecretTokens(pub Arc<dyn SecretStore>);

impl SecretTokens {
    fn label(instance: &str) -> Option<SecretLabel> {
        let safe: String = instance
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c.to_ascii_lowercase() } else { '-' })
            .collect();
        SecretLabel::new(format!("vts/{safe}")).ok()
    }
}

impl TokenStore for SecretTokens {
    fn get(&self, instance: &str) -> Option<String> {
        let label = Self::label(instance)?;
        self.0.get(&label).ok().flatten().map(|v| v.expose().to_owned())
    }

    fn put(&self, instance: &str, token: &str) {
        if let Some(label) = Self::label(instance) {
            // A keychain that refuses leaves the session working; the next connect asks again.
            drop(self.0.set(&label, SecretValue::new(token)));
        }
    }

    fn delete(&self, instance: &str) {
        if let Some(label) = Self::label(instance) {
            drop(self.0.delete(&label));
        }
    }
}

/// The model VTS has loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelInfo {
    /// VTS's unique model id: the key of a [`VtsMapping`].
    pub id: String,
    /// Its display name.
    pub name: String,
}

/// The bridge while connected.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Connected {
    /// VTS's version.
    pub vts_version: String,
    /// The loaded model, if any.
    pub model: Option<ModelInfo>,
    /// VTS's frame rate.
    pub fps: u32,
    /// VTS parameters another plugin owns, fed in `add` instead ("owned by another plugin" in the UI).
    pub conflicts: Vec<String>,
    /// The model's hotkeys, for [`suggest`](crate::hotkeys::suggest) and the mapping editor.
    pub hotkeys: Vec<Hotkey>,
    /// Hotkey triggers dropped because VTS's queue was full.
    pub hotkeys_dropped: u64,
    /// Expression files the bridge has switched on.
    pub expressions_on: Vec<String>,
}

/// What the bridge is doing.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BridgeStatus {
    /// Not connected and not trying: no token yet, or a person disconnected.
    Disconnected,
    /// Connecting, or waiting to retry after VTS went away (`attempt` 0 is the first try).
    Connecting {
        /// Retries so far.
        attempt: u32,
    },
    /// The popup is showing in VTS.
    AwaitingApproval,
    /// Connected and authenticated.
    Connected(Connected),
    /// The person revoked access in VTS: the token is gone and nothing is sent until a person connects again.
    Revoked {
        /// Expressions the bridge switched on and could not switch off.
        expressions_may_be_on: Vec<String>,
    },
    /// The person denied the popup; not asked again until a person connects.
    Denied,
    /// Something else went wrong; a person's connect tries again.
    Failed {
        /// What, in words.
        reason: String,
    },
}

#[derive(Debug)]
struct WatchState {
    status: BridgeStatus,
    version: u64,
    polls: u64,
    sent: u64,
}

/// The bridge's status, readable from any thread.
#[derive(Debug, Clone)]
pub struct StatusWatch(Arc<(Mutex<WatchState>, Condvar)>);

impl StatusWatch {
    fn new() -> Self {
        Self(Arc::new((
            Mutex::new(WatchState { status: BridgeStatus::Disconnected, version: 0, polls: 0, sent: 0 }),
            Condvar::new(),
        )))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, WatchState> {
        self.0.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The status now.
    #[must_use]
    pub fn get(&self) -> BridgeStatus {
        self.lock().status.clone()
    }

    /// Changes so far: a new value means a new status.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.lock().version
    }

    /// How many times the bridge thread has looked at its clock and its queues: a test waits for this to move after
    /// advancing a fake clock, so the bridge has seen the new time.
    #[must_use]
    pub fn polls(&self) -> u64 {
        self.lock().polls
    }

    /// How many requests the bridge has written to the socket: a test waits until the other side has read them
    /// all, so their arrival times are the times they were sent.
    #[must_use]
    pub fn sent(&self) -> u64 {
        self.lock().sent
    }

    /// Waits up to `timeout` for a status `pred` accepts, and returns it.
    #[must_use]
    pub fn wait_for(&self, timeout: Duration, pred: impl Fn(&BridgeStatus) -> bool) -> Option<BridgeStatus> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if pred(&state.status) {
                return Some(state.status.clone());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            state = self.0.1.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Waits up to `timeout` for a status `pred` accepts that is newer than `version` (from [`version`](Self::version)),
    /// and returns it.
    #[must_use]
    pub fn wait_after(
        &self,
        version: u64,
        timeout: Duration,
        pred: impl Fn(&BridgeStatus) -> bool,
    ) -> Option<BridgeStatus> {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        loop {
            if state.version > version && pred(&state.status) {
                return Some(state.status.clone());
            }
            let left = deadline.checked_duration_since(Instant::now())?;
            state = self.0.1.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// Waits up to `timeout` until the bridge has polled `n` more times.
    #[must_use]
    pub fn wait_polls(&self, n: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.lock();
        let target = state.polls + n;
        while state.polls < target {
            let Some(left) = deadline.checked_duration_since(Instant::now()) else { return false };
            state = self.0.1.wait_timeout(state, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        true
    }

    fn set(&self, status: BridgeStatus) {
        let mut state = self.lock();
        if state.status != status {
            state.status = status;
            state.version += 1;
            self.0.1.notify_all();
        }
    }

    fn poll(&self) {
        self.lock().polls += 1;
        self.0.1.notify_all();
    }

    fn sent_one(&self) {
        self.lock().sent += 1;
    }
}

enum Command {
    Connect,
    Disconnect,
    Trigger(String),
    Release(String),
    Mapping(VtsMapping),
    Stop,
}

/// The bridge to `VTube Studio`. Dropping it switches off what it switched on and closes the connection.
pub struct VtsClient {
    commands: Sender<Command>,
    watch: StatusWatch,
    injector: Arc<Injector>,
    thread: Option<JoinHandle<()>>,
}

impl VtsClient {
    /// Starts the bridge thread. With a stored token it connects at once; without one it waits for
    /// [`connect`](Self::connect).
    ///
    /// # Errors
    /// [`ClientError::NotLoopback`] for a host that is not this computer; [`ClientError::Thread`] when the thread
    /// cannot start.
    pub fn spawn(
        config: ClientConfig,
        tokens: Arc<dyn TokenStore>,
        clock: Arc<dyn Clock>,
        map: &ChannelMap,
    ) -> Result<Self, ClientError> {
        if !config.is_loopback() {
            return Err(ClientError::NotLoopback(config.host));
        }
        let (commands, rx) = channel();
        let watch = StatusWatch::new();
        let injector = Arc::new(Injector::new(map));
        let worker = session::Worker::new(config, tokens, clock, rx, watch.clone(), Arc::clone(&injector), map);
        let thread = std::thread::Builder::new()
            .name("vts-bridge".into())
            .spawn(move || worker.run())
            .map_err(|e| ClientError::Thread(e.to_string()))?;
        Ok(Self { commands, watch, injector, thread: Some(thread) })
    }

    /// A person's connect: tries now, and may show VTS's popup when no stored token works.
    pub fn connect(&self) {
        drop(self.commands.send(Command::Connect));
    }

    /// Switches off the expressions the bridge switched on, closes the connection and stops retrying.
    pub fn disconnect(&self) {
        drop(self.commands.send(Command::Disconnect));
    }

    /// Runs what `omuzaki` is mapped to in the current model: its hotkey, or switches its expression on.
    pub fn trigger(&self, omuzaki: impl Into<String>) {
        drop(self.commands.send(Command::Trigger(omuzaki.into())));
    }

    /// Switches off the expression `omuzaki` is mapped to, if the bridge switched it on.
    pub fn release(&self, omuzaki: impl Into<String>) {
        drop(self.commands.send(Command::Release(omuzaki.into())));
    }

    /// Replaces the hotkey and expression mapping.
    pub fn set_mapping(&self, mapping: VtsMapping) {
        drop(self.commands.send(Command::Mapping(mapping)));
    }

    /// The status, readable from any thread.
    #[must_use]
    pub fn status(&self) -> &StatusWatch {
        &self.watch
    }

    /// The frame thread's side: [`Injector::push`] each frame's channels.
    #[must_use]
    pub fn injector(&self) -> Arc<Injector> {
        Arc::clone(&self.injector)
    }
}

impl Drop for VtsClient {
    fn drop(&mut self) {
        drop(self.commands.send(Command::Stop));
        if let Some(thread) = self.thread.take() {
            // A panicked bridge thread has nothing left to clean up.
            drop(thread.join());
        }
    }
}

/// Listens on `socket` up to `wait` for VTS's state broadcast and returns the port it announces for `instance`
/// (any instance when `instance` is `default`).
pub fn discover_on(socket: &UdpSocket, wait: Duration, instance: &str) -> Option<u16> {
    let deadline = Instant::now() + wait;
    let mut buf = [0u8; 2048];
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        socket.set_read_timeout(Some(left.max(Duration::from_millis(1)))).ok()?;
        let Ok(n) = socket.recv(&mut buf) else { return None };
        if let Some((port, id)) = parse_broadcast(&buf[..n])
            && (instance == "default" || id == instance)
        {
            return Some(port);
        }
    }
}

/// VTS's port: the broadcast's when one arrives on [`DISCOVERY_PORT`] within [`DISCOVERY_WAIT`], else 8001.
fn discover(instance: &str) -> u16 {
    UdpSocket::bind(("0.0.0.0", DISCOVERY_PORT))
        .ok()
        .and_then(|socket| discover_on(&socket, DISCOVERY_WAIT, instance))
        .unwrap_or(DEFAULT_PORT)
}

/// The port and instance id of a `VTubeStudioAPIStateBroadcast`, when it says the API is on.
#[must_use]
pub fn parse_broadcast(datagram: &[u8]) -> Option<(u16, String)> {
    let value: serde_json::Value = serde_json::from_slice(datagram).ok()?;
    if value["apiName"] != crate::protocol::API_NAME || value["messageType"] != "VTubeStudioAPIStateBroadcast" {
        return None;
    }
    let data = &value["data"];
    if data["active"] != true {
        return None;
    }
    let port = u16::try_from(data["port"].as_u64()?).ok()?;
    Some((port, data["instanceID"].as_str().unwrap_or_default().to_owned()))
}

#[cfg(test)]
mod tests;
