//! The last change to each document path: the stale-write guard's index (design `agent-sessions.md` §5.1) and the
//! name a conflict message gives for who changed a path.
//!
//! Every applied patch updates it: ops, rollbacks, undos, redos, session ends and undo-session, each with the actor
//! who made the change. A lookup checks the path itself, every ancestor (by walking its prefixes) and every descendant
//! (a range of the ordered map), so it never scans the op log. Actors are interned, so updating an entry that exists
//! allocates nothing.

use std::collections::BTreeMap;

use omuzaki_doc_api::{DocPath, DocVersion, Patch};
use omuzaki_ops_api::Actor;

/// One path's last change: the document version after it and who made it (an index into `actors`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Change {
    version: DocVersion,
    actor: u32,
}

/// The newest change at, above or below a path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LastChange<'a> {
    /// The path that changed.
    pub(crate) path: &'a DocPath,
    /// Who changed it.
    pub(crate) by: &'a Actor,
    /// The document version after that change.
    pub(crate) version: DocVersion,
}

/// The index. Its size is the number of distinct paths changed since the executor started (or was restored).
#[derive(Default)]
pub(crate) struct Index {
    paths: BTreeMap<DocPath, Change>,
    actors: Vec<Actor>,
}

impl Index {
    /// Records that `by` changed every path of `patch`, leaving the document at `version`.
    pub(crate) fn record(&mut self, patch: &Patch, by: &Actor, version: DocVersion) {
        if patch.is_empty() {
            return;
        }
        let actor = self.intern(by);
        let change = Change { version, actor };
        for path in patch.changes.iter().map(|c| &c.path) {
            match self.paths.get_mut(path) {
                Some(slot) => *slot = change,
                None => {
                    self.paths.insert(path.clone(), change);
                }
            }
        }
    }

    /// The newest change at `path`, at one of its ancestors or below it, made by anyone but `except`.
    pub(crate) fn newest(&self, path: &DocPath, except: Option<&Actor>) -> Option<LastChange<'_>> {
        let except = except.and_then(|actor| self.actors.iter().position(|a| a == actor));
        let keep =
            |(_, change): &(&DocPath, &Change)| except.is_none_or(|e| usize::try_from(change.actor).ok() != Some(e));
        let segments = path.segments();
        let ancestors = (0..segments.len()).filter_map(|n| {
            let prefix = DocPath::root();
            let prefix = segments[..n].iter().fold(prefix, |p, s| p.join(s.as_str()));
            self.paths.get_key_value(&prefix)
        });
        let below = self.paths.range(path.clone()..).take_while(|(p, _)| p.starts_with(path));
        ancestors.chain(below).filter(keep).max_by_key(|(_, change)| change.version).and_then(|(path, change)| {
            let by = self.actors.get(usize::try_from(change.actor).ok()?)?;
            Some(LastChange { path, by, version: change.version })
        })
    }

    /// How many paths it knows.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.paths.len()
    }

    fn intern(&mut self, actor: &Actor) -> u32 {
        let index = if let Some(index) = self.actors.iter().position(|a| a == actor) {
            index
        } else {
            self.actors.push(actor.clone());
            self.actors.len() - 1
        };
        // A u32 holds more actors than one executor will ever see; past it, the last one is reused.
        u32::try_from(index).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn path(s: &str) -> DocPath {
        DocPath::parse(s).unwrap()
    }

    fn set(p: &str) -> Patch {
        Patch::new().with(path(p), None, Some(json!(1)))
    }

    #[test]
    fn a_lookup_sees_the_path_its_ancestors_and_its_descendants_newest_first() {
        let (alice, bot) = (Actor::Person { name: "alice".into() }, Actor::Agent { id: "b#1".into(), model: None });
        let mut index = Index::default();
        index.record(&set("face/blend"), &alice, DocVersion(3));
        index.record(&set("face"), &bot, DocVersion(4));
        index.record(&set("face/eyes/l"), &alice, DocVersion(5));
        let at = |p: &str| index.newest(&path(p), None).map(|c| (c.path.to_string(), c.version.0));
        assert_eq!(at("face/blend"), Some(("face".into(), 4)), "an ancestor's newer change");
        assert_eq!(at("face"), Some(("face/eyes/l".into(), 5)), "a descendant's newer change");
        assert_eq!(at("hair"), None);
        let not_alice = index.newest(&path("face/eyes"), Some(&alice)).unwrap();
        assert_eq!((not_alice.by, not_alice.version), (&bot, DocVersion(4)));
        assert_eq!(index.newest(&path("face/blend"), Some(&bot)).map(|c| c.version), Some(DocVersion(3)));
        assert_eq!(index.len(), 3);
    }
}
