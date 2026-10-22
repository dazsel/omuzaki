//! The session across opens and crashes (settings.md D9, first-run.md §6): the studio's open writes the crash-loop
//! guard before the load and records the project in Open Recent after it; a headless open writes nothing; and the
//! startup plan over real folders recovers a crashed project first, opens a healthy last project, and explains one
//! that is gone.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::path::Path;
use std::sync::Arc;

use omuzaki_app::startup::{FolderProbe, left_open_projects, plan};
use omuzaki_app::{AppCore, AppOptions};
use omuzaki_core::time::FakeClock;
use omuzaki_ops_api::{Actor, ServicesBuilder};
use omuzaki_settings::startup::{Folder, Notice, Probe, StartupPlan};
use omuzaki_settings_api::service::Settings;
use omuzaki_settings_api::session::SetupProgress;
use omuzaki_settings_mock::MemorySettings;

fn person() -> Actor {
    Actor::Person { name: "pat".into() }
}

fn settings() -> Arc<MemorySettings> {
    let settings = Arc::new(MemorySettings::with_clock(Arc::new(FakeClock::default())));
    // Setup is behind the person: these tests are about reopening.
    settings.session().set_setup(Some(SetupProgress::ended("go_live", vec![]))).unwrap();
    settings
}

fn options(settings: &Arc<MemorySettings>) -> AppOptions {
    let services = ServicesBuilder::new().insert::<dyn Settings>(settings.clone()).build();
    AppOptions::studio(person()).with_services(services)
}

#[test]
fn the_studio_records_an_open_and_a_headless_host_records_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let show = tmp.path().join("show");
    let settings = settings();
    AppCore::create(&show, "Spring show", options(&settings)).unwrap().close().unwrap();
    let state = settings.session().state();
    assert_eq!(state.opening, None, "the guard is cleared once the project is open");
    assert_eq!(state.last_project.as_deref(), Some(show.as_path()));
    assert_eq!(state.recent[0].name, "Spring show");

    let quiet = self::settings();
    let headless = AppOptions::headless(person())
        .with_services(ServicesBuilder::new().insert::<dyn Settings>(quiet.clone()).build());
    AppCore::open(&show, headless).unwrap().close().unwrap();
    assert!(quiet.session().state().recent.is_empty(), "the CLI and MCP never write the session");
}

#[test]
fn the_plan_recovers_a_crashed_project_opens_a_healthy_one_and_explains_a_missing_one() {
    let tmp = tempfile::tempdir().unwrap();
    let show = tmp.path().join("show");
    let settings = settings();
    AppCore::create(&show, "Show", options(&settings)).unwrap().close().unwrap();
    assert_eq!(plan(settings.as_ref()), StartupPlan::Open { project: show.clone() });

    // A crash: the journal is left without its close record.
    let core = AppCore::open(&show, options(&settings)).unwrap();
    core.abandon();
    assert_eq!(left_open_projects(&settings.session().state()), std::slice::from_ref(&show));
    assert_eq!(plan(settings.as_ref()), StartupPlan::Recover { projects: vec![show.clone()] });
    AppCore::open(&show, options(&settings)).unwrap().close().unwrap();
    assert_eq!(plan(settings.as_ref()), StartupPlan::Open { project: show.clone() }, "recovered, then closed");

    std::fs::rename(&show, tmp.path().join("moved")).unwrap();
    assert_eq!(
        plan(settings.as_ref()),
        StartupPlan::Welcome { notices: vec![Notice::Missing { project: show.clone() }], offer: None }
    );
    assert_eq!(settings.session().state().recent.len(), 1, "it stays in Open Recent until the person removes it");
}

#[test]
fn the_folder_probe_judges_project_toml_without_opening() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(FolderProbe.folder(&tmp.path().join("gone")), Folder::Missing);
    assert!(matches!(FolderProbe.folder(tmp.path()), Folder::Unreadable { .. }), "no project.toml");
    std::fs::write(tmp.path().join("project.toml"), "name = \"Later\"\nschema = 4000\n").unwrap();
    assert!(matches!(FolderProbe.folder(tmp.path()), Folder::Newer { found: 4000, .. }));
    std::fs::write(tmp.path().join("project.toml"), "name = \"Now\"\nschema = 1\n").unwrap();
    assert_eq!(FolderProbe.folder(Path::new(tmp.path())), Folder::Ready);
}

#[test]
fn a_session_that_cannot_record_the_open_refuses_to_open() {
    // The crash-loop guard can't be made durable (the data folder is a file), so the load never starts.
    let tmp = tempfile::tempdir().unwrap();
    let show = tmp.path().join("show");
    omuzaki_doc::Project::create(&show, "Show").unwrap();
    let blocked = tmp.path().join("data");
    std::fs::write(&blocked, b"a file, not a folder").unwrap();
    let store =
        Arc::new(omuzaki_settings::Store::open(&omuzaki_settings::Paths::new(tmp.path().join("config"), blocked)));
    let services = ServicesBuilder::new().insert::<dyn Settings>(store).build();
    let refused = AppCore::open(&show, AppOptions::studio(person()).with_services(services));
    assert!(matches!(refused, Err(omuzaki_app::AppError::Session(_))), "{:?}", refused.err());
}
