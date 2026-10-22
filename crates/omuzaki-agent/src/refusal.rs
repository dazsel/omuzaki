//! What `omuzaki-mcp` says when it refuses a call, read from the structured result (D2.6).
//!
//! The server renders an error as `structuredContent { ok: false, code, message, hint, detail? }`, where `detail` is the
//! op layer's `ErrorDetail` as JSON. The host branches on the code and the detail; the text is for the model.

use omuzaki_ops_api::{ApprovalId, EndReason, ErrorDetail, Need};
use serde_json::Value;

use crate::link::ToolReply;

/// A refusal the host acts on.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Refusal {
    /// Nothing ran; the request waits for a person.
    Queued(ApprovalId, Need),
    /// The session is paused; nothing ran.
    Paused,
    /// A rate limit; retry after this many seconds.
    RetryAfter(f64),
    /// The session has ended.
    Ended(EndReason),
}

/// The refusal in `reply`, if it is one the host acts on. `Denied`, `Conflict` and every other failure go to the model
/// as returned.
pub(crate) fn classify(reply: &ToolReply) -> Option<Refusal> {
    let data = reply.structured.as_ref()?;
    let code = data.get("code").and_then(Value::as_str)?;
    let detail: ErrorDetail = serde_json::from_value(data.get("detail")?.clone()).ok()?;
    match (code, detail) {
        ("waiting", ErrorDetail::Queued { approval, need }) => Some(Refusal::Queued(approval, need)),
        ("waiting", ErrorDetail::Paused { .. }) => Some(Refusal::Paused),
        ("limited", ErrorDetail::RetryAfter { seconds, .. }) => Some(Refusal::RetryAfter(f64::from(seconds))),
        (_, ErrorDetail::Ended { reason }) => Some(Refusal::Ended(reason)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::{Actor, Need, PauseReason};
    use serde_json::json;

    use super::*;

    fn reply(code: &str, detail: &ErrorDetail) -> ToolReply {
        let data = json!({"ok": false, "code": code, "message": "m", "hint": "h", "detail": detail});
        ToolReply { blocks: vec![], is_error: true, structured: Some(data) }
    }

    #[test]
    fn each_refusal_the_host_acts_on_is_read_from_the_detail() {
        let queued = ErrorDetail::Queued { approval: ApprovalId(7), need: Need::Person { reason: "r".into() } };
        assert_eq!(
            classify(&reply("waiting", &queued)),
            Some(Refusal::Queued(ApprovalId(7), Need::Person { reason: "r".into() }))
        );
        let paused = ErrorDetail::Paused { by: Actor::System, reason: PauseReason::Person };
        assert_eq!(classify(&reply("waiting", &paused)), Some(Refusal::Paused));
        let limited = ErrorDetail::RetryAfter { seconds: 1.5, limit: "edits_per_minute=600".into() };
        assert_eq!(classify(&reply("limited", &limited)), Some(Refusal::RetryAfter(1.5)));
        let ended = ErrorDetail::Ended { reason: EndReason::Closed };
        assert_eq!(classify(&reply("denied", &ended)), Some(Refusal::Ended(EndReason::Closed)));
    }

    #[test]
    fn denied_and_conflict_and_plain_results_are_not_refusals() {
        let changed = json!({"ok": false, "code": "conflict", "message": "m", "detail": {"kind": "no_session"}});
        assert_eq!(classify(&ToolReply { blocks: vec![], is_error: true, structured: Some(changed) }), None);
        assert_eq!(
            classify(&ToolReply { blocks: vec![], is_error: false, structured: Some(json!({"ok": true})) }),
            None
        );
        assert_eq!(classify(&ToolReply { blocks: vec![], is_error: false, structured: None }), None);
    }
}
