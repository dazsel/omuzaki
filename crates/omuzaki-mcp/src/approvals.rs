//! The agent's own approvals: what it asked a person to allow, and what came of it (design `agent-sessions.md` §5.2,
//! §7). A request the executor queues comes back as `waiting` with an approval id; the person decides in Omuzaki's
//! window; the decision reaches the agent either through `approval_wait` or in the heads-up of its next result, once.
//!
//! Nothing here can approve anything: the only calls are reads of the registry and of the event feed.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_ops_api::{ApprovalId, ErrorDetail, Event, Need, OpError, OpKind, Resolution, Resolved, Sessions};
use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Value, json};

use crate::changes::{actor_label, plain};
use crate::events::EventHub;
use crate::render::{self, Call};

/// The longest comment of a person's rejection that is shown, in characters.
const COMMENT_CHARS: usize = 300;

/// What is remembered about one of this session's approvals, to word its outcome.
#[derive(Clone, Debug)]
pub(crate) struct Own {
    /// The tool the agent called (`export_vrm`), or `pairing` for the approval that pairs the client.
    pub tool: String,
    /// What kind of op it was, so an approved outcome renders like that op's own success. `None` for pairing.
    pub kind: Option<OpKind>,
}

/// This session's approvals.
#[derive(Default)]
pub(crate) struct OwnApprovals {
    known: BTreeMap<ApprovalId, Own>,
    /// What the session's own results (blocking calls, `tasks/get`, `approval_wait`) have delivered.
    cursor: ApprovalCursor,
}

/// How far one consumer has been told about decisions: the newest event looked at, and the approvals whose outcome it
/// has been given. Each consumer of the feed has its own (see the module docs of `events`).
#[derive(Clone, Default)]
pub(crate) struct ApprovalCursor {
    /// Approvals whose outcome this consumer has been told.
    reported: BTreeSet<ApprovalId>,
    /// The newest event already looked at for outcomes.
    scanned: u64,
}

impl ApprovalCursor {
    /// A cursor that starts at event `scanned`: only decisions after it are news.
    pub(crate) fn new(scanned: u64) -> Self {
        Self { scanned, ..Self::default() }
    }
}

impl OwnApprovals {
    /// Starts at the feed's current end: only decisions from now on are news.
    pub(crate) fn new(scanned: u64) -> Self {
        Self { cursor: ApprovalCursor::new(scanned), ..Self::default() }
    }

    /// Remembers an approval this session's refused call queued.
    pub(crate) fn remember(&mut self, id: ApprovalId, own: Own) {
        self.known.entry(id).or_insert(own);
    }

    /// Remembers the approval a refusal queued, if it is one.
    pub(crate) fn remember_queued(&mut self, error: &OpError, tool: &str, kind: OpKind) {
        if let Some(ErrorDetail::Queued { approval, .. }) = error.detail.as_deref() {
            self.remember(*approval, Own { tool: tool.to_owned(), kind: Some(kind) });
        }
    }

    /// A copy of the session's own cursor, for a consumer that starts from what the session has yet to be told.
    pub(crate) fn cursor(&self) -> ApprovalCursor {
        self.cursor.clone()
    }

    pub(crate) fn get(&self, id: ApprovalId) -> Option<&Own> {
        self.known.get(&id)
    }

    pub(crate) fn mark_reported(&mut self, id: ApprovalId) {
        self.cursor.reported.insert(id);
    }

    /// Remembers the approval a decision itself queued: an approved request that then failed because it needs another
    /// approval (`Resolved::Failed` carrying `Queued`). The agent is shown that approval, so it must be one it can
    /// wait on and hear about; it belongs to the same call as the one it came from.
    pub(crate) fn remember_chained(&mut self, resolution: &Resolution, from: &Own) {
        if let Resolved::Failed { error } = &resolution.result
            && let Some(ErrorDetail::Queued { approval, .. }) = error.detail.as_deref()
        {
            self.remember(*approval, from.clone());
        }
    }

    /// The decisions on this session's approvals that the session's own results have not delivered, oldest first,
    /// marking them delivered. Consuming: only results the agent receives call this.
    pub(crate) fn news(&mut self, hub: &EventHub) -> Vec<(Own, Resolution)> {
        let mut cursor = std::mem::take(&mut self.cursor);
        let out = self.news_for(hub, &mut cursor);
        self.cursor = cursor;
        out
    }

    /// As [`OwnApprovals::news`] for a consumer with a `cursor` of its own (a task's pushes): it moves that cursor and
    /// leaves the session's untouched, so reporting to one consumer never consumes what another has not seen.
    pub(crate) fn news_for(&mut self, hub: &EventHub, cursor: &mut ApprovalCursor) -> Vec<(Own, Resolution)> {
        let mut out = Vec::new();
        for (n, event) in hub.entries_after(cursor.scanned) {
            cursor.scanned = n;
            let Event::ApprovalResolved { resolution, .. } = event else { continue };
            let Some(own) = self.known.get(&resolution.id).cloned() else { continue };
            // Whether or not it is news to this consumer, what it queued must be tracked.
            self.remember_chained(&resolution, &own);
            if cursor.reported.insert(resolution.id) {
                out.push((own, resolution));
            }
        }
        out
    }
}

/// Finds the decision on `id` in the feed, if it has arrived.
pub(crate) fn resolution_of(hub: &EventHub, id: ApprovalId) -> Option<Resolution> {
    hub.entries_after(0).into_iter().find_map(|(_, event)| match event {
        Event::ApprovalResolved { resolution, .. } if resolution.id == id => Some(resolution),
        _ => None,
    })
}

/// Whether `id` is still waiting for a person, according to the registry.
pub(crate) fn is_pending(sessions: &dyn Sessions, id: ApprovalId) -> bool {
    sessions.approvals().iter().any(|a| a.id == id)
}

/// What the person is being asked, in a few words: the end of "approval #N asks the person to ...".
pub(crate) fn need_text(need: &Need) -> String {
    match need {
        Need::Scope { scope } => format!("grant this session the `{scope}` scope for this request"),
        Need::Person { reason } => format!("confirm: {}", plain(reason, 200)),
        Need::Budget { .. } => "raise this session's budget, or approve this one request".to_owned(),
        Need::Pairing { client } => format!("pair the client {} with this project", plain(client, 60)),
        Need::Review { policy } => format!("review it under the policy {}", plain(policy, 60)),
    }
}

/// The outcome of an approval as a tool result: what `approval_wait` returns, and what the heads-up summarises.
///
/// An approved request that ran renders like the op's own success; a rejection carries the person's comment, as data
/// (it is text a person typed, cut and flattened to one line).
pub(crate) fn outcome(own: &Own, resolution: &Resolution, verify: Option<&str>) -> CallToolResult {
    let id = resolution.id.0;
    let by = resolution.by.as_ref().map_or_else(|| "a person".to_owned(), actor_label);
    let (mut result, kind, line) = match &resolution.result {
        Resolved::Applied { outcome } => match own.kind {
            Some(kind) => {
                let call = Call { op: &own.tool, kind, mode: omuzaki_ops_api::Mode::Apply, tx: None, verify };
                (render::success(&call, outcome), "approved", format!("Approved by {by}; the request ran."))
            }
            None => (
                render::confirmation(String::new(), json!({"ok": true})),
                "approved",
                format!("Approved by {by}. The session can go on."),
            ),
        },
        Resolved::Failed { error } => (
            render::failure(&own.tool, error, None),
            "failed",
            format!("Approved by {by}, but the request then failed. Nothing changed."),
        ),
        Resolved::Rejected { comment } => {
            let said = comment.as_deref().map(|c| plain(c, COMMENT_CHARS)).filter(|c| !c.is_empty());
            let line = match said {
                Some(comment) => format!("The person rejected this: '{comment}'. Nothing ran."),
                None => "The person rejected this. Nothing ran.".to_owned(),
            };
            (
                render::confirmation(
                    String::new(),
                    json!({"ok": false, "comment": comment.as_deref().map(|c| plain(c, COMMENT_CHARS))}),
                ),
                "rejected",
                line,
            )
        }
        Resolved::Withdrawn { reason } => (
            render::confirmation(String::new(), json!({"ok": false, "reason": format!("{reason:?}")})),
            "withdrawn",
            format!("The approval was withdrawn ({}); nothing ran.", withdrawn_text(reason)),
        ),
    };
    let ran = kind == "approved";
    result = prefixed(result, &format!("Approval #{id}: {line}"));
    if !ran {
        result.is_error = Some(true);
    }
    if let Some(Value::Object(data)) = result.structured_content.as_mut() {
        data.insert("approval".to_owned(), json!(id));
        data.insert("decision".to_owned(), json!(kind));
        data.entry("op").or_insert_with(|| json!(own.tool));
    }
    result
}

/// `line` in front of the result's first text block, which for most results is its headline.
fn prefixed(mut result: CallToolResult, line: &str) -> CallToolResult {
    match result.content.first().and_then(ContentBlock::as_text).map(|t| t.text.clone()) {
        Some(first) if !first.is_empty() => result.content[0] = ContentBlock::text(format!("{line}\n{first}")),
        Some(_) => result.content[0] = ContentBlock::text(line),
        None => result.content.insert(0, ContentBlock::text(line)),
    }
    result
}

pub(crate) fn withdrawn_text(reason: &omuzaki_ops_api::EndReason) -> &'static str {
    use omuzaki_ops_api::EndReason;
    match reason {
        EndReason::Disconnected => "the connection ended",
        EndReason::IdleTimeout => "the session went idle",
        EndReason::Closed => "the session was closed",
        EndReason::Revoked { .. } => "the person revoked the session",
        EndReason::ProjectClosed => "the project was closed",
    }
}

/// The heads-up paragraph for decisions that arrived since the agent's last call, and its structured form.
pub(crate) fn heads_up(news: &[(Own, Resolution)]) -> (String, Value) {
    let mut lines = vec!["Heads-up: approvals you were waiting for have been decided since your last call.".to_owned()];
    let mut data = Vec::new();
    for (own, resolution) in news {
        let id = resolution.id.0;
        let by = resolution.by.as_ref().map_or_else(|| "a person".to_owned(), actor_label);
        let (decision, text) = match &resolution.result {
            Resolved::Applied { .. } => ("approved", format!("approved by {by} and applied")),
            Resolved::Failed { error } => {
                ("failed", format!("approved by {by}, but it failed: {}", plain(&error.message, 200)))
            }
            Resolved::Rejected { comment } => {
                let said = comment.as_deref().map(|c| plain(c, COMMENT_CHARS)).filter(|c| !c.is_empty());
                ("rejected", said.map_or_else(|| "rejected".to_owned(), |c| format!("rejected: '{c}'")))
            }
            Resolved::Withdrawn { reason } => ("withdrawn", format!("withdrawn ({})", withdrawn_text(reason))),
        };
        lines.push(format!("- Your request #{id} ({}) was {text}.", own.tool));
        data.push(json!({"approval": id, "tool": own.tool, "decision": decision}));
    }
    (lines.join("\n"), Value::Array(data))
}

#[cfg(test)]
mod tests {
    use omuzaki_core::time::Seconds;
    use omuzaki_ops_api::{Actor, EndReason, Outcome, Plan};

    use super::*;

    fn resolution(result: Resolved) -> Resolution {
        Resolution { id: ApprovalId(4), by: Some(Actor::Person { name: "Sam".into() }), result, at: Seconds(1.0) }
    }

    fn own() -> Own {
        Own { tool: "export_vrm".into(), kind: Some(OpKind::Job) }
    }

    #[test]
    fn a_rejection_carries_the_persons_comment_on_one_line() {
        let result =
            outcome(&own(), &resolution(Resolved::Rejected { comment: Some("not\nyet \u{202e}".into()) }), None);
        let text = result.content[0].as_text().unwrap().text.clone();
        assert!(text.contains("The person rejected this: 'not yet ?'"), "{text}");
        assert_eq!(result.is_error, Some(true));
        assert_eq!(result.structured_content.unwrap()["decision"], "rejected");
    }

    #[test]
    fn an_approved_request_renders_like_the_ops_own_success() {
        let result = outcome(
            &own(),
            &resolution(Resolved::Applied { outcome: Outcome::new(Plan::answer(json!({"file": "a.vrm"}))) }),
            None,
        );
        let text = result.content[0].as_text().unwrap().text.clone();
        assert!(text.starts_with("Approval #4: Approved by user Sam; the request ran.\n"), "{text}");
        assert!(text.contains("a.vrm"), "{text}");
        assert_ne!(result.is_error, Some(true));
        let data = result.structured_content.unwrap();
        assert_eq!((data["ok"].clone(), data["approval"].clone()), (json!(true), json!(4)));
    }

    #[test]
    fn a_withdrawal_says_why() {
        let result = outcome(&own(), &resolution(Resolved::Withdrawn { reason: EndReason::Disconnected }), None);
        assert!(result.content[0].as_text().unwrap().text.contains("the connection ended"));
    }

    #[test]
    fn the_wording_of_a_need_says_what_the_person_is_asked() {
        assert!(need_text(&Need::Scope { scope: omuzaki_ops_api::Scope::Publish }).contains("`publish`"));
        assert!(need_text(&Need::Pairing { client: "claude".into() }).contains("pair the client claude"));
    }
}
