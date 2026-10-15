//! Why a request about a plugin failed.

use std::borrow::Cow;

use omuzaki_ops_api::{ErrorCode, ErrorDetail, OpError, Scope};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::FaultKind;

/// Why a request about a plugin failed. Converts to [`OpError`] for the `plugin.*` ops.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PluginError {
    /// The manifest is malformed or disagrees with the component.
    #[error("manifest: {0}")]
    Manifest(String),
    /// The component imports something the host does not provide.
    #[error("the plugin imports `{name}`, which this host does not provide")]
    #[non_exhaustive]
    Import {
        /// The import's name.
        name: String,
    },
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
    /// An import the host does not provide.
    pub fn import(name: impl Into<String>) -> Self {
        Self::Import { name: name.into() }
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
            PluginError::Denied { scope } => OpError::denied(message).with_detail(ErrorDetail::MissingScope { scope }),
            PluginError::Limit { .. } => plain(ErrorCode::Limited),
            PluginError::NotFound => OpError::not_found(message),
            PluginError::Cancelled => plain(ErrorCode::Cancelled),
            PluginError::Import { .. } | PluginError::Instantiate(_) | PluginError::Fault(_) => {
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
            PluginError::import("wasi:sockets/tcp"),
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
