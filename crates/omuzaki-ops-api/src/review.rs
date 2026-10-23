//! Team review policies: which requests wait for a reviewer before they run (VC-11, design `agent-sessions.md` §7.1
//! and §7.5, `team.md`).
//!
//! The executor asks the installed [`ReviewPolicies`] after a request's dry-run succeeds (so the licence gate has
//! run). A requirement queues the request as [`Need::Review`](crate::Need::Review) instead of running it, and it runs
//! once the required number of distinct reviewers approved it. Without policies nothing asks for a review.

/// What a policy asks of a request: the policy's name, shown on the card and in the journal, and how many distinct
/// reviewers must approve.
///
/// `#[non_exhaustive]`: build one with [`ReviewRequirement::new`].
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReviewRequirement {
    /// The policy that asks (for example `publish: 1 reviewer`).
    pub policy: String,
    /// How many distinct reviewers must approve. At least 1: a requirement of 0 asks for nothing.
    pub approvals: u8,
}

impl ReviewRequirement {
    /// Policy `policy` asks for `approvals` distinct reviewers.
    pub fn new(policy: impl Into<String>, approvals: u8) -> Self {
        Self { policy: policy.into(), approvals }
    }
}

/// The team's review rules, registered with the executor by the app. Asked under the executor's lock, so an
/// implementation must answer from what it holds and never call back into the executor.
pub trait ReviewPolicies: Send + Sync {
    /// The requirement for running an op of `scope` (`grant: false`), or for granting `scope` to a session
    /// (`grant: true`); `None` means no review.
    fn requirement(&self, scope: crate::Scope, grant: bool) -> Option<ReviewRequirement>;

    /// Whether `person` may approve a request made for `principal` under `policy`. The executor never asks about the
    /// principal or about anything but a person: those are refused before this is called.
    fn may_approve(&self, person: &crate::Actor, principal: &crate::Actor, policy: &str) -> bool;
}
