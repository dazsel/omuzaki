//! [`FakeApprovals`]: an [`Approvals`] built in a test, approval by approval.

use std::collections::BTreeMap;

use omuzaki_live_api::need::{Invocation, Need};
use omuzaki_live_api::services::{Approvals, ApprovedProgram, ShowApprovals, ShowKey};
use omuzaki_store_api::ContentHash;

/// Approvals per show, held in memory.
#[derive(Clone, Debug, Default)]
pub struct FakeApprovals {
    shows: BTreeMap<ShowKey, ShowApprovals>,
}

impl FakeApprovals {
    /// Nothing approved.
    pub fn new() -> Self {
        Self::default()
    }

    fn edit(mut self, key: &ShowKey, change: impl FnOnce(ShowApprovals) -> ShowApprovals) -> Self {
        let approvals = self.shows.remove(key).unwrap_or_default();
        self.shows.insert(key.clone(), change(approvals));
        self
    }

    /// With `need` allowed for `key`.
    #[must_use]
    pub fn allow(self, key: &ShowKey, need: Need) -> Self {
        self.edit(key, |a| a.with_granted(need))
    }

    /// With `path` chosen as an fs root for `key`.
    #[must_use]
    pub fn root(self, key: &ShowKey, path: impl Into<String>) -> Self {
        self.edit(key, |a| a.with_root(path))
    }

    /// With `invocation` approved for `key`, pinned to these hashes.
    #[must_use]
    pub fn trust_program(
        self,
        key: &ShowKey,
        invocation: Invocation,
        program_hash: ContentHash,
        script_hash: Option<ContentHash>,
    ) -> Self {
        self.edit(key, |a| a.with_program(ApprovedProgram::new(invocation, program_hash, script_hash)))
    }

    /// With this spend cap for `key`.
    #[must_use]
    pub fn spend_cap(self, key: &ShowKey, usd_micros: u64) -> Self {
        self.edit(key, |a| a.with_spend_cap(usd_micros))
    }

    /// Starting `key` with its project, or not.
    #[must_use]
    pub fn autostart(self, key: &ShowKey, autostart: bool) -> Self {
        self.edit(key, |a| a.with_autostart(autostart))
    }
}

impl Approvals for FakeApprovals {
    fn show(&self, key: &ShowKey) -> ShowApprovals {
        self.shows.get(key).cloned().unwrap_or_default()
    }

    fn program_trusted(
        &self,
        invocation: &Invocation,
        program_hash: &ContentHash,
        script_hash: Option<&ContentHash>,
    ) -> bool {
        self.shows.values().flat_map(|a| &a.programs).any(|p| {
            p.invocation == *invocation && p.program_hash == *program_hash && p.script_hash.as_ref() == script_hash
        })
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_live_api::ids::ShowId;
    use omuzaki_live_api::need::NetScheme;

    use super::*;

    #[test]
    fn approvals_are_per_show_and_programs_pinned_by_hash() {
        let main = ShowKey::new("/projects/demo", ShowId::new("main").unwrap());
        let other = ShowKey::new("/projects/demo", ShowId::new("other").unwrap());
        let obs = Invocation::new("/opt/obs/obs");
        let (hash, changed) = (ContentHash::of(b"obs v1"), ContentHash::of(b"obs v2"));
        let approvals = FakeApprovals::new()
            .allow(&main, Need::host(NetScheme::Http, "127.0.0.1", 7474))
            .root(&main, "/streams/text")
            .trust_program(&main, obs.clone(), hash, None);
        assert_eq!(approvals.show(&main).granted.len(), 2);
        assert_eq!(approvals.show(&main).roots, ["/streams/text"]);
        assert_eq!(approvals.show(&other), ShowApprovals::default());
        assert!(approvals.program_trusted(&obs, &hash, None));
        assert!(!approvals.program_trusted(&obs, &changed, None));
        assert!(!approvals.program_trusted(&obs, &hash, Some(&hash)));
    }
}
