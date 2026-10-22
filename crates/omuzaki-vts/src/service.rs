//! [`VtsService`]: the app's one bridge to `VTube Studio`, shared as a service so the `vts.*` ops, the UI and the perform
//! loop use the same session.
//!
//! The ops start and steer the bridge; the perform loop feeds it through a [`VtsFeed`], which pushes each step's
//! channels into the bridge's injector without a lock or an allocation (it re-reads which injector is current only when
//! a connect has replaced the bridge). The hotkey mapping is the project's (`connections/vts`); the ops hand it to the
//! service before each use, and a new bridge starts with the last one handed over.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use omuzaki_anim_api::channel::ChannelBuf;

use crate::client::{BridgeStatus, ClientConfig, ClientError, MemoryTokens, TokenStore, VtsClient};
use crate::clock::{Clock, SystemClock};
use crate::hotkeys::VtsMapping;
use crate::inject::{ChannelMap, Injector, MapError};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Which injector the feed pushes into: replaced, and its generation moved, each time a connect starts a new bridge.
#[derive(Default)]
struct Slot {
    generation: AtomicU64,
    injector: Mutex<Option<Arc<Injector>>>,
}

impl Slot {
    fn set(&self, injector: Option<Arc<Injector>>) {
        *lock(&self.injector) = injector;
        self.generation.fetch_add(1, Ordering::Release);
    }
}

/// The perform loop's end of the bridge: [`push`](Self::push) each step's channels.
pub struct VtsFeed {
    slot: Arc<Slot>,
    seen: u64,
    injector: Option<Arc<Injector>>,
}

impl VtsFeed {
    /// Hands this step's channels to the bridge, which sends the newest at VTS's pace. Allocation-free and lock-free
    /// except on the first push after a connect started a new bridge. A step with no bridge costs one atomic load.
    pub fn push(&mut self, channels: &ChannelBuf, face_found: bool) {
        let generation = self.slot.generation.load(Ordering::Acquire);
        if generation != self.seen {
            self.injector.clone_from(&lock(&self.slot.injector));
            self.seen = generation;
        }
        if let Some(injector) = &self.injector {
            injector.push(channels, face_found);
        }
    }
}

impl std::fmt::Debug for VtsFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VtsFeed").field("bridge", &self.injector.is_some()).finish_non_exhaustive()
    }
}

/// The bridge, if one was started, and where it points.
struct Bridge {
    config: ClientConfig,
    client: VtsClient,
}

/// The connection to `VTube Studio`, if any. Cheap to share: the app holds it in an `Arc`.
pub struct VtsService {
    map: ChannelMap,
    clock: Arc<dyn Clock>,
    bridge: Mutex<Option<Bridge>>,
    mapping: Mutex<VtsMapping>,
    slot: Arc<Slot>,
    /// Where VTS's authentication tokens are kept.
    tokens: Arc<dyn TokenStore>,
}

impl std::fmt::Debug for VtsService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VtsService").field("status", &self.status()).finish_non_exhaustive()
    }
}

impl VtsService {
    /// A service with no bridge, injecting the built-in channel map on the system clock, with tokens kept in memory
    /// until the app quits ([`with_tokens`](Self::with_tokens) keeps them in the keychain).
    ///
    /// # Errors
    /// [`MapError`] if the built-in map does not parse (a bug in `data/channel-map.toml`; this crate's tests catch it).
    pub fn new() -> Result<Self, MapError> {
        Ok(Self::with(ChannelMap::builtin()?, Arc::new(SystemClock::new())))
    }

    /// The same service, keeping VTS's tokens in `tokens` (the app passes
    /// [`SecretTokens`](crate::client::SecretTokens) over the OS keychain, so VTS asks once, not every start).
    #[must_use]
    pub fn with_tokens(mut self, tokens: Arc<dyn TokenStore>) -> Self {
        self.tokens = tokens;
        self
    }

    /// A service over `map`, timed by `clock`, with tokens in memory.
    #[must_use]
    pub fn with(map: ChannelMap, clock: Arc<dyn Clock>) -> Self {
        Self {
            map,
            clock,
            bridge: Mutex::new(None),
            mapping: Mutex::default(),
            slot: Arc::default(),
            tokens: Arc::new(MemoryTokens::default()),
        }
    }

    /// The perform loop's feed. Any number may exist; the bridge sends the newest push of any of them.
    #[must_use]
    pub fn feed(&self) -> VtsFeed {
        VtsFeed { slot: Arc::clone(&self.slot), seen: u64::MAX, injector: None }
    }

    /// A person's or an agent's connect to VTS at `config`: starts a bridge (replacing one aimed elsewhere, which
    /// switches off what it switched on), asks it to connect now, which may show VTS's permission popup, and waits up
    /// to `wait` for it to settle: connected, waiting on the popup, or stopped for a reason a person must act on.
    /// Returns the status then (still `Connecting` if VTS hasn't answered within `wait`).
    ///
    /// # Errors
    /// [`ClientError::NotLoopback`] for a host that is not this computer; [`ClientError::Thread`] if the bridge thread
    /// cannot start.
    pub fn connect(&self, config: ClientConfig, wait: Duration) -> Result<BridgeStatus, ClientError> {
        // The mapping is read before the bridge is locked: `map` takes them in the other order.
        let mapping = self.mapping();
        let watch = {
            let mut bridge = lock(&self.bridge);
            if bridge.as_ref().is_none_or(|b| b.config != config) {
                // The old bridge stops first: two bridges must never hold one VTS's parameters.
                if bridge.take().is_some() {
                    self.slot.set(None);
                }
                let client =
                    VtsClient::spawn(config.clone(), Arc::clone(&self.tokens), Arc::clone(&self.clock), &self.map)?;
                client.set_mapping(mapping);
                self.slot.set(Some(client.injector()));
                *bridge = Some(Bridge { config, client });
            }
            let Some(b) = bridge.as_ref() else { return Ok(BridgeStatus::Disconnected) };
            let watch = b.client.status().clone();
            if matches!(watch.get(), BridgeStatus::Connected(_)) {
                return Ok(watch.get());
            }
            let before = watch.version();
            b.client.connect();
            (watch, before)
        };
        let (watch, before) = watch;
        let settled = |s: &BridgeStatus| !matches!(s, BridgeStatus::Connecting { .. });
        Ok(watch.wait_after(before, wait, settled).unwrap_or_else(|| watch.get()))
    }

    /// Switches off what the bridge switched on, closes the connection and stops retrying, waiting up to `wait` for it
    /// to close. Returns whether a bridge was running, and the status then.
    pub fn disconnect(&self, wait: Duration) -> (bool, BridgeStatus) {
        let watch = lock(&self.bridge).as_ref().map(|b| {
            b.client.disconnect();
            b.client.status().clone()
        });
        let Some(watch) = watch else { return (false, BridgeStatus::Disconnected) };
        let closed = |s: &BridgeStatus| !matches!(s, BridgeStatus::Connected(_) | BridgeStatus::Connecting { .. });
        (true, watch.wait_for(wait, closed).unwrap_or_else(|| watch.get()))
    }

    /// The bridge's status: [`BridgeStatus::Disconnected`] before any connect.
    #[must_use]
    pub fn status(&self) -> BridgeStatus {
        lock(&self.bridge).as_ref().map_or(BridgeStatus::Disconnected, |b| b.client.status().get())
    }

    /// Where the bridge points, if one was started.
    #[must_use]
    pub fn config(&self) -> Option<ClientConfig> {
        lock(&self.bridge).as_ref().map(|b| b.config.clone())
    }

    /// The hotkey and expression mapping.
    #[must_use]
    pub fn mapping(&self) -> VtsMapping {
        lock(&self.mapping).clone()
    }

    /// Makes `mapping` the bridge's mapping (the project's `connections/vts`, which the ops read before each use, so an
    /// undone `vts.map` is followed too). Sends it to the bridge only when it changed.
    pub fn set_mapping(&self, mapping: VtsMapping) {
        let mut current = lock(&self.mapping);
        if *current == mapping {
            return;
        }
        *current = mapping;
        if let Some(b) = lock(&self.bridge).as_ref() {
            b.client.set_mapping(current.clone());
        }
    }

    /// Runs what `omuzaki` is mapped to. The caller checks the mapping first; an unmapped id does nothing.
    pub fn trigger(&self, omuzaki: &str) {
        if let Some(b) = lock(&self.bridge).as_ref() {
            b.client.trigger(omuzaki);
        }
    }

    /// Switches off the expression `omuzaki` is mapped to, if the bridge switched it on.
    pub fn release(&self, omuzaki: &str) {
        if let Some(b) = lock(&self.bridge).as_ref() {
            b.client.release(omuzaki);
        }
    }

    /// Whether a token for `instance` is stored (never the token itself).
    #[must_use]
    pub fn token_stored(&self, instance: &str) -> bool {
        self.tokens.get(instance).is_some()
    }

    /// The channel map the bridge injects.
    #[must_use]
    pub fn channel_map(&self) -> &ChannelMap {
        &self.map
    }
}

impl Drop for VtsService {
    fn drop(&mut self) {
        // The bridge's own drop switches off its expressions and closes; the feeds stop pushing into it.
        self.slot.set(None);
        drop(lock(&self.bridge).take());
    }
}
