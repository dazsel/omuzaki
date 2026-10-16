//! The `perform.*` ops (PF-20, PF-21): calibrate the performer's neutral face, tune the retarget's dials and read the
//! performer's status, all through the [`PerformHost`] service so the UI, MCP and agents share one path.

use omuzaki_ops_api::{OpCtx, OpError};

use crate::perform::host::{PerformError, PerformHost};
use crate::perform::triggers::MAX_KEY_CHARS;

pub mod calibrate;
pub mod set_retarget;
pub mod status;
pub mod triggers;

/// The performer behind the ops, or `Unavailable` when the app did not wire one in.
fn host<'a>(ctx: &OpCtx<'a>) -> Result<&'a dyn PerformHost, OpError> {
    ctx.service::<dyn PerformHost>()
}

/// What each refusal means to a caller of an op.
fn op_error(error: PerformError) -> OpError {
    match error {
        PerformError::CalibrationRunning => OpError::conflict(error.to_string())
            .hint("wait for the running calibration to finish (`perform.status` shows it), then ask again"),
        PerformError::QueueFull => OpError::conflict(error.to_string()).hint("ask again in a moment"),
        PerformError::Disconnected => OpError::unavailable("PerformHost").hint("start the performer, then ask again"),
        PerformError::Unsupported => OpError::unsupported(error.to_string()).hint("use the desktop app to perform live"),
        PerformError::Stalled => OpError::unavailable("PerformHost")
            .hint("the performer is not stepping; the change stays queued and applies when it runs (`perform.status` shows it)"),
        PerformError::TooManyTriggers => {
            OpError::conflict(error.to_string()).hint("unbind a key first (`trigger.list` shows the bound keys)")
        }
        PerformError::UnknownKey => {
            OpError::not_found(error.to_string()).hint("check the spelling with `trigger.list`, or bind the key first")
        }
        PerformError::InvalidKey => OpError::invalid("key", error.to_string())
            .hint(format!("use 1 to {MAX_KEY_CHARS} printable characters, such as `F1`")),
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::ErrorCode;

    use super::*;

    #[test]
    fn every_refusal_maps_to_a_code_with_a_hint() {
        let cases = [
            (PerformError::CalibrationRunning, ErrorCode::Conflict),
            (PerformError::QueueFull, ErrorCode::Conflict),
            (PerformError::Disconnected, ErrorCode::Unavailable),
            (PerformError::Stalled, ErrorCode::Unavailable),
            (PerformError::Unsupported, ErrorCode::Unsupported),
            (PerformError::TooManyTriggers, ErrorCode::Conflict),
            (PerformError::UnknownKey, ErrorCode::NotFound),
            (PerformError::InvalidKey, ErrorCode::Invalid),
        ];
        for (error, code) in cases {
            let mapped = op_error(error);
            assert_eq!(mapped.code, code);
            assert!(mapped.hint.is_some(), "{error:?} has no hint");
        }
    }
}
