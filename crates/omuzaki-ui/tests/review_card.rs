//! The review card (#1374, `team-review.md` section 8.4): a request a team policy holds for its reviewers. Approve is
//! disabled with its reason for the requester and for a copy that re-plans to another digest; a reviewer's approve
//! resolves once; the card shows "1 of 2 approvals" with names and the open comment threads; the request exports for
//! a reviewer elsewhere.

mod fake_agents;

use std::sync::{Arc, Mutex};

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use fake_agents::{Call, FakeAgents, Rig, approval, click, rig_with, session, shows};
use omuzaki_ops_api::{Decision, Need, OpError, PendingApproval, Transport};
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::audit::plain_english;
use omuzaki_ui::ds::set_strings;
use omuzaki_ui::ds::strings::{English, Pseudo};
use omuzaki_ui::ds::testing::{all_strings, harness_state};
use omuzaki_ui::panels::agent::review::{ApprovalAction, ReviewDesk, ReviewEvidence, Reviews, review_card};

const APPROVE: &str = "Approve: Review publish.upload before it runs (claude_code)";
/// The pure card names the requester by its actor id.
const APPROVE_ALONE: &str = "Approve: Review publish.upload before it runs (claude_code#1)";

/// Evidence as the team service would give it, and every export asked of it.
struct Desk {
    evidence: ReviewEvidence,
    exports: Mutex<usize>,
}

impl ReviewDesk for Desk {
    fn evidence(&self, _approval: &PendingApproval) -> Option<ReviewEvidence> {
        Some(self.evidence.clone())
    }

    fn export(&self, _approval: &PendingApproval) -> Result<String, OpError> {
        *self.exports.lock().unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
        Ok("Saved review_request.json".to_owned())
    }
}

/// Half-reviewed: Ada approved, two threads are open, this copy re-plans to the requested digest.
fn evidence() -> ReviewEvidence {
    ReviewEvidence {
        own: false,
        requested: "b3:4f2a".into(),
        local: Some("b3:4f2a".into()),
        approved_by: vec!["Ada".into()],
        needed: 2,
        threads: vec!["the fringe clips the brow".into(), "the hair colour is too warm".into()],
        feedback: Vec::new(),
        licence: Some("passed".into()),
    }
}

fn review(evidence: ReviewEvidence) -> (Harness<'static, Rig>, Arc<Desk>) {
    let agent = session(1, "claude_code", Transport::Stdio);
    let pending = approval(1, &agent, "publish.upload", Need::Review { policy: "two_reviewers".into() });
    let (mut h, _) = rig_with(FakeAgents::new(vec![agent], vec![pending]), vec2(460.0, 1000.0));
    let desk = Arc::new(Desk { evidence, exports: Mutex::new(0) });
    h.state_mut().views.insert(Reviews(Arc::clone(&desk) as Arc<dyn ReviewDesk>));
    h.run_steps(3);
    (h, desk)
}

fn approve_disabled(h: &Harness<'_, Rig>) -> bool {
    h.get_by_label(APPROVE).accesskit_node().is_disabled()
}

#[test]
fn the_requester_cannot_approve_and_is_told_why() {
    let (mut h, _) = review(ReviewEvidence { own: true, ..evidence() });
    assert!(approve_disabled(&h));
    assert!(shows(&h, "You asked for this, so another reviewer approves it"), "{:?}", all_strings(&h));
    click(&mut h, APPROVE);
    assert_eq!(h.state().fake.calls(), []);
}

#[test]
fn a_copy_that_differs_from_what_was_requested_cannot_approve() {
    let (h, _) = review(ReviewEvidence { local: Some("b3:9c01".into()), ..evidence() });
    assert!(approve_disabled(&h));
    assert!(shows(&h, "Your copy differs from what was requested: sync the project, then review"));
}

#[test]
fn a_reviewer_approves_once() {
    let (mut h, _) = review(evidence());
    assert!(!approve_disabled(&h));
    click(&mut h, APPROVE);
    assert_eq!(h.state().fake.calls(), [Call::Resolve(1, Decision::Approve)]);
}

#[test]
fn the_card_shows_the_approvals_so_far_and_the_open_threads() {
    let (h, _) = review(evidence());
    for line in [
        "1 of 2 approvals: Ada",
        "2 open comment threads",
        "Open thread: the fringe clips the brow",
        "Open thread: the hair colour is too warm",
        "Licence check: passed",
        "Asked by the policy two_reviewers",
    ] {
        assert!(shows(&h, line), "{line:?} in {:?}", all_strings(&h));
    }
}

#[test]
fn the_request_exports_for_a_remote_reviewer() {
    let (mut h, desk) = review(evidence());
    click(&mut h, "Export for a remote reviewer: Review publish.upload before it runs (claude_code)");
    assert_eq!(*desk.exports.lock().unwrap_or_else(std::sync::PoisonError::into_inner), 1);
    assert!(shows(&h, "Saved review_request.json"));
    assert!(h.state().fake.calls().is_empty(), "exporting decides nothing");
}

#[test]
fn without_the_team_service_approve_stays_off() {
    let agent = session(1, "claude_code", Transport::Stdio);
    let pending = approval(1, &agent, "publish.upload", Need::Review { policy: "two_reviewers".into() });
    let (h, _) = rig_with(FakeAgents::new(vec![agent], vec![pending]), vec2(460.0, 1000.0));
    assert!(approve_disabled(&h));
    assert!(shows(&h, "This studio can't check the request against your copy yet"));
}

#[test]
fn review_card_returns_what_the_reviewer_did() {
    let agent = session(1, "claude_code", Transport::Stdio);
    let pending = approval(1, &agent, "publish.upload", Need::Review { policy: "two_reviewers".into() });
    let state = (pending, evidence(), None::<ApprovalAction>);
    let mut h = harness_state(Settings::default(), vec2(460.0, 700.0), state, |ui, s| {
        if let Some(action) = review_card(ui, &s.0, &s.1) {
            s.2 = Some(action);
        }
    });
    h.get_by_label(APPROVE_ALONE).click();
    h.run_steps(2);
    assert_eq!(h.state().2, Some(ApprovalAction::Approve));
}

#[test]
fn every_word_goes_through_the_message_table() {
    let (mut h, _) = review(evidence());
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(3);
    // The threads, names and policy are the reviewers' own text, not the studio's words.
    let theirs = ["the fringe clips the brow", "the hair colour is too warm", "passed", "Ada"];
    let english: Vec<String> = plain_english(all_strings(&h).iter().map(String::as_str))
        .into_iter()
        .filter(|s| !theirs.iter().any(|t| s.contains(t)))
        .collect();
    assert!(english.is_empty(), "plain English in the pseudo-locale: {english:?}");
}
