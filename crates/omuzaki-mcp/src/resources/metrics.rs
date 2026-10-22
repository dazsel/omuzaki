//! The live-metrics resource: `omuzaki://metrics/live`, how the studio is doing right now, as a JSON snapshot.
//!
//! The numbers are the studio's, not this crate's: the App implements [`MetricsSource`] by composing what it has (the
//! frame meter, the tracker's latency, the audio status), because an App may depend on every crate and this one may not.
//! The resource only carries what the source says, so it names no metric of its own.

use std::sync::Arc;

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

/// Serves `omuzaki://metrics/live`.
///
/// **No updates.** A client may subscribe to the URI (the server accepts any URI a provider serves), but nothing is ever
/// pushed for it: no op event changes live numbers, and a timer here would wake every subscriber for nothing. A client
/// reads it again for fresh numbers. A rollback, undo or redo still notifies every subscribed URI, this one included.
///
/// Registering the provider, with a source, is the App's job ([`McpOptions::with_provider`](crate::McpOptions::with_provider)).
#[derive(Debug)]
pub struct MetricsProvider {
    source: Arc<dyn MetricsSource>,
}

impl MetricsProvider {
    /// A provider over `source`, the App's live numbers.
    #[must_use]
    pub fn new(source: Arc<dyn MetricsSource>) -> Self {
        Self { source }
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
        let metrics = self.source.snapshot()?;
        let body = json!({
            "metrics": metrics,
            "note": "A snapshot taken when you read it. The names and units are the studio's; nothing is pushed for \
                     this resource.",
        });
        json_contents(uri, body)
    }
}
