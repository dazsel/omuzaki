//! What a person approved for a plugin, and the ceiling its session runs under (design `plugins.md` §3.2).

use omuzaki_ops_api::{Grants, Scope};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::manifest::NEVER_GRANTED;
use crate::manifest::Requests;
use crate::summary::PER_USE;

/// What the person approved, attached to the package's content hash and publisher key (never its name).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Approval {
    /// The op scopes approved.
    pub scopes: Vec<Scope>,
    /// The network hosts approved one by one, for `http.fetch`.
    pub hosts: Vec<String>,
    /// Whether the data folder is approved.
    pub data: bool,
    /// The data folder's quota the person saw on the card, in MiB; 0 when the folder isn't approved. An update that
    /// asks for more shows the increase (design §3.2).
    #[serde(default)]
    pub data_quota_mb: u32,
}

impl Approval {
    /// An approval of `scopes`, `hosts` and, when `data_quota_mb` is given, the data folder at that quota.
    pub fn new(scopes: Vec<Scope>, hosts: Vec<String>, data_quota_mb: Option<u32>) -> Self {
        Self { scopes, hosts, data: data_quota_mb.is_some(), data_quota_mb: data_quota_mb.unwrap_or(0) }
    }

    /// Everything `requests` asks for that a plugin may ever be granted: what a person approves by ticking every line.
    pub fn all_of(requests: &Requests) -> Self {
        Self::new(
            requests.scopes.iter().copied().filter(|s| !NEVER_GRANTED.contains(s)).collect(),
            requests.net_hosts.clone(),
            requests.data.then_some(requests.data_quota_mb),
        )
    }

    /// This approval narrowed to what `requests` asks for: no scope, host or data folder it doesn't request, and no
    /// quota above the one it requests.
    #[must_use]
    pub fn within(self, requests: &Requests) -> Self {
        let data = (self.data && requests.data).then(|| self.data_quota_mb.min(requests.data_quota_mb));
        Self::new(
            self.scopes.into_iter().filter(|s| requests.scopes.contains(s)).collect(),
            self.hosts.into_iter().filter(|h| requests.net_hosts.contains(h)).collect(),
            data,
        )
    }
}

/// The plugin session's grants: manifest requests ∩ the person's approval ∩ the person's own grants. The scopes a
/// plugin is never granted are dropped whatever was approved, and so is `net`: it means "may use `http.fetch`" to
/// approved hosts, never an op of scope `net`. [`PER_USE`] scopes stay in the ceiling; the executor asks a person on
/// every use.
pub fn ceiling(requests: &Requests, approval: &Approval, person: &Grants) -> Grants {
    let scopes = requests
        .scopes
        .iter()
        .copied()
        .filter(|s| approval.scopes.contains(s) && person.allows(*s))
        .filter(|s| !NEVER_GRANTED.contains(s) && *s != Scope::Net);
    Grants::of(scopes)
}

/// Whether a call of `scope` from a plugin session waits for a person every time, even inside the ceiling.
pub fn needs_person_each_use(scope: Scope) -> bool {
    PER_USE.contains(&scope)
}

/// The hosts `fetch` may reach: requested, approved, and only while `net` is requested and approved.
pub fn approved_hosts(requests: &Requests, approval: &Approval) -> Vec<String> {
    let net = requests.scopes.contains(&Scope::Net) && approval.scopes.contains(&Scope::Net);
    if !net {
        return Vec::new();
    }
    requests.net_hosts.iter().filter(|h| approval.hosts.contains(h)).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn requests(scopes: &[Scope]) -> Requests {
        Requests { scopes: scopes.to_vec(), net_hosts: vec!["example.com".into()], ..Requests::default() }
    }

    #[test]
    fn the_ceiling_is_the_intersection_without_net_or_never_granted_scopes() {
        let asked = requests(&[Scope::Read, Scope::Perform, Scope::Publish, Scope::Net, Scope::Native]);
        let approval = Approval::new(vec![Scope::Read, Scope::Publish, Scope::Net, Scope::Native], Vec::new(), None);
        let person = Grants::of([Scope::Read, Scope::Perform, Scope::Publish, Scope::Native, Scope::Net]);
        let got: Vec<Scope> = ceiling(&asked, &approval, &person).scopes().collect();
        assert_eq!(got, [Scope::Read, Scope::Publish]);
        let narrow = Grants::of([Scope::Read]);
        assert_eq!(ceiling(&asked, &approval, &narrow).scopes().collect::<Vec<_>>(), [Scope::Read]);
        assert!(needs_person_each_use(Scope::Publish) && !needs_person_each_use(Scope::Read));
    }

    #[test]
    fn hosts_need_net_requested_and_approved_and_each_host_ticked() {
        let asked = requests(&[Scope::Net]);
        let none = Approval::new(Vec::new(), vec!["example.com".into()], None);
        assert_eq!(approved_hosts(&asked, &none), Vec::<String>::new());
        let ticked = Approval::new(vec![Scope::Net], vec!["example.com".into(), "other.org".into()], None);
        assert_eq!(approved_hosts(&asked, &ticked), ["example.com"]);
        assert_eq!(Approval::all_of(&asked).hosts, ["example.com"]);
    }

    #[test]
    fn an_approval_is_narrowed_to_the_requests_and_their_quota() {
        let asked = Requests { data: true, data_quota_mb: 64, ..requests(&[Scope::Read]) };
        let wide =
            Approval::new(vec![Scope::Read, Scope::Perform], vec!["example.com".into(), "x.org".into()], Some(500));
        let narrowed = wide.within(&asked);
        assert_eq!(narrowed, Approval::new(vec![Scope::Read], vec!["example.com".into()], Some(64)));
        let no_data = Requests { data: false, ..asked.clone() };
        assert_eq!(Approval::all_of(&asked).within(&no_data).data_quota_mb, 0);
        assert_eq!(Approval::all_of(&asked).data_quota_mb, 64);
    }
}
