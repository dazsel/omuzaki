//! The mocks keep the promises the acceptance checks of #1099 rely on.

// Test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::io::{ErrorKind, Read};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use omuzaki_ai_api::{FetchError, Fetcher};
use omuzaki_update_api::Version;
use omuzaki_update_api::clock::{Timestamp, WallClock};
use omuzaki_update_api::disk::{Disk, LinkKind};
use omuzaki_update_api::error::UpdateError;
use omuzaki_update_api::event::UpdateEvent;
use omuzaki_update_api::hold::HoldReason;
use omuzaki_update_api::progress::Progress;
use omuzaki_update_api::release::{Channel, PackageKind, Release};
use omuzaki_update_api::status::UpdateStatus;
use omuzaki_update_api::updater::Updater;
use omuzaki_update_mock::clock::FakeWallClock;
use omuzaki_update_mock::disk::{Entry, FakeDisk, Op};
use omuzaki_update_mock::server::{Call, FakeServer};
use omuzaki_update_mock::updater::MockUpdater;

fn release(minor: u64) -> Release {
    Release {
        version: Version::new(1, minor, 0),
        channel: Channel::Stable,
        sequence: 7,
        package: PackageKind::Full,
        download_bytes: 100,
        installed_bytes: 300,
        notes_url: None,
        recommended: false,
    }
}

struct Quiet;
impl Progress for Quiet {
    fn report(&self, _: u64, _: u64) {}
    fn cancelled(&self) -> bool {
        false
    }
}

fn body(server: &FakeServer, url: &str, from: u64) -> (u64, Vec<u8>) {
    let mut body = server.open(url, from).unwrap();
    let mut bytes = Vec::new();
    body.reader.read_to_end(&mut bytes).unwrap();
    (body.start, bytes)
}

#[test]
fn holds_are_listed_in_take_order_and_release_on_drop() {
    let updater = MockUpdater::new();
    let recording = updater.hold(HoldReason::Recording);
    let streaming = updater.hold(HoldReason::Streaming);
    assert_eq!(updater.holds(), [HoldReason::Recording, HoldReason::Streaming]);
    assert_eq!(updater.restart_allowed(), Err(UpdateError::Busy(vec![HoldReason::Recording, HoldReason::Streaming])));

    drop(recording);
    assert_eq!(updater.holds(), [HoldReason::Streaming]);
    drop(streaming);
    assert_eq!(updater.restart_allowed(), Ok(()));
}

#[test]
fn a_leaked_hold_stays_and_a_new_updater_starts_empty() {
    let updater = MockUpdater::new();
    std::mem::forget(updater.hold(HoldReason::Rendering));
    assert_eq!(updater.holds(), [HoldReason::Rendering]);
    assert_eq!(MockUpdater::new().holds(), []);
}

#[test]
fn two_holds_of_one_reason_release_independently() {
    let updater = MockUpdater::new();
    let first = updater.hold(HoldReason::Capturing);
    let second = updater.hold(HoldReason::Capturing);
    drop(first);
    assert_eq!(updater.holds(), [HoldReason::Capturing]);
    drop(second);
    assert_eq!(updater.holds(), []);
}

#[test]
fn scripted_answers_drive_status_and_events() {
    let updater = MockUpdater::new();
    let events = updater.subscribe();
    updater.script_check(Ok(Some(release(4))));
    updater.script_check(Err(UpdateError::Offline));
    let _hold = updater.hold(HoldReason::ProjectSaving);

    assert_eq!(updater.check(Channel::Beta), Ok(Some(release(4))));
    assert_eq!(updater.status(), UpdateStatus::Available { release: release(4) });
    assert_eq!(updater.check(Channel::Stable), Err(UpdateError::Offline));
    assert_eq!(updater.status(), UpdateStatus::Available { release: release(4) });
    assert_eq!(updater.checks(), [Channel::Beta, Channel::Stable]);

    updater.download(&release(4), &Quiet).unwrap();
    updater.stage(&release(4)).unwrap();
    let staged = UpdateStatus::Staged { version: Version::new(1, 4, 0), deferred_by: vec![HoldReason::ProjectSaving] };
    assert_eq!(updater.status(), staged);

    assert_eq!(events.next(), Some(UpdateEvent::Status(UpdateStatus::Available { release: release(4) })));
    assert_eq!(events.next(), Some(UpdateEvent::Progress { done: 100, total: 100 }));
    assert_eq!(events.next(), Some(UpdateEvent::Status(staged)));
    assert_eq!(events.next(), None);
}

#[test]
fn defaults_and_failures() {
    let updater = MockUpdater::new();
    assert_eq!(updater.check(Channel::Stable), Ok(None));
    assert_eq!(updater.rollback(), Err(UpdateError::NothingToRollBack));
    updater.script_stage(Err(UpdateError::DiskFull { need: 9 }));
    assert_eq!(updater.stage(&release(2)), Err(UpdateError::DiskFull { need: 9 }));
    assert_eq!(updater.status(), UpdateStatus::Idle { last_checked: None });
    updater.healthy();
    updater.healthy();
    assert_eq!(updater.healthy_calls(), 2);
}

#[test]
fn server_records_every_open_and_honours_ranges() {
    let server = FakeServer::new();
    server.put("https://feed/a", b"0123456789".to_vec());
    assert_eq!(body(&server, "https://feed/a", 4), (4, b"456789".to_vec()));

    server.ignore_range(true);
    assert_eq!(body(&server, "https://feed/a", 4), (0, b"0123456789".to_vec()));
    assert_eq!(
        server.calls(),
        [Call { url: "https://feed/a".into(), from: 4 }, Call { url: "https://feed/a".into(), from: 4 }]
    );
}

#[test]
fn server_faults() {
    let server = FakeServer::new();
    server.put("u", vec![1, 2, 3, 4]);
    assert!(server.flip_byte("u", 2));
    assert_eq!(body(&server, "u", 0).1, [1, 2, !3, 4]);
    assert!(!server.flip_byte("u", 4));
    assert!(server.truncate_at("u", 2));
    assert_eq!(body(&server, "u", 0).1, [1, 2]);
    assert!(!server.truncate_at("missing", 1));

    assert!(matches!(server.open("missing", 0), Err(FetchError::Network { .. })));
    server.offline(true);
    assert!(matches!(server.open("u", 0), Err(FetchError::Network { reason, .. }) if reason == "offline"));
    assert_eq!(server.calls().len(), 4);
}

/// An absolute folder on this platform: `/r` on unix, `C:\\r` on Windows, where a path without a drive is not absolute.
fn root() -> PathBuf {
    if cfg!(windows) { PathBuf::from(r"C:\r") } else { PathBuf::from("/r") }
}

fn p(rest: &str) -> PathBuf {
    root().join(rest)
}

/// The root folder of the platform's file system.
fn fs_root() -> PathBuf {
    root().ancestors().last().unwrap().to_path_buf()
}

fn disk_with_dir() -> FakeDisk {
    let disk = FakeDisk::new();
    disk.create_dir_all(&p("sub")).unwrap();
    disk
}

#[test]
fn fail_after_fails_that_operation_and_every_later_call() {
    let disk = disk_with_dir();
    disk.fail_after(3, Op::Any);
    disk.write(&p("a"), b"1").unwrap();
    disk.fsync(&p("a")).unwrap();
    assert!(disk.rename(&p("a"), &p("b")).is_err());
    assert!(disk.write(&p("c"), b"2").is_err());
    assert!(disk.read(&p("a")).is_err());
    assert!(!disk.exists(&root()));
    // The failed rename did not happen.
    disk.revive();
    assert_eq!(disk.read(&p("a")).unwrap(), b"1");
}

#[test]
fn fail_after_counts_only_the_named_operation() {
    let disk = disk_with_dir();
    disk.fail_after(2, Op::Rename);
    disk.write(&p("a"), b"1").unwrap();
    disk.rename(&p("a"), &p("b")).unwrap();
    disk.write(&p("c"), b"2").unwrap();
    assert!(disk.rename(&p("c"), &p("d")).is_err());
    assert_eq!((disk.count(Op::Write), disk.count(Op::Rename), disk.count(Op::Fsync)), (2, 2, 0));
}

#[test]
fn revive_keeps_synced_files_and_empties_the_rest() {
    let disk = disk_with_dir();
    let state = &p("state.toml");
    disk.write_atomic(state, b"new").unwrap();
    disk.write(&p("loose"), b"unsynced").unwrap();
    disk.write(&p("half"), b"x").unwrap();
    disk.fsync(&p("half")).unwrap();
    disk.write(&p("half"), b"xy").unwrap();
    disk.revive();
    assert_eq!(disk.read(state).unwrap(), b"new");
    assert_eq!(disk.read(&p("loose")).unwrap(), b"");
    // A second write after the sync is unsynced again.
    assert_eq!(disk.read(&p("half")).unwrap(), b"");
}

#[test]
fn a_kill_between_write_and_rename_leaves_the_old_file() {
    let disk = disk_with_dir();
    let state = &p("state.toml");
    disk.write_atomic(state, b"old").unwrap();
    for n in 1..=4 {
        let probe = FakeDisk::new();
        probe.create_dir_all(&root()).unwrap();
        probe.write_atomic(state, b"old").unwrap();
        probe.fail_after(n, Op::Any);
        probe.write_atomic(state, b"new").unwrap_err();
        probe.revive();
        let now = probe.read(state).unwrap();
        // Whole or not at all: never empty, never half.
        assert!(now == b"old" || now == b"new", "step {n}: {now:?}");
    }
}

#[test]
fn tree_operations() {
    let disk = disk_with_dir();
    disk.write(&p("sub/f"), b"x").unwrap();
    assert!(disk.write(&fs_root().join("nope/f"), b"x").is_err());
    assert!(disk.create_dir_all(&p("sub/f/deeper")).is_err());

    disk.symlink("sub/f", &p("link"), LinkKind::File).unwrap();
    assert!(disk.symlink("x", &p("link"), LinkKind::File).is_err());
    disk.set_executable(&p("sub/f"), true).unwrap();
    assert!(disk.set_executable(&p("none"), true).is_err());

    disk.rename(&p("sub"), &p("moved")).unwrap();
    assert!(!disk.exists(&p("sub")));
    let snap = disk.snapshot();
    assert_eq!(snap[&p("moved/f")], Entry::File { bytes: b"x".to_vec(), executable: true });
    assert_eq!(snap[&p("link")], Entry::Link { target: "sub/f".into() });

    let names: Vec<_> = disk.list_dir(&root()).unwrap().into_iter().map(|e| (e.name, e.is_dir, e.is_symlink)).collect();
    assert_eq!(names, [("link".to_owned(), false, true), ("moved".to_owned(), true, false)]);

    disk.set_free_bytes(5);
    assert_eq!(disk.free_bytes(&root()).unwrap(), 5);
    disk.remove_dir_all(&p("moved")).unwrap();
    assert!(!disk.exists(&p("moved/f")));
    assert!(disk.remove_dir_all(&p("moved")).is_err());
}

#[test]
fn wall_clock_moves_only_when_told() {
    let clock = FakeWallClock::new(Timestamp(100));
    assert_eq!(clock.now(), Timestamp(100));
    clock.advance(50);
    assert_eq!(clock.now(), Timestamp(150));
    clock.set(Timestamp(10));
    clock.advance(i64::MAX);
    assert_eq!(clock.now(), Timestamp(i64::MAX));
}

#[test]
fn a_progress_callback_may_call_back_into_the_updater() {
    struct Redraw(Arc<MockUpdater>);
    impl Progress for Redraw {
        fn report(&self, _: u64, _: u64) {
            // Re-entry: it deadlocks if `download` still holds the updater's lock while reporting.
            drop(self.0.status());
        }
        fn cancelled(&self) -> bool {
            false
        }
    }
    let updater = Arc::new(MockUpdater::new());
    let (done, finished) = mpsc::channel();
    let worker = {
        let updater = Arc::clone(&updater);
        std::thread::spawn(move || {
            let result = updater.download(&release(3), &Redraw(Arc::clone(&updater)));
            done.send(result).unwrap();
        })
    };
    let result = finished.recv_timeout(Duration::from_secs(10)).expect("download deadlocked in the progress callback");
    assert_eq!(result, Ok(()));
    worker.join().unwrap();
}

#[test]
fn the_root_folder_cannot_be_written_moved_removed_or_linked() {
    let disk = disk_with_dir();
    let top = fs_root();
    assert_eq!(disk.write(&top, b"x").unwrap_err().kind(), ErrorKind::IsADirectory);
    assert_eq!(disk.read(&top).unwrap_err().kind(), ErrorKind::IsADirectory);
    assert_eq!(disk.remove_dir_all(&top).unwrap_err().kind(), ErrorKind::ResourceBusy);
    assert_eq!(disk.rename(&top, &p("moved")).unwrap_err().kind(), ErrorKind::ResourceBusy);
    disk.write(&p("f"), b"x").unwrap();
    assert_eq!(disk.rename(&p("f"), &top).unwrap_err().kind(), ErrorKind::ResourceBusy);
    assert_eq!(disk.rename(&p("sub"), &top).unwrap_err().kind(), ErrorKind::ResourceBusy);
    assert_eq!(disk.symlink("x", &top, LinkKind::Dir).unwrap_err().kind(), ErrorKind::AlreadyExists);
    disk.set_executable(&top, true).unwrap();
    // The root is still a folder: it exists, lists its children, and nothing was added under its own name.
    assert!(disk.exists(&top));
    assert!(disk.list_dir(&top).unwrap().iter().any(|e| e.is_dir));
    assert!(!disk.snapshot().contains_key(&top));
}

#[test]
fn writing_to_a_folder_is_refused_and_leaves_it_alone() {
    let disk = disk_with_dir();
    disk.write(&p("sub/f"), b"x").unwrap();
    assert_eq!(disk.write(&p("sub"), b"y").unwrap_err().kind(), ErrorKind::IsADirectory);
    assert_eq!(disk.read(&p("sub")).unwrap_err().kind(), ErrorKind::IsADirectory);
    assert_eq!(disk.read(&p("sub/f")).unwrap(), b"x");
}

#[test]
fn renaming_a_path_onto_itself_changes_nothing() {
    let disk = disk_with_dir();
    disk.write(&p("sub/f"), b"x").unwrap();
    disk.write(&p("file"), b"y").unwrap();
    disk.symlink("file", &p("link"), LinkKind::File).unwrap();
    let before = disk.snapshot();
    for path in [p("sub"), p("file"), p("link")] {
        disk.rename(&path, &path).unwrap();
    }
    assert_eq!(disk.snapshot(), before);
    assert_eq!(disk.rename(&p("none"), &p("none")).unwrap_err().kind(), ErrorKind::NotFound);
}

#[test]
fn a_rejected_rename_moves_nothing() {
    let disk = disk_with_dir();
    disk.write(&p("sub/f"), b"x").unwrap();
    disk.create_dir_all(&p("full")).unwrap();
    disk.write(&p("full/g"), b"y").unwrap();
    let before = disk.snapshot();
    assert!(disk.rename(&p("sub"), &p("full")).is_err());
    assert!(disk.rename(&p("sub"), &p("sub/inner")).is_err());
    assert_eq!(disk.snapshot(), before);
}

/// A disk with a file, a link to it, an empty folder, a folder with a file in it, and a folder to move.
fn disk_with_kinds() -> FakeDisk {
    let disk = disk_with_dir();
    disk.write(&p("file"), b"x").unwrap();
    disk.write(&p("other"), b"y").unwrap();
    disk.symlink("file", &p("link"), LinkKind::File).unwrap();
    disk.create_dir_all(&p("empty")).unwrap();
    disk.create_dir_all(&p("full")).unwrap();
    disk.write(&p("full/g"), b"z").unwrap();
    disk.write(&p("sub/f"), b"w").unwrap();
    disk
}

fn kind_of(result: std::io::Result<()>) -> ErrorKind {
    result.unwrap_err().kind()
}

#[test]
fn rename_never_replaces_a_folder() {
    let disk = disk_with_kinds();
    let before = disk.snapshot();
    // A file onto a folder: `EISDIR` on unix.
    assert_eq!(kind_of(disk.rename(&p("file"), &p("full"))), ErrorKind::IsADirectory);
    assert_eq!(kind_of(disk.rename(&p("file"), &p("empty"))), ErrorKind::IsADirectory);
    // A folder onto a non-empty folder: `ENOTEMPTY`.
    assert_eq!(kind_of(disk.rename(&p("sub"), &p("full"))), ErrorKind::DirectoryNotEmpty);
    // A folder onto an empty folder: unix would replace it, Windows cannot, so the fake refuses.
    assert_eq!(kind_of(disk.rename(&p("sub"), &p("empty"))), ErrorKind::AlreadyExists);
    // A link onto a folder, and a folder onto a file or a link.
    assert_eq!(kind_of(disk.rename(&p("link"), &p("empty"))), ErrorKind::IsADirectory);
    assert_eq!(kind_of(disk.rename(&p("sub"), &p("file"))), ErrorKind::NotADirectory);
    assert_eq!(kind_of(disk.rename(&p("sub"), &p("link"))), ErrorKind::NotADirectory);
    assert_eq!(disk.snapshot(), before, "a refused rename changes nothing");
}

#[test]
fn rename_replaces_files_and_links_and_keeps_the_same_path_a_no_op() {
    let disk = disk_with_kinds();
    disk.rename(&p("other"), &p("file")).unwrap();
    assert_eq!(disk.read(&p("file")).unwrap(), b"y");
    disk.rename(&p("file"), &p("link")).unwrap();
    assert_eq!(disk.read(&p("link")).unwrap(), b"y");
    let before = disk.snapshot();
    disk.rename(&p("full"), &p("full")).unwrap();
    assert_eq!(disk.snapshot(), before);
}

#[test]
fn a_file_in_the_way_is_not_a_folder_for_every_mutator() {
    let disk = disk_with_kinds();
    let below = p("file/child");
    assert_eq!(kind_of(disk.write(&below, b"x")), ErrorKind::NotADirectory);
    assert_eq!(kind_of(disk.create_dir_all(&below)), ErrorKind::NotADirectory);
    assert_eq!(kind_of(disk.symlink("t", &below, LinkKind::File)), ErrorKind::NotADirectory);
    assert_eq!(kind_of(disk.rename(&p("other"), &below)), ErrorKind::NotADirectory);
    assert_eq!(kind_of(disk.rename(&below, &p("elsewhere"))), ErrorKind::NotADirectory);
    assert_eq!(disk.read(&below).unwrap_err().kind(), ErrorKind::NotADirectory);
    assert_eq!(kind_of(disk.remove_dir_all(&p("file"))), ErrorKind::NotADirectory);
    assert_eq!(disk.list_dir(&p("file")).unwrap_err().kind(), ErrorKind::NotADirectory);
    // A missing path with only folders above it is plain `NotFound`.
    assert_eq!(disk.read(&p("sub/none")).unwrap_err().kind(), ErrorKind::NotFound);
}

#[test]
fn creating_what_already_exists_as_another_kind_is_refused() {
    let disk = disk_with_kinds();
    let before = disk.snapshot();
    assert_eq!(kind_of(disk.create_dir_all(&p("file"))), ErrorKind::AlreadyExists);
    assert_eq!(kind_of(disk.create_dir_all(&p("link"))), ErrorKind::AlreadyExists);
    assert_eq!(kind_of(disk.symlink("t", &p("full"), LinkKind::Dir)), ErrorKind::AlreadyExists);
    assert_eq!(kind_of(disk.symlink("t", &p("file"), LinkKind::File)), ErrorKind::AlreadyExists);
    assert_eq!(kind_of(disk.write(&p("full"), b"x")), ErrorKind::IsADirectory);
    disk.create_dir_all(&p("full")).unwrap();
    assert_eq!(disk.snapshot(), before);
}
