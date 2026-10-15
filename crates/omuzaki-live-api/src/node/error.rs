//! What can go wrong in a node: [`NodeError`] (a misuse or a fault, never an expected failure, which fires `failed`
//! instead) and [`ParamErrors`] (parameters a kind refuses).

use std::fmt;

use omuzaki_l10n_api::Text;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Input;
use crate::l10n;
use crate::value::{PortType, ValError};

/// The class of a [`NodeError`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NodeErrorCode {
    /// A port the node doesn't have, or one used the wrong way (firing a value output).
    UnknownPort,
    /// A value of the wrong type, or one that breaks a value rule.
    Type,
    /// An engine limit (design D8).
    Limit,
    /// A port read as a live output that isn't one.
    NotLive,
    /// An [`Input`] the node doesn't handle: one added after it was written (design D21). The engine shows it as a
    /// fault to fix; the harness fails the test.
    Unhandled,
    /// A fault in the node or its host.
    Internal,
}

/// A misuse or a fault inside a node. The engine drops that token's branch, counts it and shows it; the node keeps
/// running (design D5). An expected failure (a bad value, an op refused) fires the node's `failed` output instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct NodeError {
    /// The class.
    pub code: NodeErrorCode,
    /// What happened, for people.
    pub text: Text,
}

impl NodeError {
    /// A port the node doesn't have.
    pub fn unknown_port(port: &str) -> Self {
        Self { code: NodeErrorCode::UnknownPort, text: l10n::unknown_port(port) }
    }

    /// A value of type `found` where `port` takes `expected`.
    pub fn type_mismatch(port: &str, expected: PortType, found: PortType) -> Self {
        Self { code: NodeErrorCode::Type, text: l10n::type_mismatch(port, expected.as_str(), found.as_str()) }
    }

    /// A value for `port` that breaks a value rule.
    pub fn invalid_value(port: &str, error: &ValError) -> Self {
        Self { code: NodeErrorCode::Type, text: l10n::value_invalid(port, error.to_string()) }
    }

    /// A number `what` that isn't finite.
    pub fn not_finite(what: &str) -> Self {
        Self { code: NodeErrorCode::Type, text: l10n::not_finite(what) }
    }

    /// The limit named `limit` (a constant of [`limits`](crate::limits)) of `max` was reached.
    pub fn limit(limit: &'static str, max: usize) -> Self {
        Self { code: NodeErrorCode::Limit, text: l10n::limit(limit, i64::try_from(max).unwrap_or(i64::MAX)) }
    }

    /// `port` isn't a live output.
    pub fn not_live(port: &str) -> Self {
        Self { code: NodeErrorCode::NotLive, text: l10n::not_live(port) }
    }

    /// The node doesn't handle `input`: its wildcard arm (design D21).
    pub fn unhandled(input: &Input<'_>) -> Self {
        Self { code: NodeErrorCode::Unhandled, text: l10n::unhandled(input.name()) }
    }

    /// A variable or group `name` the show doesn't declare. The engine keeps a node that names one inert, so a
    /// running node meets this only through a host's misuse.
    pub fn undeclared(name: &str) -> Self {
        Self { code: NodeErrorCode::Internal, text: l10n::undeclared(name) }
    }

    /// An emitted payload for `kind` that doesn't fit it.
    pub fn invalid_payload(kind: &str, reason: impl Into<String>) -> Self {
        Self { code: NodeErrorCode::Type, text: l10n::payload_invalid(kind, reason) }
    }

    /// A fault, with the reason.
    pub fn internal(reason: impl Into<String>) -> Self {
        Self { code: NodeErrorCode::Internal, text: l10n::internal(reason) }
    }
}

impl fmt::Display for NodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.text, f)
    }
}

impl std::error::Error for NodeError {}

/// One thing wrong with a node's parameters, at a path into them (`where.reward_title`, `ports.out`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ParamError {
    /// Where, as dotted keys from the parameters' root; empty for the whole.
    pub path: String,
    /// What is wrong, for people.
    pub text: Text,
}

impl ParamError {
    /// A problem at `path`.
    pub fn new(path: impl Into<String>, text: Text) -> Self {
        Self { path: path.into(), text }
    }
}

/// Everything wrong with a node's parameters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ParamErrors(pub Vec<ParamError>);

impl ParamErrors {
    /// One problem.
    pub fn one(path: impl Into<String>, text: Text) -> Self {
        Self(vec![ParamError::new(path, text)])
    }
}

impl fmt::Display for ParamErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, error) in self.0.iter().enumerate() {
            let separator = if i == 0 { "" } else { "; " };
            match error.path.as_str() {
                "" => write!(f, "{separator}{}", error.text)?,
                path => write!(f, "{separator}{path}: {}", error.text)?,
            }
        }
        Ok(())
    }
}

impl std::error::Error for ParamErrors {}
