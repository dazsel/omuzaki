//! The mocks keep the promises the acceptance checks of #1099 rely on.

// Test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::io::Read;
use std::path::Path;

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

fn disk_with_dir() -> FakeDisk {
    let disk = FakeDisk::new();
    disk.create_dir_all(Path::new("/r/sub")).unwrap();
    disk
}

#[test]
fn fail_after_fails_that_operation_and_every_later_call() {
    let disk = disk_with_dir();
    disk.fail_after(3, Op::Any);
    disk.write(Path::new("/r/a"), b"1").unwrap();
    disk.fsync(Path::new("/r/a")).unwrap();
    assert!(disk.rename(Path::new("/r/a"), Path::new("/r/b")).is_err());
    assert!(disk.write(Path::new("/r/c"), b"2").is_err());
    assert!(disk.read(Path::new("/r/a")).is_err());
    assert!(!disk.exists(Path::new("/r")));
    // The failed rename did not happen.
    disk.revive();
    assert_eq!(disk.read(Path::new("/r/a")).unwrap(), b"1");
}

#[test]
fn fail_after_counts_only_the_named_operation() {
    let disk = disk_with_dir();
    disk.fail_after(2, Op::Rename);
    disk.write(Path::new("/r/a"), b"1").unwrap();
    disk.rename(Path::new("/r/a"), Path::new("/r/b")).unwrap();
    disk.write(Path::new("/r/c"), b"2").unwrap();
    assert!(disk.rename(Path::new("/r/c"), Path::new("/r/d")).is_err());
    assert_eq!((disk.count(Op::Write), disk.count(Op::Rename), disk.count(Op::Fsync)), (2, 2, 0));
}

#[test]
fn revive_keeps_synced_files_and_empties_the_rest() {
    let disk = disk_with_dir();
    let state = Path::new("/r/state.toml");
    disk.write_atomic(state, b"new").unwrap();
    disk.write(Path::new("/r/loose"), b"unsynced").unwrap();
    disk.write(Path::new("/r/half"), b"x").unwrap();
    disk.fsync(Path::new("/r/half")).unwrap();
    disk.write(Path::new("/r/half"), b"xy").unwrap();
    disk.revive();
    assert_eq!(disk.read(state).unwrap(), b"new");
    assert!(disk.read(Path::new("/r/loose")).unwrap().is_empty());
    // A second write after the sync is unsynced again.
    assert!(disk.read(Path::new("/r/half")).unwrap().is_empty());
}

#[test]
fn a_kill_between_write_and_rename_leaves_the_old_file() {
    let disk = disk_with_dir();
    let state = Path::new("/r/state.toml");
    disk.write_atomic(state, b"old").unwrap();
    for n in 1..=4 {
        let probe = FakeDisk::new();
        probe.create_dir_all(Path::new("/r")).unwrap();
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
    disk.write(Path::new("/r/sub/f"), b"x").unwrap();
    assert!(disk.write(Path::new("/nope/f"), b"x").is_err());
    assert!(disk.create_dir_all(Path::new("/r/sub/f/deeper")).is_err());

    disk.symlink("sub/f", Path::new("/r/link"), LinkKind::File).unwrap();
    assert!(disk.symlink("x", Path::new("/r/link"), LinkKind::File).is_err());
    disk.set_executable(Path::new("/r/sub/f"), true).unwrap();
    assert!(disk.set_executable(Path::new("/r/none"), true).is_err());

    disk.rename(Path::new("/r/sub"), Path::new("/r/moved")).unwrap();
    assert!(!disk.exists(Path::new("/r/sub")));
    let snap = disk.snapshot();
    assert_eq!(snap[Path::new("/r/moved/f")], Entry::File { bytes: b"x".to_vec(), executable: true });
    assert_eq!(snap[Path::new("/r/link")], Entry::Link { target: "sub/f".into() });

    let names: Vec<_> =
        disk.list_dir(Path::new("/r")).unwrap().into_iter().map(|e| (e.name, e.is_dir, e.is_symlink)).collect();
    assert_eq!(names, [("link".to_owned(), false, true), ("moved".to_owned(), true, false)]);

    disk.set_free_bytes(5);
    assert_eq!(disk.free_bytes(Path::new("/r")).unwrap(), 5);
    disk.remove_dir_all(Path::new("/r/moved")).unwrap();
    assert!(!disk.exists(Path::new("/r/moved/f")));
    assert!(disk.remove_dir_all(Path::new("/r/moved")).is_err());
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
