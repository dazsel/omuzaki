//! The review card (`team-review.md` section 8.4): a request a team policy holds for its reviewers
//! ([`Need::Review`](omuzaki_ops_api::Need::Review)).
//!
//! It shows who asked and why, the preview, the licence check, the open comment threads and imported feedback on
//! what it touches, and how many approvals it has ("1 of 2 approvals: Ada"). Approve is disabled, with the reason
//! shown, for the person who asked (never the requester, section 8.3) and when this studio's copy re-plans to a
//! different digest than the one requested (section 8.5: nobody signs content A while looking at content B). Reject,
//! with a comment, is always there, and the request can be exported for a reviewer on another machine.
//!
//! The evidence comes from the team service through a [`ReviewDesk`] the app shares as a view model ([`Reviews`]).
//! Without one, the card cannot check the request against this copy, so Approve stays disabled and says so.

use std::sync::Arc;

use egui::Ui;
use omuzaki_ops_api::{Actor, OpError, PendingApproval};

use super::super::text::{self, say};
use super::approvals::Answer;
use crate::ds::theme::ds;
use crate::ds::widgets::approval_card::{ApprovalCard, Decision as CardDecision};

/// What a reviewer needs to see to decide (section 8.4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReviewEvidence {
    /// The person looking is the one who asked (for an agent, its principal).
    pub own: bool,
    /// The digest of the preview that was requested.
    pub requested: String,
    /// The digest this studio's copy re-plans to, once it has; `None` until then.
    pub local: Option<String>,
    /// The reviewers who approved so far, by name.
    pub approved_by: Vec<String>,
    /// How many approvals the policy needs.
    pub needed: u32,
    /// The unresolved comment threads anchored to anything the request touches, one line each.
    pub threads: Vec<String>,
    /// Feedback imported from review links, one line each.
    pub feedback: Vec<String>,
    /// The licence gate's result, in words.
    pub licence: Option<String>,
}

/// Where the review card gets its evidence and sends an export: the team service.
pub trait ReviewDesk {
    /// The evidence for `approval`, or `None` if the desk knows nothing of it.
    fn evidence(&self, approval: &PendingApproval) -> Option<ReviewEvidence>;

    /// Writes the request for a reviewer on another machine; returns where it went, in words.
    ///
    /// # Errors
    /// Whatever stopped the export.
    fn export(&self, approval: &PendingApproval) -> Result<String, OpError>;
}

/// The review desk, shared with panels as a view model.
#[derive(Clone)]
pub struct Reviews(pub Arc<dyn ReviewDesk>);

/// What the reviewer did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalAction {
    /// Approve, as this reviewer.
    Approve,
    /// Reject; the comment reaches the requester.
    Reject {
        /// Why.
        comment: Option<String>,
    },
    /// Export the request for a reviewer elsewhere.
    Export,
}

/// The card for a review request, and what the reviewer did this frame.
pub fn review_card(ui: &mut Ui, approval: &PendingApproval, evidence: &ReviewEvidence) -> Option<ApprovalAction> {
    show(ui, approval, &requester(&approval.actor), Some(evidence), "panel")
}

/// Who asked, by the actor's own name.
fn requester(actor: &Actor) -> String {
    match actor {
        Actor::Agent { id, .. } | Actor::Plugin { id } => id.clone(),
        Actor::Person { name } => name.clone(),
        Actor::System => String::new(),
    }
}

/// The review card inside the approval list: the evidence from `desk`, the answer in the list's terms.
pub(super) fn review_batch(
    ui: &mut Ui,
    approval: &PendingApproval,
    asked_by: &str,
    desk: Option<&dyn ReviewDesk>,
    salt: &str,
) -> Option<Answer> {
    let evidence = desk.and_then(|d| d.evidence(approval));
    Some(match show(ui, approval, asked_by, evidence.as_ref(), salt)? {
        ApprovalAction::Approve => Answer::Decide(vec![approval.id], omuzaki_ops_api::Decision::Approve),
        ApprovalAction::Reject { comment } => {
            Answer::Decide(vec![approval.id], omuzaki_ops_api::Decision::Reject { comment })
        }
        ApprovalAction::Export => Answer::Export(approval.id),
    })
}

fn show(
    ui: &mut Ui,
    approval: &PendingApproval,
    requester: &str,
    evidence: Option<&ReviewEvidence>,
    salt: &str,
) -> Option<ApprovalAction> {
    let ds = ds(ui.ctx());
    let op = approval.request.as_ref().map_or("", |r| r.op.as_str());
    let what = say(&ds, text::REVIEW_WHAT, &[("op", op)]).into_owned();
    let mut lines = Vec::new();
    if let omuzaki_ops_api::Need::Review { policy } = &approval.need {
        lines.push(say(&ds, text::REVIEW_POLICY, &[("policy", policy)]).into_owned());
    }
    if let Some(cause) = approval.request.as_ref().and_then(|r| r.cause.as_ref()).and_then(|c| c.text.as_deref()) {
        lines.push(say(&ds, text::REVIEW_WHY, &[("cause", cause)]).into_owned());
    }
    let blocked = match evidence {
        None => Some(say(&ds, text::REVIEW_UNKNOWN, &[]).into_owned()),
        Some(e) if e.own => Some(say(&ds, text::REVIEW_OWN, &[]).into_owned()),
        Some(e) if e.local.as_deref() != Some(e.requested.as_str()) => {
            Some(say(&ds, text::REVIEW_DIFFERS, &[]).into_owned())
        }
        Some(_) => None,
    };
    if let Some(e) = evidence {
        let (have, need) = (e.approved_by.len().to_string(), e.needed.to_string());
        lines.push(if e.approved_by.is_empty() {
            say(&ds, text::REVIEW_PROGRESS, &[("have", &have), ("need", &need)]).into_owned()
        } else {
            let names = e.approved_by.join(", ");
            say(&ds, text::REVIEW_PROGRESS_BY, &[("have", &have), ("need", &need), ("names", &names)]).into_owned()
        });
        if let Some(licence) = &e.licence {
            lines.push(say(&ds, text::REVIEW_LICENCE, &[("result", licence)]).into_owned());
        }
        if !e.threads.is_empty() {
            let count = e.threads.len().to_string();
            lines.push(if e.threads.len() == 1 {
                say(&ds, text::REVIEW_THREAD_ONE, &[]).into_owned()
            } else {
                say(&ds, text::REVIEW_THREADS, &[("count", &count)]).into_owned()
            });
            lines.extend(e.threads.iter().map(|t| say(&ds, text::REVIEW_THREAD, &[("text", t)]).into_owned()));
        }
        lines.extend(e.feedback.iter().map(|f| say(&ds, text::REVIEW_FEEDBACK, &[("text", f)]).into_owned()));
    }
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let changes: Vec<String> = approval
        .request
        .as_ref()
        .map(|r| r.preview.patch.changes.iter().map(|c| c.path.to_string()).collect())
        .unwrap_or_default();
    let change_refs: Vec<&str> = changes.iter().map(String::as_str).collect();
    let summary = text::changes(&ds, changes.len());
    let export = say(&ds, text::REVIEW_EXPORT, &[]);
    let mut card = ApprovalCard::new(("review_card", salt, approval.id), requester, &what)
        .scope("")
        .offers(false, false)
        .details(&refs)
        .export(&export);
    if !change_refs.is_empty() {
        card = card.changes(&summary, &change_refs);
    }
    if let Some(reason) = blocked.as_deref() {
        card = card.approve_blocked(reason);
    }
    Some(match card.show(ui)? {
        CardDecision::Approve | CardDecision::ApproveAll | CardDecision::Accept(_) if blocked.is_none() => {
            ApprovalAction::Approve
        }
        CardDecision::Export => ApprovalAction::Export,
        CardDecision::Reject { comment } => ApprovalAction::Reject { comment },
        _ => return None,
    })
}
