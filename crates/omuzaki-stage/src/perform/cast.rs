//! [`LiveCast`]: the performers of a live scene, one per performing cast member (`docs/design/drivers.md` §4).
//!
//! Each member that performs has its own [`Performer`](super::Performer) and its own [`PerformHost`]: its own
//! tracker, voice, triggers, live score and drive map. The ops reach a member's performer through this service
//! (`act.* { member }`, `drive.*`), and a member left out means the scene's subject. An app that runs a single
//! performer and no cast (today's studio) serves [`PerformHost`] alone; the ops then treat it as the subject.

use std::sync::{Arc, PoisonError, RwLock};

use omuzaki_stage_api::ids::MemberId;

use super::host::PerformHost;

/// The performers of a live scene, by member. Implemented by [`CastBook`]; the app injects it as `Arc<dyn LiveCast>`.
pub trait LiveCast: Send + Sync {
    /// The members performing now, the subject first.
    fn members(&self) -> Vec<MemberId>;

    /// The performer of `member`, or the subject's with `None`, with the member it is.
    fn performer(&self, member: Option<&MemberId>) -> Option<(MemberId, Arc<dyn PerformHost>)>;
}

/// The members performing and their hosts, the subject first. Cheap to share; the live loop adds and removes members
/// as they start and stop performing.
#[derive(Default)]
pub struct CastBook {
    members: RwLock<Vec<(MemberId, Arc<dyn PerformHost>)>>,
}

impl CastBook {
    /// No member performs yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `member` as performed by `host`, replacing an entry for the same member. The first member added is the
    /// subject until [`set_subject`](Self::set_subject) says otherwise.
    pub fn insert(&self, member: MemberId, host: Arc<dyn PerformHost>) {
        let mut members = self.members.write().unwrap_or_else(PoisonError::into_inner);
        match members.iter_mut().find(|(m, _)| *m == member) {
            Some(entry) => entry.1 = host,
            None => members.push((member, host)),
        }
    }

    /// Removes `member`. Returns whether it was performing.
    pub fn remove(&self, member: &MemberId) -> bool {
        let mut members = self.members.write().unwrap_or_else(PoisonError::into_inner);
        let before = members.len();
        members.retain(|(m, _)| m != member);
        members.len() != before
    }

    /// Makes `member` the subject (the member an op means when it names none). Returns whether it is performing.
    pub fn set_subject(&self, member: &MemberId) -> bool {
        let mut members = self.members.write().unwrap_or_else(PoisonError::into_inner);
        let Some(at) = members.iter().position(|(m, _)| m == member) else { return false };
        members[..=at].rotate_right(1);
        true
    }
}

impl LiveCast for CastBook {
    fn members(&self) -> Vec<MemberId> {
        self.members.read().unwrap_or_else(PoisonError::into_inner).iter().map(|(m, _)| m.clone()).collect()
    }

    fn performer(&self, member: Option<&MemberId>) -> Option<(MemberId, Arc<dyn PerformHost>)> {
        let members = self.members.read().unwrap_or_else(PoisonError::into_inner);
        let found = match member {
            Some(member) => members.iter().find(|(m, _)| m == member),
            None => members.first(),
        };
        found.map(|(m, host)| (m.clone(), Arc::clone(host)))
    }
}

impl std::fmt::Debug for CastBook {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CastBook").field("members", &self.members()).finish()
    }
}
