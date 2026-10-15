//! The `perform.*` ops (PF-20, PF-21): calibrate the performer's neutral face, tune the retarget's dials and read the
//! performer's status, all through the [`PerformHost`] service so the UI, MCP and agents share one path.

use omuzaki_ops_api::{OpCtx, OpError};

use crate::perform::host::{PerformError, PerformHost};

pub mod calibrate;
pub mod set_retarget;
pub mod status;

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
        PerformError::Stalled => OpError::unavailable("PerformHost")
            .hint("the performer is not stepping; the change stays queued and applies when it runs (`perform.status` shows it)"),
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
        ];
        for (error, code) in cases {
            let mapped = op_error(error);
            assert_eq!(mapped.code, code);
            assert!(mapped.hint.is_some(), "{error:?} has no hint");
        }
    }
}
