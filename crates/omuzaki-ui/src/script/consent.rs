//! The consent card's decision (#1097, `scripting.md` section 3): before a person's first run of a script whose last
//! author was not this person (an agent, an import), or that names an op reaching outside the studio (`broadcast`,
//! `publish`, `spend`, `net`, `fs` or `native`), the console lists what the script will call and asks. "Run" is
//! remembered per project and content hash, so the same script runs straight away next time and an edit asks again.
//!
//! The decision is a pure function of the author, the ops named and what was remembered; the book of remembered runs is
//! a small file beside My scripts.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use omuzaki_ops_api::{Actor, OpInfo, Scope};
use serde_json::Value;

/// The scopes that reach outside the studio: a script naming one asks first, whoever wrote it.
pub const REACHING: [Scope; 6] = [Scope::Broadcast, Scope::Publish, Scope::Spend, Scope::Net, Scope::Fs, Scope::Native];

/// Who saved a script last, as it reads to the person running it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Author {
    /// The person running it (or nobody else on record: a new script, or one of My scripts).
    Me,
    /// An agent, by its id.
    Agent(String),
    /// Another person, by name.
    Person(String),
    /// A plugin, by its id.
    Plugin(String),
    /// Omuzaki itself (an import, a migration).
    System,
}

impl Author {
    /// The author recorded on a project script (`script.list`'s `author`, an [`Actor`] as JSON), as `me` sees it.
    pub fn of(recorded: Option<&Value>, me: &Actor) -> Self {
        let Some(actor) = recorded.and_then(|v| serde_json::from_value::<Actor>(v.clone()).ok()) else {
            return Self::Me;
        };
        if &actor == me {
            return Self::Me;
        }
        match actor {
            Actor::Agent { id, .. } => Self::Agent(id),
            Actor::Person { name } => Self::Person(name),
            Actor::Plugin { id } => Self::Plugin(id),
            Actor::System => Self::System,
        }
    }
}

/// One op the card lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    /// Its registered name.
    pub name: String,
    /// What it does.
    pub summary: String,
    /// What it needs.
    pub scope: Scope,
}

/// What the card asks about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Consent {
    /// Who wrote the script.
    pub author: Author,
    /// Every op the script names, in the order named, once each.
    pub ops: Vec<Named>,
}

impl Consent {
    /// Whether any op it lists reaches outside the studio.
    pub fn reaches(&self) -> bool {
        self.ops.iter().any(|op| REACHING.contains(&op.scope))
    }
}

/// The card to show before running a script by `author` that names `named` (registry names), or `None` when it runs
/// straight away: the person's own script naming only ops that stay inside the studio, or one they agreed to already.
pub fn consent(author: &Author, named: &[String], ops: &[OpInfo], remembered: bool) -> Option<Consent> {
    let mut seen = BTreeSet::new();
    let listed: Vec<Named> = named
        .iter()
        .filter(|name| seen.insert(name.as_str()))
        .map(|name| match ops.iter().find(|info| info.name == name) {
            Some(info) => Named { name: name.clone(), summary: info.summary.to_owned(), scope: info.scope },
            None => Named { name: name.clone(), summary: String::new(), scope: Scope::Read },
        })
        .collect();
    let card = Consent { author: author.clone(), ops: listed };
    let ask = card.author != Author::Me || card.reaches();
    (ask && !remembered).then_some(card)
}

/// The runs the person agreed to, by project and content hash, kept in a file when the studio has a folder for it.
#[derive(Debug, Default)]
pub struct ConsentBook {
    file: Option<PathBuf>,
    agreed: Mutex<BTreeMap<String, BTreeSet<String>>>,
}

impl ConsentBook {
    /// A book kept in memory only.
    pub fn in_memory() -> Self {
        Self::default()
    }

    /// A book kept in `file`, read now; a file that can't be read starts an empty book.
    pub fn in_file(file: PathBuf) -> Self {
        let agreed = std::fs::read_to_string(&file).ok().and_then(|text| serde_json::from_str(&text).ok());
        Self { file: Some(file), agreed: Mutex::new(agreed.unwrap_or_default()) }
    }

    /// Whether the person agreed to run the script with `hash` in `project`.
    pub fn agreed(&self, project: &str, hash: &str) -> bool {
        let agreed = self.agreed.lock().unwrap_or_else(PoisonError::into_inner);
        agreed.get(project).is_some_and(|hashes| hashes.contains(hash))
    }

    /// Remembers that the person agreed, and writes the book.
    pub fn agree(&self, project: &str, hash: &str) {
        let mut agreed = self.agreed.lock().unwrap_or_else(PoisonError::into_inner);
        agreed.entry(project.to_owned()).or_default().insert(hash.to_owned());
        if let Some(file) = &self.file {
            let written = serde_json::to_string_pretty(&*agreed).map_err(std::io::Error::other).and_then(|text| {
                if let Some(dir) = file.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                std::fs::write(file, text)
            });
            if let Err(error) = written {
                tracing::warn!(%error, "the agreed scripts were not saved: the console asks again next time");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::{OpKind, OpMeta, Undo};
    use serde_json::json;

    use super::*;

    fn op(name: &'static str, scope: Scope) -> OpInfo {
        let meta = OpMeta { name, summary: "Does it.", kind: OpKind::Edit, scope, undo: Undo::Auto };
        OpInfo::new(meta, schemars::Schema::default())
    }

    fn ops() -> Vec<OpInfo> {
        vec![op("face.set_blush", Scope::EditCharacter), op("stream.publish_clip", Scope::Publish)]
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    #[test]
    fn the_persons_own_edits_run_straight_away() {
        assert_eq!(consent(&Author::Me, &names(&["face.set_blush", "face.set_blush"]), &ops(), false), None);
    }

    #[test]
    fn an_agents_script_or_a_reaching_op_asks_once_per_hash() {
        let agent = Author::Agent("claude".into());
        let card = consent(&agent, &names(&["face.set_blush", "face.set_blush"]), &ops(), false);
        let card = card.unwrap_or_else(|| panic!("an agent's script asks"));
        assert_eq!(card.ops.len(), 1, "each op once");
        assert_eq!(card.ops[0].summary, "Does it.");
        assert!(!card.reaches());
        let publish = consent(&Author::Me, &names(&["stream.publish_clip"]), &ops(), false);
        assert!(publish.is_some_and(|c| c.reaches()), "a publish op asks even in the person's own script");
        assert_eq!(consent(&agent, &names(&["face.set_blush"]), &ops(), true), None, "agreed before");
    }

    #[test]
    fn the_author_is_read_from_the_saved_actor() {
        let me = Actor::Person { name: "you".into() };
        assert_eq!(Author::of(None, &me), Author::Me);
        assert_eq!(Author::of(Some(&json!(me)), &me), Author::Me);
        let agent = Actor::Agent { id: "claude".into(), model: None };
        assert_eq!(Author::of(Some(&json!(agent)), &me), Author::Agent("claude".into()));
    }

    #[test]
    fn the_book_remembers_per_project_and_hash_across_opens() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let file = dir.path().join("agreed.json");
        let book = ConsentBook::in_file(file.clone());
        assert!(!book.agreed("p", "h1"));
        book.agree("p", "h1");
        let again = ConsentBook::in_file(file);
        assert!(again.agreed("p", "h1"));
        assert!(!again.agreed("p", "h2"), "an edit is a new hash");
        assert!(!again.agreed("q", "h1"), "another project asks again");
    }
}
