//! What a run gives back (D2, D9): [`Artifact`]s the host stores, text and JSON answers the caller uses at once, the
//! [`Usage`] that was billed, and [`ConnectError`], which says what to do and can never hold a key.

use omuzaki_http_api::{HttpError, HttpResponse};
use omuzaki_store_api::Commercial;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::id::MediaKind;

/// One generated file. The host stores the bytes with provenance; the adapter never does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    /// The file's bytes.
    pub bytes: Vec<u8>,
    /// Its media type (`image/png`).
    pub media_type: String,
    /// Its kind; one the capability allows.
    pub kind: MediaKind,
    /// A file name, when the vendor gives one.
    pub name: Option<String>,
    /// The model as the vendor names it, `<id>@<version>` when it says (provenance's `model`).
    pub model: String,
    /// The seed actually used (provenance records it).
    pub seed: Option<u64>,
    /// Only for a manifest with `outputs = reported`: the licence of the model that made this, from the registry.
    pub rights: Option<ReportedRights>,
}

/// The licence of a model an adapter ran, as the registry records it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReportedRights {
    /// SPDX or `LicenseRef-...`.
    pub spdx: String,
    /// Its commercial status.
    pub commercial: Commercial,
}

/// What a call used. `usd_micros` is what the vendor billed, or the adapter's own count when it does not say; it equals
/// the sum the run reported through its [`Meter`](crate::Meter).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    /// Input tokens (LLM and vision): input plus cache reads and writes.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Money, in micro-dollars.
    pub usd_micros: u64,
}

/// One output of a run. Bytes become candidates; text and JSON go back to the caller and never reach the store (D2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Output {
    /// A file, stored and trayed by the host.
    Asset(Artifact),
    /// A text answer.
    Text(String),
    /// A JSON answer.
    Json(Value),
}

/// What [`Connector::run`](crate::Connector::run) returns when anything came back. `stopped` says why it ended early:
/// cancelled, cap reached, or the vendor's error after some outputs were complete. Complete outputs are kept and
/// charged; an `Err` from `run` means no output at all.
#[derive(Clone, Debug, PartialEq)]
pub struct RunOutput {
    /// The complete outputs, in order.
    pub outputs: Vec<Output>,
    /// What was used.
    pub usage: Usage,
    /// Why the run ended early, if it did.
    pub stopped: Option<ConnectError>,
}

/// Why a connector call failed. The `Display` says what to do; no variant has a field for a key, and the host runs
/// every string through its scrubber anyway (D8).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "error", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ConnectError {
    /// The caller cancelled.
    #[error("the call was cancelled")]
    Cancelled,
    /// The service is not running or not reachable.
    #[error("the service is unavailable: {reason}")]
    #[non_exhaustive]
    Unavailable {
        /// What to start or fix.
        reason: String,
    },
    /// The key is missing or was refused.
    #[error("the service refused the key: {hint}")]
    #[non_exhaustive]
    Auth {
        /// What to do, never the key.
        hint: String,
    },
    /// The service is throttling.
    #[error("the service is rate limiting; {}", retry_hint(*retry_after_s))]
    #[non_exhaustive]
    RateLimited {
        /// Seconds it asked to wait.
        retry_after_s: Option<u32>,
    },
    /// The vendor declined the content.
    #[error("the service declined the request: {reason}; change the prompt or inputs")]
    #[non_exhaustive]
    Refused {
        /// The vendor's reason.
        reason: String,
    },
    /// The next billable request would pass the call's cap.
    #[error(
        "the next request would cost {} in all, above this call's cap of {}; raise the cap or ask for less",
        usd(*needed_usd_micros),
        usd(*cap_usd_micros)
    )]
    #[non_exhaustive]
    CapReached {
        /// The cap, micro-dollars.
        cap_usd_micros: u64,
        /// What the call would have cost with the next request, micro-dollars.
        needed_usd_micros: u64,
    },
    /// The service did not answer in time.
    #[error("the service did not answer within {after_s} s; try again later")]
    #[non_exhaustive]
    Timeout {
        /// The timeout, seconds.
        after_s: u32,
    },
    /// The answer is not what the connector expects.
    #[error("the service answered something unexpected: {what}")]
    #[non_exhaustive]
    BadResponse {
        /// What was wrong.
        what: String,
    },
    /// The connector cannot do this.
    #[error("not supported: {what}")]
    #[non_exhaustive]
    Unsupported {
        /// What it cannot do.
        what: String,
    },
    /// Anything else.
    #[error("the call failed: {message}")]
    #[non_exhaustive]
    Failed {
        /// What happened.
        message: String,
    },
}

impl ConnectError {
    /// [`ConnectError::Unavailable`].
    pub fn unavailable(reason: impl Into<String>) -> Self {
        Self::Unavailable { reason: reason.into() }
    }

    /// [`ConnectError::Auth`].
    pub fn auth(hint: impl Into<String>) -> Self {
        Self::Auth { hint: hint.into() }
    }

    /// [`ConnectError::RateLimited`].
    pub fn rate_limited(retry_after_s: Option<u32>) -> Self {
        Self::RateLimited { retry_after_s }
    }

    /// [`ConnectError::Refused`].
    pub fn refused(reason: impl Into<String>) -> Self {
        Self::Refused { reason: reason.into() }
    }

    /// [`ConnectError::CapReached`].
    pub fn cap_reached(cap_usd_micros: u64, needed_usd_micros: u64) -> Self {
        Self::CapReached { cap_usd_micros, needed_usd_micros }
    }

    /// [`ConnectError::Timeout`].
    pub fn timeout(after_s: u32) -> Self {
        Self::Timeout { after_s }
    }

    /// [`ConnectError::BadResponse`].
    pub fn bad_response(what: impl Into<String>) -> Self {
        Self::BadResponse { what: what.into() }
    }

    /// [`ConnectError::Unsupported`].
    pub fn unsupported(what: impl Into<String>) -> Self {
        Self::Unsupported { what: what.into() }
    }

    /// [`ConnectError::Failed`].
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed { message: message.into() }
    }

    /// The error for a transport failure of a request sent with `timeout_s`: the mapping every HTTP adapter shares
    /// and the conformance suite checks. Cancelled stays cancelled, a timeout names it, an answer larger than allowed
    /// is a bad response, a refused or failed connection says the service is unavailable.
    pub fn from_http(e: &HttpError, timeout_s: u32) -> Self {
        match e {
            HttpError::Cancelled => Self::Cancelled,
            HttpError::Timeout => Self::timeout(timeout_s),
            HttpError::TooLarge { limit } => Self::bad_response(format!("the answer is larger than {limit} bytes")),
            HttpError::Network(why) => Self::unavailable(format!("{why}; check that the service is running")),
            HttpError::Refused(why) => Self::unavailable(format!("the request was not sent: {why}")),
        }
    }

    /// The error for a non-2xx answer, or `None` for a 2xx: 401 and 403 are [`Auth`](Self::Auth), 429
    /// [`RateLimited`](Self::RateLimited) with its `retry-after` seconds, anything else [`Failed`](Self::Failed) with
    /// the status. The body is never copied in: it may echo the key.
    pub fn from_status(response: &HttpResponse) -> Option<Self> {
        let status = response.status;
        Some(match status {
            200..=299 => return None,
            401 | 403 => Self::auth(format!("status {status}: check the key in the connector's settings")),
            429 => {
                let retry = response
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                    .and_then(|(_, v)| v.trim().parse().ok());
                Self::rate_limited(retry)
            }
            _ => Self::failed(format!("the service answered status {status}")),
        })
    }
}

fn retry_hint(retry_after_s: Option<u32>) -> String {
    match retry_after_s {
        Some(s) => format!("try again in {s} s"),
        None => "try again later".to_owned(),
    }
}

/// Micro-dollars as dollars: `$0.04`, `$0.0153`, `$1.00`. At least two decimals, more only when needed, so sums stay
/// exact on screen.
pub fn usd(micros: u64) -> String {
    let (whole, frac) = (micros / 1_000_000, micros % 1_000_000);
    let digits = format!("{frac:06}");
    let kept = digits.trim_end_matches('0');
    let decimals = if kept.len() < 2 { &digits[..2] } else { kept };
    format!("${whole}.{decimals}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollars_are_exact() {
        assert_eq!(usd(40_000), "$0.04");
        assert_eq!(usd(15_300), "$0.0153");
        assert_eq!(usd(1_000_000), "$1.00");
        assert_eq!(usd(1), "$0.000001");
        assert_eq!(usd(0), "$0.00");
        assert_eq!(usd(12_345_670_000), "$12345.67");
    }

    #[test]
    fn statuses_map_to_errors() {
        let r = |status, headers: Vec<(String, String)>| HttpResponse { status, headers, body: b"sk-secret".to_vec() };
        assert_eq!(ConnectError::from_status(&r(204, vec![])), None);
        assert!(matches!(ConnectError::from_status(&r(401, vec![])), Some(ConnectError::Auth { .. })));
        let limited = ConnectError::from_status(&r(429, vec![("Retry-After".into(), "7".into())]));
        assert_eq!(limited, Some(ConnectError::rate_limited(Some(7))));
        let failed = ConnectError::from_status(&r(500, vec![])).unwrap();
        assert!(matches!(failed, ConnectError::Failed { .. }) && !failed.to_string().contains("sk-secret"));
        assert_eq!(ConnectError::from_http(&HttpError::Timeout, 30), ConnectError::timeout(30));
        assert!(matches!(
            ConnectError::from_http(&HttpError::TooLarge { limit: 9 }, 30),
            ConnectError::BadResponse { .. }
        ));
    }

    #[test]
    fn display_says_what_to_do() {
        let e = ConnectError::cap_reached(100_000, 120_000);
        assert_eq!(
            e.to_string(),
            "the next request would cost $0.12 in all, above this call's cap of $0.10; raise the cap or ask for less"
        );
        assert_eq!(ConnectError::rate_limited(None).to_string(), "the service is rate limiting; try again later");
    }
}
