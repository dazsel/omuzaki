//! The job adapter: an io op's progress reaches the job's signal, and its cancellation follows the job's flag.

// Test helpers may panic: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use omuzaki_doc_mock::MemDoc;
use omuzaki_io_api::{IoError, Progress};
use omuzaki_io_ops::ops::progress::JobProgress;
use omuzaki_ops_api::{Actor, Cancel, CancelFlag, JobCtx, JobDoc, JobId, JobSignal, OpError};

/// A job signal that records what it is told and shares its cancellation flag.
#[derive(Default)]
struct Signal {
    cancelled: CancelFlag,
    progress: Mutex<Vec<(String, f32)>>,
}

impl JobSignal for Signal {
    fn progress(&self, stage: &str, fraction: f32) {
        self.progress.lock().unwrap().push((stage.to_owned(), fraction));
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.is_cancelled()
    }

    fn cancel_flag(&self) -> CancelFlag {
        self.cancelled.clone()
    }

    fn spend(&self, _usd_micros: u64) -> Result<(), OpError> {
        Ok(())
    }
}

/// A job document over an empty in-memory store.
struct Doc(MemDoc);

impl JobDoc for Doc {
    fn read(&self, f: &mut dyn FnMut(&dyn omuzaki_doc_api::DocRead)) {
        f(&self.0);
    }
}

#[test]
fn stage_reaches_the_job_signal_with_the_same_stage_and_fraction() {
    let actor = Actor::Person { name: "alice".into() };
    let doc = Doc(MemDoc::new());
    let signal = Signal::default();
    let ctx = JobCtx::new(JobId(7), &actor, &doc, &signal);
    JobProgress(&ctx).stage("bake", 0.25);
    assert_eq!(*signal.progress.lock().unwrap(), vec![("bake".to_owned(), 0.25)]);
}

#[test]
fn is_cancelled_follows_the_job_signal() {
    let actor = Actor::Person { name: "alice".into() };
    let doc = Doc(MemDoc::new());
    let signal = Signal::default();
    let ctx = JobCtx::new(JobId(7), &actor, &doc, &signal);
    let progress = JobProgress(&ctx);
    assert!(!progress.is_cancelled());
    signal.cancelled.cancel();
    assert!(progress.is_cancelled());
}

#[test]
fn checkpoint_reports_then_fails_with_cancelled_once_the_job_is_cancelled() {
    let actor = Actor::Person { name: "alice".into() };
    let doc = Doc(MemDoc::new());
    let signal = Signal::default();
    let ctx = JobCtx::new(JobId(7), &actor, &doc, &signal);
    let progress = JobProgress(&ctx);
    progress.checkpoint("read", 0.5).unwrap();
    signal.cancelled.cancel();
    let err = progress.checkpoint("write", 0.75).unwrap_err();
    assert!(matches!(err, IoError::Cancelled), "{err:?}");
    assert_eq!(
        *signal.progress.lock().unwrap(),
        vec![("read".to_owned(), 0.5), ("write".to_owned(), 0.75)],
        "the cancelled checkpoint still reports its stage"
    );
}
