//! What the studio does at start (`docs/design/settings.md` D9, `docs/design/first-run.md` §6): a pure function of
//! the session, the `startup.reopen` setting, the projects left open by a crash and what the project folders hold.
//!
//! The order: **Recover** first (losing work is worse than an extra click, so recovery wins over the opt-out), then
//! **Setup** when first-run setup is needed, then **Open** the last project when `startup.reopen` is `last`, else the
//! **Welcome** screen. A project that cannot be reopened, or one that was being opened when the app stopped, never
//! blocks the start and never disappears: the Welcome screen gets a notice naming it. Opening a project starts no
//! capture or output, and headless surfaces (the CLI, MCP) never call this: a project is always named there.

use std::path::{Path, PathBuf};

use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::session::SessionState;
use omuzaki_settings_api::setting::Setting;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// `startup.reopen`: what the studio opens at start.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reopen {
    /// Reopen the last project.
    #[default]
    Last,
    /// Show the Welcome screen with a "Reopen <name>" card.
    Ask,
    /// Show the Welcome screen and reopen nothing.
    Welcome,
}

/// The `startup.reopen` setting (D9): a person's choice, so an agent's change waits for them; not in a profile (a
/// profile switch must not change what the next start does); read at the next start.
pub struct StartupReopen;

impl Setting for StartupReopen {
    type Value = Reopen;
    const META: SettingMeta = SettingMeta::new(
        "startup.reopen",
        "What opens at start: the last project, the Welcome screen with an offer to reopen it, or the Welcome screen",
        SettingScope::User,
        AgentWrite::Confirm,
        InProfile::Excluded,
        Apply::NextStart,
        1,
    );

    fn default() -> Reopen {
        Reopen::Last
    }
}

register_setting!(StartupReopen);

/// What a project folder holds, as far as reopening it goes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Folder {
    /// It can be opened.
    Ready,
    /// The folder is not there (deleted, moved, or on a drive that is not connected).
    Missing,
    /// It is there but cannot be opened: no `project.toml`, a file that does not read, a bad schema line.
    Unreadable {
        /// Why, for the person.
        reason: String,
    },
    /// It was saved by a newer Omuzaki.
    Newer {
        /// The schema it was saved with.
        found: u32,
        /// The newest this build opens.
        supported: u32,
    },
}

/// Looks at project folders for [`startup_plan`]. The app's probe reads `project.toml` and the journal; tests answer
/// from a table.
pub trait Probe {
    /// What `project` holds.
    fn folder(&self, project: &Path) -> Folder;
}

/// Why the Welcome screen names a project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Notice {
    /// Omuzaki stopped without closing it: open it to restore it (the opt-out is `ask` or `welcome`, so this is a
    /// card rather than the recovery screen).
    Recover {
        /// The project folder.
        project: PathBuf,
    },
    /// The app stopped while it was opening this project last time: reopen it, or open its recovery copy.
    Interrupted {
        /// The project folder.
        project: PathBuf,
    },
    /// The folder is gone, or its drive is not connected. "Locate..." and "Remove from recent".
    Missing {
        /// The project folder.
        project: PathBuf,
    },
    /// The folder is there but cannot be opened.
    Unreadable {
        /// The project folder.
        project: PathBuf,
        /// Why.
        reason: String,
    },
    /// A newer Omuzaki saved it.
    Newer {
        /// The project folder.
        project: PathBuf,
        /// The schema it was saved with.
        found: u32,
        /// The newest this build opens.
        supported: u32,
    },
}

/// What the studio does at start.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "plan", rename_all = "snake_case")]
pub enum StartupPlan {
    /// Offer recovery for these projects, newest first; the chosen one opens and becomes the last project.
    Recover {
        /// The projects Omuzaki stopped without closing.
        projects: Vec<PathBuf>,
    },
    /// Run first-run setup, resuming at the saved step; `resume_project` is reopened for it when it can be.
    Setup {
        /// The project setup made before it stopped.
        resume_project: Option<PathBuf>,
    },
    /// Open the last project.
    Open {
        /// The project folder.
        project: PathBuf,
    },
    /// Show the Welcome screen.
    Welcome {
        /// Cards about projects that need the person, in the order to show them.
        notices: Vec<Notice>,
        /// The project to offer as "Reopen <name>" (`startup.reopen = ask`).
        offer: Option<PathBuf>,
    },
}

fn notice(project: &Path, folder: Folder) -> Option<Notice> {
    let project = project.to_path_buf();
    match folder {
        Folder::Ready => None,
        Folder::Missing => Some(Notice::Missing { project }),
        Folder::Unreadable { reason } => Some(Notice::Unreadable { project, reason }),
        Folder::Newer { found, supported } => Some(Notice::Newer { project, found, supported }),
    }
}

/// The projects the session knows, newest first: the recent list, then the last project if it is not in it.
fn known(session: &SessionState) -> Vec<&Path> {
    let mut known: Vec<&Path> = session.recent.iter().map(|r| r.path.as_path()).collect();
    if let Some(last) = session.last_project.as_deref().filter(|last| !known.contains(last)) {
        known.insert(0, last);
    }
    known
}

/// Decides what the studio does at start (see the [module docs](self)). `left_open` lists projects whose journal says
/// Omuzaki stopped without closing them (`omuzaki_doc::journal::left_open`); only those the session knows count, in
/// its order. `probe` is asked only about the project the plan would reopen, never about the whole recent list, so a
/// slow network drive costs one look.
pub fn startup_plan(session: &SessionState, reopen: Reopen, left_open: &[PathBuf], probe: &dyn Probe) -> StartupPlan {
    let crashed: Vec<PathBuf> =
        known(session).into_iter().filter(|p| left_open.iter().any(|l| l == p)).map(Path::to_path_buf).collect();
    let mut notices = Vec::new();
    if !crashed.is_empty() {
        // The opt-out has no meaning before setup has finished, so setup's own project is recovered first too.
        if reopen == Reopen::Last || session.setup_needed() {
            return StartupPlan::Recover { projects: crashed };
        }
        notices.extend(crashed.iter().map(|project| Notice::Recover { project: project.clone() }));
    }
    if session.setup_needed() {
        let resume_project = session.last_project.clone().filter(|p| probe.folder(p) == Folder::Ready);
        return StartupPlan::Setup { resume_project };
    }
    let last = session.last_project.as_deref().filter(|last| !crashed.iter().any(|c| c == last));
    if let Some(opening) = &session.opening {
        // It stopped while opening this one: open nothing (no crash loop), and say so.
        if !crashed.contains(opening) {
            notices.push(Notice::Interrupted { project: opening.clone() });
        }
        return StartupPlan::Welcome { notices, offer: None };
    }
    let Some(last) = last.filter(|_| reopen != Reopen::Welcome) else {
        return StartupPlan::Welcome { notices, offer: None };
    };
    match notice(last, probe.folder(last)) {
        Some(cannot) => {
            notices.push(cannot);
            StartupPlan::Welcome { notices, offer: None }
        }
        None if reopen == Reopen::Last => StartupPlan::Open { project: last.to_path_buf() },
        None => StartupPlan::Welcome { notices, offer: Some(last.to_path_buf()) },
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use omuzaki_settings_api::session::SetupProgress;

    use super::*;

    /// Answers from a table, `Ready` by default, and counts the looks.
    #[derive(Default)]
    struct Table {
        folders: BTreeMap<PathBuf, Folder>,
        looked: RefCell<Vec<PathBuf>>,
    }

    impl Table {
        fn with(mut self, project: &str, folder: Folder) -> Self {
            self.folders.insert(PathBuf::from(project), folder);
            self
        }
    }

    impl Probe for Table {
        fn folder(&self, project: &Path) -> Folder {
            self.looked.borrow_mut().push(project.to_path_buf());
            self.folders.get(project).cloned().unwrap_or(Folder::Ready)
        }
    }

    /// A session that has opened `/p/a` then `/p/b` (so `/p/b` is last) and finished setup.
    fn used() -> SessionState {
        let mut s = SessionState::default();
        s.opened(Path::new("/p/a"), "A", 1);
        s.opened(Path::new("/p/b"), "B", 2);
        s.set_setup(Some(SetupProgress::ended("go_live", vec![])));
        s
    }

    fn p(path: &str) -> PathBuf {
        PathBuf::from(path)
    }

    fn welcome(notices: Vec<Notice>, offer: Option<&str>) -> StartupPlan {
        StartupPlan::Welcome { notices, offer: offer.map(p) }
    }

    #[test]
    fn each_opt_out_with_a_healthy_last_project() {
        let table = Table::default();
        assert_eq!(startup_plan(&used(), Reopen::Last, &[], &table), StartupPlan::Open { project: p("/p/b") });
        assert_eq!(startup_plan(&used(), Reopen::Ask, &[], &table), welcome(vec![], Some("/p/b")));
        assert_eq!(startup_plan(&used(), Reopen::Welcome, &[], &table), welcome(vec![], None));
    }

    #[test]
    fn recovery_wins_over_every_opt_out_newest_first_and_ignores_projects_the_session_does_not_know() {
        let table = Table::default();
        let crashed = [p("/p/a"), p("/elsewhere"), p("/p/b")];
        assert_eq!(
            startup_plan(&used(), Reopen::Last, &crashed, &table),
            StartupPlan::Recover { projects: vec![p("/p/b"), p("/p/a")] }
        );
        let cards = vec![Notice::Recover { project: p("/p/b") }, Notice::Recover { project: p("/p/a") }];
        assert_eq!(startup_plan(&used(), Reopen::Ask, &crashed, &table), welcome(cards.clone(), None));
        assert_eq!(startup_plan(&used(), Reopen::Welcome, &crashed, &table), welcome(cards, None));
        // An older project crashed and the last one did not: the last one is still offered beside the card.
        let older = [p("/p/a")];
        assert_eq!(
            startup_plan(&used(), Reopen::Ask, &older, &table),
            welcome(vec![Notice::Recover { project: p("/p/a") }], Some("/p/b"))
        );
    }

    #[test]
    fn a_project_that_cannot_be_reopened_explains_itself_and_stays_in_recent() {
        let gone = Table::default().with("/p/b", Folder::Missing);
        let session = used();
        assert_eq!(
            startup_plan(&session, Reopen::Last, &[], &gone),
            welcome(vec![Notice::Missing { project: p("/p/b") }], None)
        );
        assert_eq!(session.recent.len(), 2, "the plan never edits the session");
        let unreadable = Table::default().with("/p/b", Folder::Unreadable { reason: "no project.toml".into() });
        assert_eq!(
            startup_plan(&used(), Reopen::Ask, &[], &unreadable),
            welcome(vec![Notice::Unreadable { project: p("/p/b"), reason: "no project.toml".into() }], None)
        );
        let newer = Table::default().with("/p/b", Folder::Newer { found: 9, supported: 1 });
        assert_eq!(
            startup_plan(&used(), Reopen::Last, &[], &newer),
            welcome(vec![Notice::Newer { project: p("/p/b"), found: 9, supported: 1 }], None)
        );
    }

    #[test]
    fn a_start_that_died_while_opening_shows_welcome_once_with_the_notice() {
        let mut session = used();
        session.begin_open(Path::new("/p/b"));
        let table = Table::default();
        assert_eq!(
            startup_plan(&session, Reopen::Last, &[], &table),
            welcome(vec![Notice::Interrupted { project: p("/p/b") }], None)
        );
        assert!(table.looked.borrow().is_empty(), "nothing is probed: nothing is opened");
    }

    #[test]
    fn an_empty_session_is_a_first_run_and_setup_ignores_the_opt_out() {
        let table = Table::default();
        for reopen in [Reopen::Last, Reopen::Ask, Reopen::Welcome] {
            assert_eq!(
                startup_plan(&SessionState::default(), reopen, &[], &table),
                StartupPlan::Setup { resume_project: None }
            );
        }
        // Setup stopped part way after making its project: it resumes with that project.
        let mut session = SessionState::default();
        session.opened(Path::new("/p/sample"), "Sample", 1);
        session.set_setup(Some(SetupProgress::new("input", vec![], false)));
        assert_eq!(
            startup_plan(&session, Reopen::Welcome, &[], &table),
            StartupPlan::Setup { resume_project: Some(p("/p/sample")) }
        );
        // An existing user from before setup (history, no setup record) is never shown it.
        let mut before = SessionState::default();
        before.opened(Path::new("/p/a"), "A", 1);
        assert_eq!(startup_plan(&before, Reopen::Last, &[], &table), StartupPlan::Open { project: p("/p/a") });
    }

    #[test]
    fn recovery_comes_before_setup() {
        let mut session = SessionState::default();
        session.opened(Path::new("/p/sample"), "Sample", 1);
        session.set_setup(Some(SetupProgress::new("output", vec![], false)));
        for reopen in [Reopen::Last, Reopen::Ask, Reopen::Welcome] {
            let plan = startup_plan(&session, reopen, &[p("/p/sample")], &Table::default());
            assert_eq!(plan, StartupPlan::Recover { projects: vec![p("/p/sample")] });
        }
    }

    #[test]
    fn only_the_project_to_reopen_is_probed() {
        let table = Table::default();
        let mut session = used();
        for i in 0..15 {
            session.opened(Path::new(&format!("/p/{i}")), "n", 10 + i);
        }
        startup_plan(&session, Reopen::Last, &[], &table);
        assert_eq!(*table.looked.borrow(), [p("/p/14")]);
    }

    #[test]
    fn the_plan_reads_as_json_with_a_tag() {
        let plan = welcome(vec![Notice::Missing { project: p("/p/b") }], None);
        let json = serde_json::to_value(&plan).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(json["plan"], "welcome");
        assert_eq!(json["notices"][0]["kind"], "missing");
    }
}
