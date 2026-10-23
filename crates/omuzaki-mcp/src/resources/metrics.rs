//! The live-metrics resource: `omuzaki://metrics/live`, how the studio is doing right now, as a JSON snapshot.
//!
//! The numbers are the studio's, not this crate's: the App implements [`MetricsSource`] by composing what it has (the
//! frame meter, the tracker's latency, the audio status), because an App may depend on every crate and this one may not.
//! The resource only carries what the source says, so it names no metric of its own.

use std::sync::Arc;

use omuzaki_metrics_api::MetricsReader;
use omuzaki_ops_api::{Grants, OpError, Runner};
use rmcp::model::{Resource, ResourceContents, ResourceTemplate};
use serde_json::{Map, Value, json};

use super::{ResourceProvider, json_contents};

/// The one URI this provider serves.
pub(crate) const METRICS_URI: &str = "omuzaki://metrics/live";
const JSON: &str = "application/json";

/// The studio's live numbers, supplied by the App.
pub trait MetricsSource: std::fmt::Debug + Send + Sync {
    /// The latest numbers as a JSON object, for example `{"fps": 59.9, "frame_ms": 6.1, "latency_ms": 41.0}`. Names and
    /// units are the source's. Returns at once: it is called for every read, on a blocking thread.
    ///
    /// # Errors
    /// `unavailable` when nothing is running to measure.
    fn snapshot(&self) -> Result<Map<String, Value>, OpError>;
}

/// [`MetricsSource`] over the typed hub reader the App's subsystems publish into (`omuzaki-metrics-api`): every
/// section, `null` until its writer has published once.
#[derive(Debug, Clone)]
pub struct ReaderSource(pub MetricsReader);

impl MetricsSource for ReaderSource {
    fn snapshot(&self) -> Result<Map<String, Value>, OpError> {
        match serde_json::to_value(self.0.read()) {
            Ok(Value::Object(sections)) => Ok(sections),
            Ok(other) => Err(OpError::internal(format!("the live metrics are not an object: {other}"))),
            Err(e) => Err(OpError::internal(format!("the live metrics can't be written as JSON: {e}"))),
        }
    }
}

/// What a headless server (`omuzaki mcp` with no studio) answers: nothing runs a frame loop to measure.
const HEADLESS_NOTE: &str = "no studio is running; start one to see live numbers";

/// Serves `omuzaki://metrics/live`, under the `read` scope like every resource.
///
/// It takes a mode, chosen by the App, never guessed from empty sections: [`MetricsProvider::studio`] serves the
/// source's snapshot as `{"running": true, "metrics": ...}` (a section no subsystem has published yet is `null`), and
/// [`MetricsProvider::headless`] always serves `{"running": false, "note": ...}`. Both list the URI as
/// `application/json`.
///
/// **No updates.** A client may subscribe to the URI (the server accepts any URI a provider serves), but nothing is ever
/// pushed for it: no op event changes live numbers, and a timer here would wake every subscriber for nothing. A client
/// reads it again for fresh numbers. A rollback, undo or redo still notifies every subscribed URI, this one included.
///
/// Registering the provider is the App's job ([`McpOptions::with_provider`](crate::McpOptions::with_provider)).
#[derive(Debug)]
pub struct MetricsProvider {
    source: Option<Arc<dyn MetricsSource>>,
}

impl MetricsProvider {
    /// A running studio's provider over `source`, its live numbers.
    #[must_use]
    pub fn studio(source: Arc<dyn MetricsSource>) -> Self {
        Self { source: Some(source) }
    }

    /// The same as [`studio`](Self::studio).
    #[must_use]
    pub fn new(source: Arc<dyn MetricsSource>) -> Self {
        Self::studio(source)
    }

    /// A headless server's provider: it says no studio is running.
    #[must_use]
    pub fn headless() -> Self {
        Self { source: None }
    }
}

impl ResourceProvider for MetricsProvider {
    /// A fixed URI is not a template.
    fn templates(&self) -> Vec<ResourceTemplate> {
        Vec::new()
    }

    fn list(&self, _runner: &dyn Runner, _grants: &Grants) -> Result<Vec<Resource>, OpError> {
        Ok(vec![
            Resource::new(METRICS_URI, "live metrics")
                .with_title("Live metrics")
                .with_description(
                    "The studio's frame rate, latency and budget use right now. A snapshot taken when you read it; \
                     nothing is pushed for it, so read it again for fresh numbers.",
                )
                .with_mime_type(JSON),
        ])
    }

    fn serves(&self, uri: &str) -> bool {
        uri == METRICS_URI
    }

    fn read(&self, _runner: &dyn Runner, _grants: &Grants, uri: &str) -> Result<Vec<ResourceContents>, OpError> {
        let Some(source) = &self.source else {
            return json_contents(uri, json!({ "running": false, "note": HEADLESS_NOTE }));
        };
        let metrics = source.snapshot()?;
        let body = json!({
            "running": true,
            "metrics": metrics,
            "note": "A snapshot taken when you read it. The names and units are the studio's; nothing is pushed for \
                     this resource.",
        });
        json_contents(uri, body)
    }
}
