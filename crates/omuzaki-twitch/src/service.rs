//! [`TwitchService`]: the app's one Twitch connection, shared as a service. It knows where Twitch is and where
//! events go (the automation engine's inbox); `twitch.connect` hands it the token it read from the secret store.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::{Clock, SystemClock};
use omuzaki_http_api::HttpTransport;
use omuzaki_live_api::event::EventSink;
use omuzaki_secrets_api::value::SecretValue;

use crate::adapter::{Adapter, Endpoints, TwitchStatus};
use crate::error::TwitchError;
use crate::eventsub::{EVENTSUB_URL, HELIX_URL, OAUTH_URL};
use crate::helix::Helix;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The Twitch connection, if any.
pub struct TwitchService {
    endpoints: Endpoints,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
    adapter: Mutex<Option<Adapter>>,
}

impl std::fmt::Debug for TwitchService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TwitchService").field("endpoints", &self.endpoints).finish_non_exhaustive()
    }
}

impl TwitchService {
    /// A service that reaches Twitch's own addresses through `http` and publishes into `sink`.
    pub fn new(http: Arc<dyn HttpTransport>, sink: Arc<dyn EventSink>) -> Self {
        let helix = Helix::new(http, HELIX_URL, OAUTH_URL);
        Self::with_endpoints(Endpoints { socket: EVENTSUB_URL.to_owned(), helix }, sink)
    }

    /// A service on other addresses: a test server's. Addresses are fixed when the service is built, never taken
    /// from an op, so no caller can send the token anywhere but Twitch.
    pub fn with_endpoints(endpoints: Endpoints, sink: Arc<dyn EventSink>) -> Self {
        Self { endpoints, sink, clock: Arc::new(SystemClock::new()), adapter: Mutex::new(None) }
    }

    /// Connects with `token`, replacing any connection there was. The connection runs on its own thread; read
    /// [`status`](Self::status) for how it goes.
    ///
    /// # Errors
    /// [`TwitchError::Network`] if the thread can't start.
    pub fn connect(&self, token: SecretValue) -> Result<(), TwitchError> {
        let mut adapter = lock(&self.adapter);
        // The old connection closes first, so two never publish at once.
        adapter.take();
        *adapter =
            Some(Adapter::start(self.endpoints.clone(), token, Arc::clone(&self.sink), Arc::clone(&self.clock))?);
        Ok(())
    }

    /// Closes the connection; `false` when there was none.
    pub fn disconnect(&self) -> bool {
        let old = lock(&self.adapter).take();
        old.is_some()
    }

    /// The connection's state; `None` when there is none.
    #[must_use]
    pub fn status(&self) -> Option<TwitchStatus> {
        lock(&self.adapter).as_ref().map(Adapter::status)
    }
}
