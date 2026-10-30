//! [`YouTubeService`]: the app's one YouTube connection, shared as a service. It knows where YouTube is and where
//! events go (the show's inbox); `youtube.connect` hands it the token it read from the secret store.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::{Clock, SystemClock};
use omuzaki_http_api::HttpTransport;
use omuzaki_live_api::event::EventSink;
use omuzaki_secrets_api::value::SecretValue;

use crate::adapter::{Adapter, Endpoints, YouTubeStatus};
use crate::api::{API_URL, Api};
use crate::error::YouTubeError;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The YouTube connection, if any.
pub struct YouTubeService {
    endpoints: Endpoints,
    sink: Arc<dyn EventSink>,
    clock: Arc<dyn Clock>,
    adapter: Mutex<Option<Adapter>>,
}

impl std::fmt::Debug for YouTubeService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("YouTubeService").field("endpoints", &self.endpoints).finish_non_exhaustive()
    }
}

impl YouTubeService {
    /// A service that reaches YouTube's own API through `http` and publishes into `sink`.
    pub fn new(http: Arc<dyn HttpTransport>, sink: Arc<dyn EventSink>) -> Self {
        Self::with_endpoints(Endpoints::new(Api::new(http, API_URL)), sink)
    }

    /// A service on other addresses: a test server's. Addresses are fixed when the service is built, never taken from
    /// an op, so no caller can send the token anywhere but YouTube.
    pub fn with_endpoints(endpoints: Endpoints, sink: Arc<dyn EventSink>) -> Self {
        Self { endpoints, sink, clock: Arc::new(SystemClock::new()), adapter: Mutex::new(None) }
    }

    /// Connects with `token` to the channel's live chat (or `video`'s), replacing any connection there was. The
    /// connection runs on its own thread; read [`status`](Self::status) for how it goes.
    ///
    /// # Errors
    /// [`YouTubeError::Network`] if the thread can't start.
    pub fn connect(&self, token: SecretValue, video: Option<String>) -> Result<(), YouTubeError> {
        let mut adapter = lock(&self.adapter);
        // The old connection closes first, so two never publish at once.
        adapter.take();
        *adapter = Some(Adapter::start(
            self.endpoints.clone(),
            token,
            video,
            Arc::clone(&self.sink),
            Arc::clone(&self.clock),
        )?);
        Ok(())
    }

    /// Closes the connection; `false` when there was none.
    pub fn disconnect(&self) -> bool {
        let old = lock(&self.adapter).take();
        old.is_some()
    }

    /// The connection's state; `None` when there is none.
    #[must_use]
    pub fn status(&self) -> Option<YouTubeStatus> {
        lock(&self.adapter).as_ref().map(Adapter::status)
    }
}
