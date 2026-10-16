//! Resources an agent can read besides the op log: the seam a host extends, and the registry that serves it.
//!
//! A [`ResourceProvider`] owns a family of `omuzaki://` URIs. The server asks every provider to list its resources and
//! templates and sends a read to whichever provider [`serves`](ResourceProvider::serves) the URI, so adding a family
//! means implementing the trait and registering it with [`McpOptions::with_provider`](crate::McpOptions::with_provider),
//! never editing a list in this crate. The built-in [`DocProvider`](doc::DocProvider) serves the document.

pub(crate) mod doc;
pub(crate) mod library;
pub(crate) mod measure;
pub(crate) mod metrics;
pub(crate) mod renders;

use std::fmt::Debug;
use std::sync::Arc;

use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{ErrorCode, Grants, OpError, Runner};
use rmcp::ErrorData as McpError;
use rmcp::model::{Resource, ResourceContents, ResourceTemplate};
use serde_json::json;

pub use library::LibraryProvider;
pub use measure::MeasureProvider;
pub use metrics::{MetricsProvider, MetricsSource};
pub use renders::RenderProvider;

/// A family of readable `omuzaki://` resources.
///
/// The server checks the `read` scope before calling [`list`](Self::list) or [`read`](Self::read), so a provider
/// never sees a session that lacks it. Both calls are blocking and run off the async threads; a provider reads the
/// document through the `runner` it is handed and holds no session state.
pub trait ResourceProvider: Debug + Send + Sync {
    /// The URI templates (RFC 6570) this provider serves, for `resources/templates/list`: `omuzaki://character/{id}`.
    fn templates(&self) -> Vec<ResourceTemplate>;

    /// The concrete resources worth listing now, for `resources/list`.
    ///
    /// # Errors
    /// Whatever the runner refuses with.
    fn list(&self, runner: &dyn Runner, grants: &Grants) -> Result<Vec<Resource>, OpError>;

    /// Whether `uri` belongs to this provider. This tells an unknown URI (`resource_not_found` naming the templates)
    /// from one that is this provider's but doesn't exist (its own `not_found`).
    fn serves(&self, uri: &str) -> bool;

    /// The contents of `uri`, which [`serves`](Self::serves) accepted.
    ///
    /// # Errors
    /// `NotFound` for a URI of this family that names nothing, otherwise whatever the runner refuses with.
    fn read(&self, runner: &dyn Runner, grants: &Grants, uri: &str) -> Result<Vec<ResourceContents>, OpError>;

    /// The URIs of this provider's resources that a change at `paths` affects, so a client subscribed to one is sent
    /// `notifications/resources/updated`. It is called on the async threads for every applied op: answer from the
    /// paths alone, without reading the document. A returned URI ending in `/` stands for every resource beneath it
    /// (`omuzaki://character/` is every character), for a change that reaches many resources without naming them, such
    /// as replacing the whole `characters` object. The default affects nothing, so a provider whose resources don't
    /// come from the document (or that doesn't support subscriptions) needs no code.
    fn touched(&self, paths: &[DocPath]) -> Vec<String> {
        let _ = paths;
        Vec::new()
    }
}

/// The providers of one server: the built-in document provider, then the host's.
#[derive(Clone, Debug)]
pub(crate) struct Providers(Vec<Arc<dyn ResourceProvider>>);

impl Providers {
    pub(crate) fn new(extra: &[Arc<dyn ResourceProvider>]) -> Self {
        let mut all: Vec<Arc<dyn ResourceProvider>> = vec![Arc::new(doc::DocProvider)];
        all.extend(extra.iter().map(Arc::clone));
        Self(all)
    }

    pub(crate) fn all(&self) -> &[Arc<dyn ResourceProvider>] {
        &self.0
    }

    /// The provider that owns `uri`, if any.
    pub(crate) fn serving(&self, uri: &str) -> Option<Arc<dyn ResourceProvider>> {
        self.0.iter().find(|p| p.serves(uri)).map(Arc::clone)
    }

    /// The URIs, across all providers, that a change at `paths` affects.
    pub(crate) fn touched(&self, paths: &[DocPath]) -> Vec<String> {
        self.0.iter().flat_map(|p| p.touched(paths)).collect()
    }

    /// Every template, in provider order.
    pub(crate) fn templates(&self) -> Vec<ResourceTemplate> {
        self.0.iter().flat_map(|p| p.templates()).collect()
    }
}

/// The contents a resource returns for a JSON body: pretty text with the MIME type, after the one place that removes
/// the digits an `f32` widened to `f64` brings (see `floats`). Every provider answers through this.
///
/// # Errors
/// `internal` if the body can't be encoded.
pub(crate) fn json_contents(uri: &str, body: serde_json::Value) -> Result<Vec<ResourceContents>, OpError> {
    let text = serde_json::to_string_pretty(&crate::floats::tidied(body))
        .map_err(|e| OpError::internal(format!("couldn't encode {uri}: {e}")))?;
    Ok(vec![ResourceContents::text(text, uri).with_mime_type("application/json")])
}

/// A provider's refusal as the protocol error a client sees: a missing resource is the protocol's own
/// `resource_not_found`, a missing scope keeps the `denied` code the tools use, anything else is internal.
pub(crate) fn protocol_error(error: OpError) -> McpError {
    let mut message = error.message;
    if let Some(hint) = error.hint {
        message = format!("{message} {hint}");
    }
    match error.code {
        ErrorCode::NotFound => McpError::resource_not_found(message, None),
        ErrorCode::Denied => McpError::invalid_request(message, Some(json!({"code": "denied"}))),
        code => McpError::internal_error(message, Some(json!({"code": code.to_string()}))),
    }
}
