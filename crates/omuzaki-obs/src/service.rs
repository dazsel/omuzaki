//! [`ObsService`]: the app's one connection to OBS, shared as a service so the ops, the UI and automations use the same
//! link.

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::client::{ObsClient, ObsError};
use crate::limits::DEFAULT_PORT;
use crate::protocol::{ObsEvent, RequestType};

/// Where OBS listens by default: this machine.
pub const DEFAULT_HOST: &str = "127.0.0.1";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The connection to OBS, if any. Cheap to share: the app holds it in an `Arc`.
#[derive(Debug, Default)]
pub struct ObsService {
    client: Mutex<Option<Arc<ObsClient>>>,
}

impl ObsService {
    /// A service with no connection.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Connects to `host:port`, replacing any connection there was.
    ///
    /// # Errors
    /// As [`ObsClient::connect`]; the old connection is kept when the new one fails.
    pub fn connect(&self, host: &str, port: u16, password: Option<&str>) -> Result<Arc<ObsClient>, ObsError> {
        let client = Arc::new(ObsClient::connect(host, port, password)?);
        *lock(&self.client) = Some(Arc::clone(&client));
        Ok(client)
    }

    /// Closes the connection; `false` when there was none.
    pub fn disconnect(&self) -> bool {
        lock(&self.client).take().is_some()
    }

    /// The open connection, if it is still connected.
    #[must_use]
    pub fn client(&self) -> Option<Arc<ObsClient>> {
        lock(&self.client).as_ref().filter(|c| c.is_connected()).cloned()
    }

    /// The last connection, connected or not (for its close reason).
    #[must_use]
    pub fn last(&self) -> Option<Arc<ObsClient>> {
        lock(&self.client).clone()
    }

    /// Sends `request` on the open connection.
    ///
    /// # Errors
    /// [`ObsError::Closed`] when there is no open connection; otherwise as [`ObsClient::request`].
    pub fn request<R: RequestType>(&self, request: &R) -> Result<R::Response, ObsError> {
        let client = self.client().ok_or_else(|| ObsError::Closed(self.last().and_then(|c| c.close_reason())))?;
        client.request(request)
    }

    /// Events from the open connection; `None` when there is none.
    #[must_use]
    pub fn subscribe(&self) -> Option<Receiver<ObsEvent>> {
        self.client().map(|c| c.subscribe())
    }
}

/// The default target, for answers and dry runs.
#[must_use]
pub fn default_target() -> (String, u16) {
    (DEFAULT_HOST.to_owned(), DEFAULT_PORT)
}
