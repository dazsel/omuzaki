//! Why a request about a plugin failed.

use std::borrow::Cow;

use omuzaki_ops_api::{ErrorCode, ErrorDetail, OpError, Scope};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::FaultKind;

/// Why a request about a plugin failed. Converts to [`OpError`] for the `plugin.*` ops.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PluginError {
    /// The manifest is malformed or disagrees with the component.
    #[error("manifest: {0}")]
    Manifest(String),
    /// The component imports something the host does not provide: a WASI interface outside the stub subset, or
    /// anything else (design §4.1).
    #[error("the plugin imports {}, which this host does not provide", unexpected.join(", "))]
    #[non_exhaustive]
    Imports {
        /// The imports the host refuses, sorted.
        unexpected: Vec<String>,
    },
    /// The component targets a WIT version outside this host's window.
    #[error("the plugin was built for omuzaki:plugin {built_for}; this host serves {}", host.join(" and "))]
    #[non_exhaustive]
    Incompatible {
        /// The version its imports name.
        built_for: String,
        /// The versions this host serves, newest first.
        host: Vec<String>,
    },
    /// The package's signature does not verify, or its key is revoked.
    #[error("signature: {0}")]
    Signature(String),
    /// The package itself is refused: a hostile archive, a missing file, a size cap.
    #[error("package: {0}")]
    Package(String),
    /// Reading or writing the plugin's files failed.
    #[error("files: {0}")]
    Io(String),
    /// The component could not be instantiated.
    #[error("could not start the plugin: {0}")]
    Instantiate(String),
    /// The plugin faulted.
    #[error("the plugin faulted: {0:?}")]
    Fault(FaultKind),
    /// A scope is missing or not approved.
    #[error("the plugin lacks the scope `{scope:?}`")]
    #[non_exhaustive]
    Denied {
        /// The scope it lacks.
        scope: Scope,
    },
    /// A limit was hit.
    #[error("the plugin hit a limit: {what}")]
    #[non_exhaustive]
    Limit {
        /// Which limit, named from the constant that sets it.
        what: Cow<'static, str>,
    },
    /// No such plugin.
    #[error("no such plugin")]
    NotFound,
    /// The request was cancelled.
    #[error("the request was cancelled")]
    Cancelled,
}

impl PluginError {
    /// Imports the host does not provide; sorted so the message is stable.
    pub fn imports(unexpected: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut unexpected: Vec<String> = unexpected.into_iter().map(Into::into).collect();
        unexpected.sort();
        unexpected.dedup();
        Self::Imports { unexpected }
    }

    /// A component built for `built_for` on a host serving `host`.
    pub fn incompatible(built_for: impl Into<String>, host: Vec<String>) -> Self {
        Self::Incompatible { built_for: built_for.into(), host }
    }

    /// A missing or unapproved scope.
    pub fn denied(scope: Scope) -> Self {
        Self::Denied { scope }
    }

    /// A limit named `what`.
    pub fn limit(what: impl Into<Cow<'static, str>>) -> Self {
        Self::Limit { what: what.into() }
    }
}

impl From<PluginError> for OpError {
    fn from(error: PluginError) -> Self {
        let message = error.to_string();
        let plain = |code| OpError { code, message: message.clone(), field: None, hint: None, detail: None };
        match error {
            PluginError::Manifest(_) => OpError::invalid("manifest", message),
            PluginError::Package(_) => OpError::invalid("package", message),
            PluginError::Signature(_) => OpError::invalid("signature", message),
            PluginError::Incompatible { .. } => plain(ErrorCode::Unsupported),
            PluginError::Io(_) => plain(ErrorCode::Unavailable),
            PluginError::Denied { scope } => OpError::denied(message).with_detail(ErrorDetail::MissingScope { scope }),
            PluginError::Limit { .. } => plain(ErrorCode::Limited),
            PluginError::NotFound => OpError::not_found(message),
            PluginError::Cancelled => plain(ErrorCode::Cancelled),
            PluginError::Imports { .. } | PluginError::Instantiate(_) | PluginError::Fault(_) => {
                OpError::internal(message)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all() -> Vec<PluginError> {
        vec![
            PluginError::Manifest("no id".into()),
            PluginError::imports(["wasi:sockets/tcp"]),
            PluginError::incompatible("0.1", vec!["0.3".into(), "0.2".into()]),
            PluginError::Signature("mismatch".into()),
            PluginError::Package("`../x` leaves the package".into()),
            PluginError::Io("disk full".into()),
            PluginError::Instantiate("bad magic".into()),
            PluginError::Fault(FaultKind::Timeout),
            PluginError::denied(Scope::Perform),
            PluginError::limit("instances"),
            PluginError::NotFound,
            PluginError::Cancelled,
        ]
    }

    #[test]
    fn every_variant_round_trips() {
        for error in all() {
            let json = serde_json::to_string(&error).unwrap();
            assert_eq!(serde_json::from_str::<PluginError>(&json).unwrap(), error, "{json}");
        }
        assert!(serde_json::to_value(schemars::schema_for!(PluginError)).unwrap().is_object());
    }

    #[test]
    fn every_variant_maps_to_its_op_error() {
        let codes: Vec<ErrorCode> = all().into_iter().map(|e| OpError::from(e).code).collect();
        assert_eq!(
            codes,
            [
                ErrorCode::Invalid,
                ErrorCode::Internal,
                ErrorCode::Unsupported,
                ErrorCode::Invalid,
                ErrorCode::Invalid,
                ErrorCode::Unavailable,
                ErrorCode::Internal,
                ErrorCode::Internal,
                ErrorCode::Denied,
                ErrorCode::Limited,
                ErrorCode::NotFound,
                ErrorCode::Cancelled,
            ]
        );
    }

    #[test]
    fn the_op_error_keeps_what_the_caller_needs() {
        let denied = OpError::from(PluginError::denied(Scope::Perform));
        assert_eq!(denied.detail.as_deref(), Some(&ErrorDetail::MissingScope { scope: Scope::Perform }));
        let manifest = OpError::from(PluginError::Manifest("no id".into()));
        assert_eq!(manifest.field.as_deref(), Some("manifest"));
        assert!(manifest.message.contains("no id"));
        assert!(OpError::from(PluginError::limit("instances")).message.contains("instances"));
    }
}
