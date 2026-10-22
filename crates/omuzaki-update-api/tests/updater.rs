//! The `Updater` contract as a caller sees it: `healthy` reports a failed marker write as an error, not as success.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use omuzaki_update_api::error::UpdateError;
use omuzaki_update_api::event::{UpdateEvents, event_queue};
use omuzaki_update_api::hold::{Hold, HoldReason};
use omuzaki_update_api::progress::Progress;
use omuzaki_update_api::release::{Channel, Release};
use omuzaki_update_api::status::UpdateStatus;
use omuzaki_update_api::updater::Updater;

/// A stand-in whose marker write fails the way a full disk does, or succeeds when `fail` is false.
struct Stub {
    fail: bool,
}

impl Updater for Stub {
    fn status(&self) -> UpdateStatus {
        UpdateStatus::Idle { last_checked: None }
    }

    fn check(&self, _channel: Channel) -> Result<Option<Release>, UpdateError> {
        Ok(None)
    }

    fn download(&self, _release: &Release, _progress: &dyn Progress) -> Result<(), UpdateError> {
        Ok(())
    }

    fn stage(&self, _release: &Release) -> Result<(), UpdateError> {
        Ok(())
    }

    fn rollback(&self) -> Result<(), UpdateError> {
        Err(UpdateError::NothingToRollBack)
    }

    fn hold(&self, _why: HoldReason) -> Hold {
        Hold::none()
    }

    fn healthy(&self) -> Result<(), UpdateError> {
        if self.fail { Err(UpdateError::Io("no space left on device".to_owned())) } else { Ok(()) }
    }

    fn subscribe(&self) -> UpdateEvents {
        event_queue().1
    }

    fn restart_allowed(&self) -> Result<(), UpdateError> {
        Ok(())
    }
}

#[test]
fn a_failed_marker_write_is_an_io_error_from_healthy() {
    let updater: &dyn Updater = &Stub { fail: true };
    match updater.healthy() {
        Err(UpdateError::Io(reason)) => assert!(reason.contains("no space")),
        other => panic!("healthy did not report the failure: {other:?}"),
    }
}

#[test]
fn a_written_marker_is_ok_from_healthy_and_idempotent() {
    let updater: &dyn Updater = &Stub { fail: false };
    assert_eq!(updater.healthy(), Ok(()));
    assert_eq!(updater.healthy(), Ok(()));
}
