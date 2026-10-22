//! The approvals store (design D11, #955): machine-local, keyed by show, written atomically; only a person approves;
//! a program is pinned to its content; a damaged file approves nothing; and the session ceiling never holds the
//! scopes no show may have.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::panic)]
// The damaged-file test writes its fixture file in a temporary folder.
#![allow(clippy::disallowed_methods)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use omuzaki_live::approvals::{ApprovalError, DiskApprovals, ceiling};
use omuzaki_live::compile::ProgramHasher;
use omuzaki_live_api::ids::ShowId;
use omuzaki_live_api::need::{Invocation, Need, NetScheme};
use omuzaki_live_api::services::{Approvals, ShowApprovals, ShowKey};
use omuzaki_ops_api::{Actor, Scope};
use omuzaki_store_api::ContentHash;

#[derive(Default)]
struct Files(Mutex<BTreeMap<String, Vec<u8>>>);

impl Files {
    fn put(&self, path: &str, bytes: &[u8]) {
        self.0.lock().unwrap().insert(path.to_owned(), bytes.to_vec());
    }
}

impl ProgramHasher for Files {
    fn hash(&self, path: &str) -> Option<ContentHash> {
        self.0.lock().unwrap().get(path).map(|b| ContentHash::of(b))
    }
}

fn key(project: &str) -> ShowKey {
    ShowKey::new(project, ShowId::new("main").unwrap())
}

fn person() -> Actor {
    Actor::Person { name: "Streamer".to_owned() }
}

fn host() -> Need {
    Need::host(NetScheme::Https, "example.com", 443)
}

#[test]
fn approvals_survive_a_restart_and_a_copied_project_has_none() {
    let dir = tempfile::tempdir().unwrap();
    let store = DiskApprovals::new(dir.path());
    let needs = [host(), Need::Scope(Scope::Broadcast), Need::op("obs.scene.set")];
    store.allow(&key("/projects/demo"), &needs, &["/projects/demo/overlay".to_owned()], &person()).unwrap();
    store.set_autostart(&key("/projects/demo"), true).unwrap();

    let again = DiskApprovals::new(dir.path());
    let approvals = again.show(&key("/projects/demo"));
    assert_eq!(approvals.granted.len(), 3);
    assert_eq!(approvals.roots, ["/projects/demo/overlay"]);
    assert!(approvals.autostart);
    assert_eq!(again.findings().len(), 0);
    // The same show in a copy of the project (another folder) starts with nothing.
    assert_eq!(again.show(&key("/projects/copy")), ShowApprovals::default());
}

#[test]
fn an_unsaved_projects_approvals_last_one_run() {
    let dir = tempfile::tempdir().unwrap();
    let store = DiskApprovals::new(dir.path());
    store.allow(&key(""), &[Need::op("trigger.press")], &[], &person()).unwrap();
    store.allow(&key("/projects/demo"), &[Need::op("obs.scene.set")], &[], &person()).unwrap();
    assert_eq!(store.show(&key("")).granted.len(), 1, "approved for this run");

    let again = DiskApprovals::new(dir.path());
    assert_eq!(again.show(&key("")), ShowApprovals::default(), "another unsaved project's show starts with nothing");
    assert_eq!(again.show(&key("/projects/demo")).granted.len(), 1);

    // A hand-edited entry for an unsaved project is ignored.
    let path = again.path().to_owned();
    let written = std::fs::read_to_string(&path).unwrap();
    let text = format!("{written}\n{}", written.replace("/projects/demo", ""));
    std::fs::write(&path, text).unwrap();
    let edited = DiskApprovals::new(dir.path());
    assert_eq!(edited.findings().len(), 0, "the edited file reads");
    assert_eq!(edited.show(&key("/projects/demo")).granted.len(), 1);
    assert_eq!(edited.show(&key("")), ShowApprovals::default());
}

#[test]
fn only_a_person_approves() {
    let dir = tempfile::tempdir().unwrap();
    let store = DiskApprovals::new(dir.path());
    let agent = Actor::Agent { id: "helper".to_owned(), model: None };
    for by in [agent, Actor::System] {
        assert_eq!(store.allow(&key("/p"), &[host()], &[], &by), Err(ApprovalError::NotAPerson));
    }
    assert_eq!(store.show(&key("/p")), ShowApprovals::default());
    assert!(!store.path().exists(), "nothing was written");
}

#[test]
fn a_program_is_trusted_only_with_the_content_it_was_approved_with() {
    let dir = tempfile::tempdir().unwrap();
    let files = Arc::new(Files::default());
    files.put("/bin/tool", b"version 1");
    let store = DiskApprovals::with_hasher(dir.path(), Arc::clone(&files) as Arc<dyn ProgramHasher>);
    let invocation = Invocation::new("/bin/tool");
    store.allow(&key("/p"), &[Need::Program(invocation.clone())], &[], &person()).unwrap();
    assert!(store.program_trusted(&invocation, &ContentHash::of(b"version 1"), None));
    assert!(!store.program_trusted(&invocation, &ContentHash::of(b"version 2"), None));
    // A program that can't be read can't be approved.
    let missing = Invocation::new("/bin/missing");
    assert_eq!(
        store.allow(&key("/p"), &[Need::Program(missing)], &[], &person()),
        Err(ApprovalError::Unreadable("/bin/missing".to_owned()))
    );
    // Revoking takes the pin with it.
    store.revoke(&key("/p"), &Need::Program(invocation.clone())).unwrap();
    assert!(!store.program_trusted(&invocation, &ContentHash::of(b"version 1"), None));
}

#[test]
fn a_damaged_file_approves_nothing_with_a_finding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("automation").join("approvals.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "shows = = [").unwrap();
    let store = DiskApprovals::new(dir.path());
    assert_eq!(store.show(&key("/p")), ShowApprovals::default());
    assert_eq!(store.findings().len(), 1);
    // The next approval writes a fresh file.
    store.allow(&key("/p"), &[host()], &[], &person()).unwrap();
    assert_eq!(DiskApprovals::new(dir.path()).show(&key("/p")).granted.len(), 1);
}

#[test]
fn the_ceiling_is_the_base_plus_what_was_approved_and_never_the_forbidden_scopes() {
    let mut approvals = ShowApprovals::default().with_root("/projects/demo/overlay");
    for scope in [Scope::Broadcast, Scope::Native, Scope::Input, Scope::EditLibrary, Scope::EditSettings] {
        approvals = approvals.with_granted(Need::Scope(scope));
    }
    let grants = ceiling(&approvals);
    assert!(grants.allows(Scope::Read) && grants.allows(Scope::Perform) && grants.allows(Scope::Broadcast));
    for never in [Scope::Native, Scope::Input, Scope::EditLibrary, Scope::EditSettings] {
        assert!(!grants.allows(never), "{never}");
    }
    // Files only through the chosen folders: no folder, no fs.
    assert!(grants.allows(Scope::Fs));
    let base = ceiling(&ShowApprovals::default());
    assert!(!base.allows(Scope::Broadcast) && !base.allows(Scope::Fs));
}
