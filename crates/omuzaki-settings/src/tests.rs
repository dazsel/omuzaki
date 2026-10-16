//! Behaviour of the store against real folders: load, write, migrate, race and recover.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use omuzaki_ops_api::{Actor, Mode};
use omuzaki_settings_api::error::{SettingError, SettingErrorKind};
use omuzaki_settings_api::event::{CHANGE_LOG_CAPACITY, ChangeCause};
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::profile::ProfileSource;
use omuzaki_settings_api::registry::SettingRegistration;
use omuzaki_settings_api::service::{Layer, ListFilter, Settings, SettingsExt, Source};
use omuzaki_settings_api::setting::Setting;
use serde_json::{Value, json};

use crate::backend::FileBackend;
use crate::{Paths, Store};

macro_rules! setting {
    ($name:ident, $key:literal, $ty:ty, $default:expr, $scope:expr, $apply:expr, $version:literal) => {
        struct $name;
        impl Setting for $name {
            type Value = $ty;
            const META: SettingMeta = SettingMeta::new(
                $key,
                "a test setting",
                $scope,
                AgentWrite::Free,
                InProfile::Excluded,
                $apply,
                $version,
            );
            fn default() -> $ty {
                $default
            }
        }
    };
}

setting!(Theme, "ui.theme", String, "dark".to_owned(), SettingScope::User, Apply::Live, 1);
setting!(Tier, "quality.tier", String, "live".to_owned(), SettingScope::User, Apply::Live, 1);
setting!(Device, "audio.device", String, "default".to_owned(), SettingScope::Machine, Apply::Live, 1);
setting!(Channel, "update.channel", String, "stable".to_owned(), SettingScope::Machine, Apply::NextStart, 1);

struct Scale;
impl Setting for Scale {
    type Value = u16;
    const META: SettingMeta = SettingMeta::new(
        "ui.scale",
        "UI scale",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Excluded,
        Apply::Live,
        1,
    );
    fn default() -> u16 {
        100
    }
    fn check(v: &u16) -> Result<(), SettingError> {
        SettingError::range("ui.scale", *v, 75, 200)
    }
}

/// Stored as `{ percent = n }` at version 1, `"n%"` at version 2 and a plain number now.
struct Migrated;
impl Setting for Migrated {
    type Value = u32;
    const META: SettingMeta = SettingMeta::new(
        "ui.mig",
        "migrated",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Excluded,
        Apply::Live,
        3,
    );
    fn default() -> u32 {
        100
    }
    fn migrate(from: u32, old: Value) -> Result<Value, SettingError> {
        let bad = || SettingError::invalid("ui.mig", format!("cannot read {old} stored at version {from}"));
        match from {
            1 => old.get("percent").cloned().ok_or_else(bad),
            2 => old
                .as_str()
                .and_then(|s| s.strip_suffix('%'))
                .and_then(|n| n.parse::<u32>().ok())
                .map(Value::from)
                .ok_or_else(bad),
            _ => Err(bad()),
        }
    }
}

struct Renamed;
impl Setting for Renamed {
    type Value = i32;
    const META: SettingMeta = SettingMeta::new(
        "ui.new_name",
        "renamed",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Excluded,
        Apply::Live,
        1,
    );
    const RENAMED_FROM: &'static [&'static str] = &["ui.old_name"];
    fn default() -> i32 {
        0
    }
}

fn registrations() -> Vec<&'static SettingRegistration> {
    static ALL: OnceLock<Vec<SettingRegistration>> = OnceLock::new();
    ALL.get_or_init(|| {
        let p = "omuzaki-settings";
        vec![
            SettingRegistration::of::<Theme>(p),
            SettingRegistration::of::<Tier>(p),
            SettingRegistration::of::<Device>(p),
            SettingRegistration::of::<Channel>(p),
            SettingRegistration::of::<Scale>(p),
            SettingRegistration::of::<Migrated>(p),
            SettingRegistration::of::<Renamed>(p),
        ]
    })
    .iter()
    .collect()
}

struct Rig {
    _dir: tempfile::TempDir,
    paths: Paths,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::new(dir.path().join("config"), dir.path().join("data"));
        Self { _dir: dir, paths }
    }

    fn open(&self) -> Store {
        Store::open_with(&self.paths, registrations())
    }

    fn user_file(&self) -> PathBuf {
        self.paths.config().join("settings.toml")
    }

    fn machine_file(&self) -> PathBuf {
        self.paths.data().join("machine.toml")
    }

    fn write_user(&self, text: &str) {
        fs::create_dir_all(self.paths.config()).unwrap();
        fs::write(self.user_file(), text).unwrap();
    }

    fn user_text(&self) -> String {
        fs::read_to_string(self.user_file()).unwrap()
    }

    fn names_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }
}

fn person() -> Actor {
    Actor::Person { name: "tester".into() }
}

fn set<S: Setting>(store: &Store, value: &S::Value) {
    store.set::<S>(value, Layer::Auto, Mode::Apply, &person()).unwrap();
}

fn get<S: Setting>(store: &Store) -> S::Value {
    store.get::<S>().unwrap()
}

#[test]
fn a_new_store_reads_every_default_from_nothing() {
    let rig = Rig::new();
    let store = rig.open();
    assert_eq!(get::<Scale>(&store), 100);
    assert_eq!(get::<Device>(&store), "default");
    let snapshot = store.snapshot();
    assert_eq!(snapshot.iter().count(), registrations().len());
    assert!(snapshot.iter().all(|(_, _, source)| *source == Source::Default));
    assert_eq!(store.findings(), Vec::new());
    drop(store);
    assert!(!rig.user_file().exists(), "nothing changed, so nothing is written");
}

#[test]
fn a_change_reaches_the_snapshot_at_once_and_the_file_after_the_debounce() {
    let rig = Rig::new();
    let store = rig.open();
    set::<Scale>(&store, &125);
    assert_eq!(get::<Scale>(&store), 125);
    assert!(!rig.user_file().exists(), "the writer waits for the debounce");
    let start = Instant::now();
    while !rig.user_file().exists() {
        assert!(start.elapsed() < crate::MAX_DELAY * 3, "the writer thread never wrote");
        thread::sleep(Duration::from_millis(10));
    }
    drop(store);
    assert!(rig.user_text().contains("scale = 125"));
    assert_eq!(get::<Scale>(&rig.open()), 125, "the next start reads it back");
}

#[test]
fn dropping_the_store_flushes_and_user_and_machine_keys_go_to_their_own_folders() {
    let rig = Rig::new();
    let store = rig.open();
    set::<Theme>(&store, &"light".to_owned());
    set::<Device>(&store, &"mic-1".to_owned());
    drop(store);
    let user = rig.user_text();
    let machine = fs::read_to_string(rig.machine_file()).unwrap();
    assert!(user.contains("theme = \"light\"") && !user.contains("mic-1"), "{user}");
    assert!(machine.contains("device = \"mic-1\"") && !machine.contains("light"), "{machine}");
    assert!(user.starts_with("format = 1"), "a new file carries its format marker: {user}");
}

#[test]
fn comments_order_and_unknown_keys_survive_a_write() {
    let rig = Rig::new();
    let original = "# my settings\nformat = 1\n\n[ui]\n# bigger please\nscale = 125 # my pick\ntheme = \"dark\"\n\n\
                    [future_feature]   # a newer build wrote this\nlevel = 3\nname = 'x'\n";
    rig.write_user(original);
    let store = rig.open();
    assert_eq!(get::<Scale>(&store), 125);
    set::<Scale>(&store, &130);
    set::<Tier>(&store, &"high".to_owned());
    store.flush().unwrap();
    let expected = original.replace("scale = 125 # my pick", "scale = 130 # my pick");
    let text = rig.user_text();
    assert!(text.starts_with(&expected), "everything the writer did not touch is byte-identical: {text}");
    assert!(text[expected.len()..].contains("[quality]") && text.contains("tier = \"high\""), "{text}");
}

#[test]
fn a_bad_value_stays_in_the_file_reads_as_default_and_is_reported_until_written_over() {
    let rig = Rig::new();
    rig.write_user("[ui]\nscale = 900\n");
    let store = rig.open();
    assert_eq!(get::<Scale>(&store), 100);
    let findings = store.findings();
    assert_eq!(findings.len(), 1);
    assert_eq!((findings[0].error.key.as_str(), &findings[0].stored), ("ui.scale", &json!(900)));
    set::<Theme>(&store, &"light".to_owned());
    store.flush().unwrap();
    assert!(rig.user_text().contains("scale = 900"), "an unrelated write leaves the bad value: {}", rig.user_text());
    set::<Scale>(&store, &110);
    assert_eq!(store.findings(), Vec::new());
    store.flush().unwrap();
    assert!(rig.user_text().contains("scale = 110"));
}

#[test]
fn a_file_that_does_not_parse_is_renamed_and_start_goes_on_with_defaults() {
    let rig = Rig::new();
    rig.write_user("[ui\nscale = ");
    let store = rig.open();
    assert_eq!(get::<Scale>(&store), 100);
    let names = Rig::names_in(rig.paths.config());
    let broken: Vec<_> = names.iter().filter(|n| n.starts_with("settings.toml.broken-")).collect();
    assert_eq!(broken.len(), 1, "{names:?}");
    assert!(!rig.user_file().exists());
    assert!(store.findings()[0].error.to_string().contains("does not parse"));
    set::<Scale>(&store, &120);
    store.flush().unwrap();
    assert!(rig.user_text().contains("scale = 120"));
    assert_eq!(fs::read_to_string(rig.paths.config().join(broken[0])).unwrap(), "[ui\nscale = ");
}

#[test]
fn a_file_that_breaks_after_start_is_moved_aside_by_the_next_write() {
    let rig = Rig::new();
    let store = rig.open();
    rig.write_user("not = [toml");
    set::<Scale>(&store, &120);
    store.flush().unwrap();
    assert!(rig.user_text().contains("scale = 120"));
    assert!(Rig::names_in(rig.paths.config()).iter().any(|n| n.starts_with("settings.toml.broken-")));
}

#[test]
fn a_value_from_every_earlier_version_is_migrated_to_the_same_current_value() {
    let fixtures = [
        ("version 1", "[ui]\nmig = { percent = 125 }\n"),
        ("version 2", "[ui]\nmig = \"125%\"\n\n[versions]\n\"ui.mig\" = 2\n"),
        ("version 3", "[ui]\nmig = 125\n\n[versions]\n\"ui.mig\" = 3\n"),
    ];
    for (name, text) in fixtures {
        let rig = Rig::new();
        rig.write_user(text);
        let store = rig.open();
        assert_eq!(get::<Migrated>(&store), 125, "{name}");
        assert!(store.findings().is_empty(), "{name}: {:?}", store.findings());
    }
}

#[test]
fn the_first_write_after_a_migration_copies_the_file_once_and_stamps_the_new_version() {
    let rig = Rig::new();
    let old = "[ui]\nmig = \"125%\"\n\n[versions]\n\"ui.mig\" = 2\n";
    rig.write_user(old);
    let store = rig.open();
    let backup = rig.paths.config().join(format!("settings.toml.bak-{}", env!("CARGO_PKG_VERSION")));
    assert!(!backup.exists(), "migration alone writes nothing");
    set::<Migrated>(&store, &150);
    store.flush().unwrap();
    assert_eq!(fs::read_to_string(&backup).unwrap(), old);
    assert!(rig.user_text().contains("mig = 150") && rig.user_text().contains("\"ui.mig\" = 3"), "{}", rig.user_text());
    set::<Theme>(&store, &"light".to_owned());
    store.flush().unwrap();
    assert_eq!(fs::read_to_string(&backup).unwrap(), old, "one copy per app version");
}

#[test]
fn a_failed_migration_reports_and_keeps_the_stored_value() {
    let rig = Rig::new();
    rig.write_user("[ui]\nmig = \"garbage\"\n\n[versions]\n\"ui.mig\" = 2\n");
    let store = rig.open();
    assert_eq!(get::<Migrated>(&store), 100);
    assert_eq!(store.findings().len(), 1);
    set::<Theme>(&store, &"light".to_owned());
    store.flush().unwrap();
    assert!(rig.user_text().contains("mig = \"garbage\""));
}

#[test]
fn a_newer_version_is_kept_verbatim_never_migrated_down_or_overwritten_by_a_default() {
    let rig = Rig::new();
    let text = "[ui]\nmig = { from_the_future = true }\n\n[versions]\n\"ui.mig\" = 9\n";
    rig.write_user(text);
    let store = rig.open();
    assert_eq!(get::<Migrated>(&store), 100);
    assert!(store.findings()[0].error.to_string().contains("version 9"));
    set::<Theme>(&store, &"light".to_owned());
    store.flush().unwrap();
    let after = rig.user_text();
    assert!(after.contains("mig = { from_the_future = true }") && after.contains("\"ui.mig\" = 9"), "{after}");
}

#[test]
fn a_renamed_key_moves_on_load_unless_the_new_key_exists() {
    let rig = Rig::new();
    rig.write_user("[ui]\nold_name = 7\n");
    let store = rig.open();
    assert_eq!(get::<Renamed>(&store), 7);
    drop(store);
    let text = rig.user_text();
    assert!(text.contains("new_name = 7") && !text.contains("old_name"), "{text}");

    let rig = Rig::new();
    rig.write_user("[ui]\nold_name = 7\nnew_name = 3\n");
    let store = rig.open();
    assert_eq!(get::<Renamed>(&store), 3);
    drop(store);
    assert!(rig.user_text().contains("old_name = 7"), "the old value is kept, not merged");
}

#[test]
fn two_stores_on_one_folder_are_last_writer_wins_per_key_not_per_file() {
    let rig = Rig::new();
    let (a, b) = (rig.open(), rig.open());
    set::<Scale>(&a, &110);
    set::<Theme>(&b, &"light".to_owned());
    a.flush().unwrap();
    b.flush().unwrap();
    let text = rig.user_text();
    assert!(text.contains("scale = 110") && text.contains("theme = \"light\""), "both keys survive: {text}");
    set::<Scale>(&a, &120);
    a.flush().unwrap();
    set::<Scale>(&b, &130);
    b.flush().unwrap();
    assert!(rig.user_text().contains("scale = 130") && rig.user_text().contains("theme = \"light\""));
    assert_eq!(get::<Scale>(&rig.open()), 130);
}

#[test]
fn a_store_paused_after_its_read_cannot_lose_the_update_another_store_writes_meanwhile() {
    let rig = Rig::new();
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let release_rx = Mutex::new(release_rx);
    let entered_tx = Mutex::new(entered_tx);
    let first = AtomicBool::new(true);
    let mut backend = FileBackend::new(rig.paths.clone());
    backend.after_read = Some(Box::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            entered_tx.lock().unwrap().send(()).unwrap();
            release_rx.lock().unwrap().recv().unwrap();
        }
    }));
    let a = Store::with_backend(Box::new(backend), registrations());
    let b = rig.open();
    set::<Scale>(&a, &110);
    // A's writer has now read the file and holds the lock, paused.
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    set::<Theme>(&b, &"light".to_owned());
    thread::scope(|scope| {
        let b_write = scope.spawn(|| b.flush());
        thread::sleep(Duration::from_millis(200));
        assert!(!b_write.is_finished(), "B waits for A's lock instead of merging from the same old text");
        assert!(!rig.user_file().exists());
        release_tx.send(()).unwrap();
        b_write.join().unwrap().unwrap();
    });
    drop(a);
    let text = rig.user_text();
    assert!(text.contains("scale = 110") && text.contains("theme = \"light\""), "neither update is lost: {text}");
}

/// The other half of the lock test: run as a child process by the test below, it holds the lock until its stdin
/// closes. Run on its own it does nothing.
#[test]
fn lock_holder_child() {
    let Some(path) = std::env::var_os("OMUZAKI_TEST_HOLD_LOCK") else { return };
    let file = fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path).unwrap();
    file.lock().unwrap();
    println!("LOCKED");
    drop(std::io::stdin().read_to_end(&mut Vec::new()));
}

struct Holder(Child);

impl Drop for Holder {
    fn drop(&mut self) {
        drop(self.0.kill());
        drop(self.0.wait());
    }
}

#[test]
fn a_lock_held_by_another_process_gives_a_bounded_wait_pending_keys_and_a_reported_error() {
    let rig = Rig::new();
    fs::create_dir_all(rig.paths.config()).unwrap();
    let lock = rig.paths.config().join("settings.toml.lock");
    let child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tests::lock_holder_child", "--nocapture"])
        .env("OMUZAKI_TEST_HOLD_LOCK", &lock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut holder = Holder(child);
    let stdout = holder.0.stdout.take().unwrap();
    let locked = BufReader::new(stdout).lines().map_while(Result::ok).any(|line| line == "LOCKED");
    assert!(locked, "the child never took the lock");

    let wait = Duration::from_millis(300);
    let store =
        Store::with_backend(Box::new(FileBackend::new(rig.paths.clone()).with_lock_wait(wait)), registrations());
    set::<Scale>(&store, &110);
    let start = Instant::now();
    let error = store.flush().unwrap_err();
    assert!(start.elapsed() >= wait && start.elapsed() < wait * 20, "{:?}", start.elapsed());
    assert!(matches!(error.kind, SettingErrorKind::Storage { .. }) && error.to_string().contains("another process"));
    assert!(!rig.user_file().exists());
    assert_eq!(get::<Scale>(&store), 110, "the running value is unaffected");
    assert!(store.findings().iter().any(|f| f.from == "write"), "the error is reported");

    drop(holder.0.stdin.take());
    holder.0.wait().unwrap();
    store.flush().unwrap();
    assert!(rig.user_text().contains("scale = 110"), "the keys stayed dirty and now land");
    assert_eq!(store.findings(), Vec::new());
}

#[test]
fn set_validates_and_names_the_key() {
    let rig = Rig::new();
    let store = rig.open();
    let by = person();
    let range = store.set_raw("ui.scale", json!(900), Layer::Auto, Mode::Apply, &by).unwrap_err();
    assert_eq!((range.key.as_str(), range.kind.id()), ("ui.scale", "out_of_range"));
    let kind = store.set_raw("ui.scale", json!("big"), Layer::Auto, Mode::Apply, &by).unwrap_err().kind;
    assert_eq!(kind.id(), "wrong_type");
    assert_eq!(
        store.set_raw("nope.key", json!(1), Layer::Auto, Mode::Apply, &by).unwrap_err().kind.id(),
        "unknown_key"
    );
    assert_eq!(get::<Scale>(&store), 100);
}

#[test]
fn a_change_is_one_event_a_no_change_is_none_and_a_dry_run_touches_nothing() {
    let rig = Rig::new();
    let store = rig.open();
    let events = store.subscribe();
    let by = person();
    let dry = store.set_raw("ui.scale", json!(120), Layer::Auto, Mode::DryRun, &by).unwrap();
    assert!(events.next().is_none() && store.changes(10).is_empty());
    assert_eq!(get::<Scale>(&store), 100);
    let real = store.set_raw("ui.scale", json!(120), Layer::Auto, Mode::Apply, &by).unwrap();
    assert_eq!(dry, real);
    let event = events.next().unwrap();
    assert_eq!(event.cause, ChangeCause::Set);
    assert_eq!(event.change_of("ui.scale").map(|c| (&c.before, &c.after)), Some((&json!(100), &json!(120))));
    store.set_raw("ui.scale", json!(120), Layer::Auto, Mode::Apply, &by).unwrap();
    assert!(events.next().is_none(), "same value, no event");
    assert_eq!(store.changes(10).len(), 1);
    assert_eq!(store.changes(10)[0].actor, by);
}

#[test]
fn reset_falls_back_to_the_default_and_removes_the_key_from_the_file() {
    let rig = Rig::new();
    rig.write_user("[ui]\nscale = 125\ntheme = \"light\"\n");
    let store = rig.open();
    let by = person();
    let events = store.subscribe();
    assert_eq!(store.reset("ui.scale", Layer::Auto, Mode::DryRun, &by).unwrap().map(|c| c.after), Some(json!(100)));
    assert_eq!(get::<Scale>(&store), 125);
    let change = store.reset("ui.scale", Layer::Auto, Mode::Apply, &by).unwrap().unwrap();
    assert_eq!((change.before, change.after), (json!(125), json!(100)));
    assert_eq!(events.next().unwrap().cause, ChangeCause::Reset);
    assert_eq!(store.reset("ui.scale", Layer::Auto, Mode::Apply, &by).unwrap(), None);
    store.flush().unwrap();
    assert_eq!(rig.user_text(), "[ui]\ntheme = \"light\"\n");
}

#[test]
fn the_change_log_keeps_the_last_200_newest_first() {
    let rig = Rig::new();
    let store = rig.open();
    for n in 0..(CHANGE_LOG_CAPACITY + 50) {
        set::<Scale>(&store, &u16::try_from(75 + n % 100).unwrap());
    }
    let log = store.changes(usize::MAX);
    assert_eq!(log.len(), CHANGE_LOG_CAPACITY);
    assert_eq!(log[0].change.after, json!(75 + (CHANGE_LOG_CAPACITY + 49) % 100));
    assert_eq!(store.changes(3).len(), 3);
}

#[test]
fn list_and_resolve_report_default_source_and_modified() {
    let rig = Rig::new();
    rig.write_user("[ui]\nscale = 125\n");
    let store = rig.open();
    let scale = store.resolve("ui.scale").unwrap();
    assert_eq!((scale.value, scale.default, scale.source), (json!(125), json!(100), Source::Base));
    assert_eq!(store.resolve("ui.theme").unwrap().source, Source::Default);
    let changed = store.list(&ListFilter::all().only_changed());
    assert_eq!(changed.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["ui.scale"]);
    let ui: Vec<_> = store.list(&ListFilter::all().with_area("ui")).iter().map(|r| r.key.clone()).collect();
    assert_eq!(ui, ["ui.mig", "ui.new_name", "ui.scale", "ui.theme"]);
    assert_eq!(store.resolve("nope.key").unwrap_err().kind.id(), "unknown_key");
}

#[test]
fn reload_applies_a_hand_edit_as_one_event_and_a_second_reload_changes_nothing() {
    let rig = Rig::new();
    let store = rig.open();
    let events = store.subscribe();
    let by = person();
    fs::create_dir_all(rig.paths.data()).unwrap();
    rig.write_user("[ui]\nscale = 150\ntheme = \"light\"\n");
    fs::write(rig.machine_file(), "[update]\nchannel = \"beta\"\n").unwrap();
    let preview = store.reload(Mode::DryRun, &by).unwrap();
    assert!(events.next().is_none() && get::<Scale>(&store) == 100);
    let report = store.reload(Mode::Apply, &by).unwrap();
    assert_eq!(report, preview);
    assert_eq!((report.changed, report.pending_restart.as_slice()), (3, ["update.channel".to_owned()].as_slice()));
    let event = events.next().unwrap();
    assert_eq!((event.cause, event.changes.len()), (ChangeCause::Reloaded, 3));
    assert!(events.next().is_none());
    assert_eq!(get::<Scale>(&store), 150);
    assert_eq!(store.reload(Mode::Apply, &by).unwrap().changed, 0);
    assert!(events.next().is_none(), "nothing differs, so no event");
}

#[test]
fn reload_keeps_a_change_that_is_not_written_yet() {
    let rig = Rig::new();
    let store = rig.open();
    set::<Scale>(&store, &111);
    rig.write_user("[ui]\nscale = 150\ntheme = \"light\"\n");
    store.reload(Mode::Apply, &person()).unwrap();
    assert_eq!((get::<Scale>(&store), get::<Theme>(&store)), (111, "light".to_owned()));
}

#[test]
fn profiles_are_not_stored_and_session_state_is_kept_in_memory() {
    let rig = Rig::new();
    let store = rig.open();
    assert_eq!(store.profiles(), Vec::new());
    assert_eq!(store.switch_profile(None, Mode::Apply, &person()).unwrap().changed, 0);
    assert!(store.save_profile("x", ProfileSource::Current, false, Mode::Apply, &person()).is_err());
    store.session().begin_open(Path::new("/p/a")).unwrap();
    assert_eq!(store.session().state().opening.as_deref(), Some(Path::new("/p/a")));
}

#[test]
fn nothing_is_moved_aside_when_the_other_file_cannot_be_read() {
    let rig = Rig::new();
    rig.write_user("[ui\nscale = ");
    // A folder where machine.toml should be: reading it fails.
    fs::create_dir_all(rig.machine_file()).unwrap();
    let store = rig.open();
    assert!(rig.user_file().exists(), "start-up leaves the broken file for a later pass");
    assert!(store.findings().iter().any(|f| f.error.to_string().contains("left in place")), "{:?}", store.findings());
    let error = store.reload(Mode::Apply, &person()).unwrap_err();
    assert!(matches!(error.kind, SettingErrorKind::Storage { .. }));
    assert!(rig.user_file().exists(), "a failed reload changes nothing on disk");
    assert!(!Rig::names_in(rig.paths.config()).iter().any(|n| n.contains(".broken-")));
}

/// A store whose writer pauses inside its first write, after the file is read and while the lock is held.
fn paused_store(rig: &Rig, on_first: impl Fn() + Send + Sync + 'static) -> Store {
    let first = AtomicBool::new(true);
    let mut backend = FileBackend::new(rig.paths.clone());
    backend.after_read = Some(Box::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            on_first();
        }
    }));
    Store::with_backend(Box::new(backend), registrations())
}

#[test]
fn a_reload_waits_for_a_flush_in_progress_and_then_keeps_what_it_wrote() {
    let rig = Rig::new();
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (entered_tx, release_rx) = (Mutex::new(entered_tx), Mutex::new(release_rx));
    let store = paused_store(&rig, move || {
        entered_tx.lock().unwrap().send(()).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
    });
    set::<Scale>(&store, &111);
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    thread::scope(|scope| {
        let reload = scope.spawn(|| store.reload(Mode::Apply, &person()));
        thread::sleep(Duration::from_millis(200));
        assert!(!reload.is_finished(), "the reload must not read the file while a flush is about to replace it");
        release_tx.send(()).unwrap();
        reload.join().unwrap().unwrap();
    });
    assert_eq!(get::<Scale>(&store), 111);
    store.flush().unwrap();
    assert!(rig.user_text().contains("scale = 111"));
    assert_eq!(get::<Scale>(&store), 111, "the snapshot did not go back to the old file text");
}

#[test]
fn a_reload_drops_the_finding_for_a_value_a_pending_edit_replaces() {
    let rig = Rig::new();
    let (entered_tx, entered_rx) = mpsc::channel::<()>();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let (entered_tx, release_rx) = (Mutex::new(entered_tx), Mutex::new(release_rx));
    // The writer is held inside its first flush, so the edit stays pending while the reload runs.
    let store = paused_store(&rig, move || {
        entered_tx.lock().unwrap().send(()).unwrap();
        release_rx.lock().unwrap().recv().unwrap();
    });
    set::<Theme>(&store, &"light".to_owned());
    entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    release_tx.send(()).unwrap();
    store.flush().unwrap();
    rig.write_user("[ui]\nscale = 900\ntheme = \"light\"\n");
    set::<Scale>(&store, &111);
    store.reload(Mode::Apply, &person()).unwrap();
    assert_eq!(get::<Scale>(&store), 111);
    assert_eq!(store.findings(), Vec::new(), "the bad stored 900 is about to be written over");
}

#[test]
fn reset_removes_the_retired_name_too_so_reopening_does_not_bring_it_back() {
    let rig = Rig::new();
    rig.write_user("[ui]\nold_name = 7\nnew_name = 3\n");
    let store = rig.open();
    assert_eq!(get::<Renamed>(&store), 3);
    store.reset("ui.new_name", Layer::Auto, Mode::Apply, &person()).unwrap();
    drop(store);
    assert!(!rig.user_text().contains("old_name") && !rig.user_text().contains("new_name"), "{}", rig.user_text());
    assert_eq!(get::<Renamed>(&rig.open()), 0);
}

#[test]
fn a_versions_item_that_is_not_a_table_fails_the_write_instead_of_dropping_the_stamp() {
    let rig = Rig::new();
    rig.write_user("versions = 7\n");
    let store = rig.open();
    set::<Migrated>(&store, &150);
    let error = store.flush().unwrap_err();
    assert!(error.to_string().contains("versions"), "{error}");
    assert_eq!(rig.user_text(), "versions = 7\n", "nothing was half-written");
    assert!(store.findings().iter().any(|f| f.from == "write"));
    assert_eq!(get::<Migrated>(&store), 150);
}

#[test]
fn a_file_found_broken_during_a_write_is_reported_as_a_finding() {
    let rig = Rig::new();
    let store = rig.open();
    rig.write_user("not = [toml");
    set::<Scale>(&store, &120);
    store.flush().unwrap();
    let findings = store.findings();
    let moved = findings.iter().find(|f| f.from == "settings.toml").expect("a finding for the moved file");
    assert!(moved.error.to_string().contains("moved to settings.toml.broken-"), "{moved:?}");
}

#[test]
fn a_panicked_writer_is_reported_and_the_drop_still_writes_the_pending_changes() {
    let rig = Rig::new();
    let store = paused_store(&rig, || panic!("the writer fails once"));
    set::<Scale>(&store, &111);
    let start = Instant::now();
    while !store.findings().iter().any(|f| f.error.to_string().contains("writer thread stopped")) {
        assert!(start.elapsed() < crate::MAX_DELAY * 3, "the dead writer was never reported");
        thread::sleep(Duration::from_millis(10));
    }
    assert!(!rig.user_file().exists());
    drop(store);
    assert!(rig.user_text().contains("scale = 111"));
}

#[test]
fn a_quarantine_followed_by_a_failed_write_is_reported_as_such() {
    let rig = Rig::new();
    let user_file = rig.user_file();
    let mut backend = FileBackend::new(rig.paths.clone());
    // The moment the broken file is out of the way, a folder takes its name, so the replacement cannot be written.
    backend.after_move = Some(Box::new({
        let user_file = user_file.clone();
        move || fs::create_dir(&user_file).unwrap()
    }));
    let store = Store::with_backend(Box::new(backend), registrations());
    rig.write_user("not = [toml");
    set::<Scale>(&store, &120);
    assert!(store.flush().is_err());
    let findings = store.findings();
    let moved = findings.iter().find(|f| f.from == "settings.toml").expect("the quarantine is reported");
    let text = moved.error.to_string();
    assert!(text.contains("moved to settings.toml.broken-") && text.contains("still pending"), "{text}");
    assert!(!text.contains("a new file holds"), "{text}");
    fs::remove_dir(&user_file).unwrap();
    store.flush().unwrap();
    assert!(rig.user_text().contains("scale = 120"), "the pending change landed once the folder was gone");
}

#[test]
fn a_file_repaired_by_another_process_before_the_quarantine_is_read_again_not_defaulted() {
    let rig = Rig::new();
    rig.write_user("[ui\nscale = ");
    let mut backend = FileBackend::new(rig.paths.clone());
    let user_file = rig.user_file();
    backend.before_quarantine = Some(Box::new(move || fs::write(&user_file, "[ui]\nscale = 130\n").unwrap()));
    let store = Store::with_backend(Box::new(backend), registrations());
    assert_eq!(get::<Scale>(&store), 130, "the repaired text is what loads");
    assert_eq!(store.findings(), Vec::new());
    assert!(!Rig::names_in(rig.paths.config()).iter().any(|n| n.contains(".broken-")));
    assert_eq!(rig.user_text(), "[ui]\nscale = 130\n");
}

#[test]
fn a_file_replaced_by_another_broken_text_is_moved_and_one_that_vanished_is_not_reported() {
    let rig = Rig::new();
    rig.write_user("[ui\nscale = ");
    let mut backend = FileBackend::new(rig.paths.clone());
    let user_file = rig.user_file();
    let first = AtomicBool::new(true);
    backend.before_quarantine = Some(Box::new(move || {
        if first.swap(false, Ordering::SeqCst) {
            fs::write(&user_file, "[other\n").unwrap();
        }
    }));
    let store = Store::with_backend(Box::new(backend), registrations());
    assert_eq!(get::<Scale>(&store), 100);
    let names = Rig::names_in(rig.paths.config());
    let broken: Vec<_> = names.iter().filter(|n| n.contains(".broken-")).collect();
    assert_eq!(broken.len(), 1, "{names:?}");
    assert_eq!(fs::read_to_string(rig.paths.config().join(broken[0])).unwrap(), "[other\n", "the file as found");
    assert_eq!(store.findings().len(), 1);

    let rig = Rig::new();
    rig.write_user("[ui\nscale = ");
    let mut backend = FileBackend::new(rig.paths.clone());
    let user_file = rig.user_file();
    backend.before_quarantine = Some(Box::new(move || fs::remove_file(&user_file).unwrap()));
    let store = Store::with_backend(Box::new(backend), registrations());
    assert_eq!((get::<Scale>(&store), store.findings()), (100, Vec::new()));
}
