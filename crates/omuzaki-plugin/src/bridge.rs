//! The seam to the op bridge (design §2): what `ops.call`, `plan`, `describe` and `list` reach. The bridge itself is
//! the op-bridge task's; until it lands every host links [`NoBridge`], so a plugin's op calls are refused cleanly.

use omuzaki_ops_api::{ErrorCode, OpError};
use wasmtime::component::{ComponentType, Lift, Lower};

/// What the `ops` imports call into: the plugin's session at the runner.
pub trait OpBridge: Send + Sync {
    /// Runs `op` with `args` (JSON) in the plugin's session (`Mode::Apply`).
    ///
    /// # Errors
    /// The op's error, or the executor's refusal.
    fn call(&self, op: &str, args: &str) -> Result<String, OpError>;

    /// Dry-runs `op` (`Mode::DryRun`); the plan as JSON.
    ///
    /// # Errors
    /// As [`call`](Self::call).
    fn plan(&self, op: &str, args: &str) -> Result<String, OpError>;

    /// The op's argument schema, or `None` for an unknown op.
    fn describe(&self, op: &str) -> Option<String>;

    /// The ops the plugin's ceiling could ever allow, sorted.
    fn list(&self) -> Vec<String>;
}

/// The bridge before the op-bridge task: every call is `unsupported`, `describe` knows nothing, `list` is empty.
pub struct NoBridge;

impl OpBridge for NoBridge {
    fn call(&self, op: &str, _: &str) -> Result<String, OpError> {
        Err(OpError::unsupported(format!("{op}: this host has no op bridge yet")))
    }

    fn plan(&self, op: &str, args: &str) -> Result<String, OpError> {
        self.call(op, args)
    }

    fn describe(&self, _: &str) -> Option<String> {
        None
    }

    fn list(&self) -> Vec<String> {
        Vec::new()
    }
}

/// The WIT `error-code`, `ErrorCode` one to one.
#[derive(ComponentType, Lift, Lower, Copy, Clone, Debug, PartialEq, Eq)]
#[component(enum)]
#[repr(u8)]
pub enum WitErrorCode {
    /// `invalid`.
    #[component(name = "invalid")]
    Invalid,
    /// `not-found`.
    #[component(name = "not-found")]
    NotFound,
    /// `denied`.
    #[component(name = "denied")]
    Denied,
    /// `conflict`.
    #[component(name = "conflict")]
    Conflict,
    /// `unsupported`.
    #[component(name = "unsupported")]
    Unsupported,
    /// `unavailable`.
    #[component(name = "unavailable")]
    Unavailable,
    /// `internal`.
    #[component(name = "internal")]
    Internal,
    /// `waiting`.
    #[component(name = "waiting")]
    Waiting,
    /// `limited`.
    #[component(name = "limited")]
    Limited,
    /// `cancelled`.
    #[component(name = "cancelled")]
    Cancelled,
}

/// The WIT `op-error`: `OpError` one to one, `detail` as JSON.
#[derive(ComponentType, Lift, Lower, Clone, Debug, PartialEq, Eq)]
#[component(record)]
pub struct WitOpError {
    /// The class.
    pub code: WitErrorCode,
    /// The English message.
    pub message: String,
    /// `ErrorDetail` as JSON.
    pub detail: Option<String>,
    /// The hint.
    pub hint: Option<String>,
}

impl From<&OpError> for WitOpError {
    fn from(error: &OpError) -> Self {
        let code = match error.code {
            ErrorCode::Invalid => WitErrorCode::Invalid,
            ErrorCode::NotFound => WitErrorCode::NotFound,
            ErrorCode::Denied => WitErrorCode::Denied,
            ErrorCode::Conflict => WitErrorCode::Conflict,
            ErrorCode::Unsupported => WitErrorCode::Unsupported,
            ErrorCode::Unavailable => WitErrorCode::Unavailable,
            ErrorCode::Waiting => WitErrorCode::Waiting,
            ErrorCode::Limited => WitErrorCode::Limited,
            ErrorCode::Cancelled => WitErrorCode::Cancelled,
            _ => WitErrorCode::Internal,
        };
        let detail = error.detail.as_ref().and_then(|d| serde_json::to_string(d).ok());
        Self { code, message: error.message.clone(), detail, hint: error.hint.clone() }
    }
}
