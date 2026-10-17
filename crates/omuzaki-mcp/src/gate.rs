//! The front door of the HTTP port: one path, one credential, bounded bodies and sessions, then rmcp.
//!
//! Every request must (1) name the MCP path and (2) carry a live access token. A POST's body is then (3) read here,
//! within `max_body_bytes` and `body_timeout`, and a POST that would open a session (4) needs room under the token's
//! `max_sessions_per_token`. Only then does (5) that token's rmcp service see it, and it checks `Host` and `Origin`
//! (see [`crate::access`]). Anything refused is answered here without touching the runner, in a message that names
//! the limit and its value (see [`crate::limits`]).

use std::convert::Infallible;
use std::fmt::Display;
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full};
use hyper::body::Body;
use hyper::header::{CONNECTION, CONTENT_TYPE, HeaderValue, WWW_AUTHENTICATE};
use hyper::{Method, Request, Response, StatusCode};

use crate::access::{Denied, Registry};
use crate::limits::{
    BodyRefusal, Limits, REFUSAL_WARN_INTERVAL, RefusalLog, SessionsFull, may_open_session, read_body,
};

/// Where the gate writes the refusals it logs. The default writes `tracing` events; a test substitutes a sink of its own
/// and reads what the gate decided without installing a process-wide subscriber (whose callsite cache other threads'
/// events can disturb).
pub(crate) trait RefusalSink: Send + Sync {
    /// A request without a valid token. `suppressed` is `Some(n)` when it is written at `warn` (with `n` earlier
    /// refusals only counted), `None` when it is only a `debug` event.
    fn token_refused(&self, suppressed: Option<u64>, denied: Denied, method: &Method);
    /// A request body that was not accepted, with the status it got and the message it was answered with.
    fn body_refused(&self, suppressed: Option<u64>, status: StatusCode, message: &str);
}

/// The production sink: `warn` for the first refusal of an interval, `debug` for the rest.
struct Tracing;

impl RefusalSink for Tracing {
    fn token_refused(&self, suppressed: Option<u64>, denied: Denied, method: &Method) {
        // Never the credential itself: only that there was a failure, and what kind.
        if let Some(suppressed) = suppressed {
            tracing::warn!(?denied, %method, suppressed, "refused a request without a valid access token");
        } else {
            tracing::debug!(?denied, %method, "refused a request without a valid access token");
        }
    }

    fn body_refused(&self, suppressed: Option<u64>, status: StatusCode, message: &str) {
        if let Some(suppressed) = suppressed {
            tracing::warn!(status = status.as_u16(), %message, suppressed, "refused a request body");
        } else {
            tracing::debug!(status = status.as_u16(), %message, "refused a request body");
        }
    }
}

/// What every response from this port carries.
pub(crate) type ResponseBody = BoxBody<Bytes, Infallible>;

/// Routes requests to the MCP service of the token they present.
pub(crate) struct Gate {
    registry: Arc<Registry>,
    path: String,
    /// Each kind of refusal is rate-limited on its own, so a flood of one cannot hide the other.
    token_refusals: RefusalLog,
    body_refusals: RefusalLog,
    sink: Arc<dyn RefusalSink>,
}

impl Gate {
    pub(crate) fn new(registry: Arc<Registry>, path: String) -> Self {
        Self::with_sink(registry, path, Arc::new(Tracing))
    }

    /// A gate that writes its logged refusals to `sink`.
    pub(crate) fn with_sink(registry: Arc<Registry>, path: String, sink: Arc<dyn RefusalSink>) -> Self {
        Self {
            registry,
            path,
            token_refusals: RefusalLog::new(REFUSAL_WARN_INTERVAL),
            body_refusals: RefusalLog::new(REFUSAL_WARN_INTERVAL),
            sink,
        }
    }

    /// Answers one request.
    pub(crate) async fn handle<B>(&self, request: Request<B>) -> Response<ResponseBody>
    where
        B: Body + Send + 'static,
        B::Error: Display,
    {
        if request.uri().path() != self.path {
            return text(StatusCode::NOT_FOUND, format!("Not Found: the MCP endpoint is {}", self.path));
        }
        let endpoint = match self.registry.authorize(request.headers()) {
            Ok(endpoint) => endpoint,
            Err(denied) => {
                self.sink.token_refused(self.token_refusals.warn_now(), denied, request.method());
                return unauthorized(denied);
            }
        };
        let limits = self.registry.limits();
        let (parts, body) = request.into_parts();
        // Only a POST has a body rmcp reads. It is read here, after the token check (so a client without a token can't
        // make the port wait for anything) and in full before rmcp sees any of it, so a client that stalls or overruns
        // is cut off by a limit this port controls and describes.
        let (body, _admission) = if parts.method == Method::POST {
            let bytes = match read_body(body, limits.max_body_bytes, limits.body_timeout).await {
                Ok(bytes) => bytes,
                Err(refusal) => return refuse_body(&refusal, limits, &self.body_refusals, self.sink.as_ref()),
            };
            // Held until rmcp has answered, by which time a session it opened is in its table.
            let admission = if may_open_session(&parts.headers, &bytes) {
                match endpoint.admit_session().await {
                    Ok(admission) => Some(admission),
                    Err(full) => return sessions_full(&full, limits),
                }
            } else {
                None
            };
            (bytes, admission)
        } else {
            (Bytes::new(), None)
        };
        endpoint.service().handle(Request::from_parts(parts, Full::new(body))).await
    }
}

/// 401 with the `WWW-Authenticate` challenge RFC 6750 asks for.
fn unauthorized(denied: Denied) -> Response<ResponseBody> {
    let (challenge, message) = match denied {
        Denied::Missing => (
            r#"Bearer realm="omuzaki-mcp""#,
            "Unauthorized: send the access token the user gave you as `Authorization: Bearer <token>`",
        ),
        Denied::Unknown => (
            r#"Bearer realm="omuzaki-mcp", error="invalid_token""#,
            "Unauthorized: that access token is not valid. It may have been revoked; ask the user for a new one",
        ),
    };
    let mut response = text(StatusCode::UNAUTHORIZED, message.to_owned());
    response.headers_mut().insert(WWW_AUTHENTICATE, HeaderValue::from_static(challenge));
    response
}

/// 408, 413 or 400 for a body that was not accepted, closing the connection: what is left of the body is not worth
/// reading, and the client has shown it cannot be trusted to finish it.
fn refuse_body(
    refusal: &BodyRefusal,
    limits: &Limits,
    log: &RefusalLog,
    sink: &dyn RefusalSink,
) -> Response<ResponseBody> {
    let (status, message) = match refusal {
        BodyRefusal::TooLarge => (
            StatusCode::PAYLOAD_TOO_LARGE,
            format!("Payload Too Large: the request body is over max_body_bytes = {} bytes", limits.max_body_bytes),
        ),
        BodyRefusal::TimedOut => (
            StatusCode::REQUEST_TIMEOUT,
            format!(
                "Request Timeout: the whole request body must arrive within body_timeout = {:?}",
                limits.body_timeout
            ),
        ),
        BodyRefusal::Unreadable(cause) => {
            (StatusCode::BAD_REQUEST, format!("Bad Request: the request body could not be read: {cause}"))
        }
    };
    sink.body_refused(log.warn_now(), status, &message);
    let mut response = text(status, message);
    response.headers_mut().insert(CONNECTION, HeaderValue::from_static("close"));
    response
}

/// 429 for a token that already has as many sessions as it may.
fn sessions_full(full: &SessionsFull, limits: &Limits) -> Response<ResponseBody> {
    text(
        StatusCode::TOO_MANY_REQUESTS,
        format!(
            "Too Many Requests: this access token already has {} open sessions, and max_sessions_per_token = {}. \
             Existing sessions were left running. End one with DELETE and its Mcp-Session-Id, or wait for a quiet one \
             to be closed after idle_timeout = {:?}",
            full.open, full.max, limits.idle_timeout
        ),
    )
}

fn text(status: StatusCode, message: String) -> Response<ResponseBody> {
    let mut response = Response::new(Full::new(Bytes::from(message)).boxed());
    *response.status_mut() = status;
    response.headers_mut().insert(CONTENT_TYPE, hyper::header::HeaderValue::from_static("text/plain; charset=utf-8"));
    response
}

#[cfg(test)]
mod tests;
