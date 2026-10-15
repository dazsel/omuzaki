//! What a person approved for a plugin, and the ceiling its session runs under (design `plugins.md` §3.2).

use omuzaki_ops_api::{Grants, Scope};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::manifest::NEVER_GRANTED;
use crate::{PER_USE, Requests};

/// What the person approved, attached to the package's content hash and publisher key (never its name).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Approval {
    /// The op scopes approved.
    pub scopes: Vec<Scope>,
    /// The network hosts approved one by one, for `http.fetch`.
    pub hosts: Vec<String>,
    /// Whether the data folder is approved.
    pub data: bool,
}

impl Approval {
    /// Everything `requests` asks for that a plugin may ever be granted: what a person approves by ticking every line.
    pub fn all_of(requests: &Requests) -> Self {
        Self {
            scopes: requests.scopes.iter().copied().filter(|s| !NEVER_GRANTED.contains(s)).collect(),
            hosts: requests.net_hosts.clone(),
            data: requests.data,
        }
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
        let approval =
            Approval { scopes: vec![Scope::Read, Scope::Publish, Scope::Net, Scope::Native], ..Approval::default() };
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
        let none = Approval { hosts: vec!["example.com".into()], ..Approval::default() };
        assert_eq!(approved_hosts(&asked, &none), Vec::<String>::new());
        let ticked =
            Approval { scopes: vec![Scope::Net], hosts: vec!["example.com".into(), "other.org".into()], data: false };
        assert_eq!(approved_hosts(&asked, &ticked), ["example.com"]);
        assert_eq!(Approval::all_of(&asked).hosts, ["example.com"]);
    }
}
