//! The program feed: frames are read back only while an output runs and only when a slot of the program's grid is due.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::convert::Infallible;
use std::sync::Arc;

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream::feed::{Offer, ProgramFeed};
use omuzaki_stream::record::runtime::RecordPlan;
use omuzaki_stream::record::settings::RecordFormat;
use omuzaki_stream::service::{Program, PushOutcome, StreamService};

const SIZE: UVec2 = UVec2::new(16, 9);

fn picture() -> Image {
    Image { size: SIZE, alpha: AlphaEncoding::Premultiplied, rgba8: vec![0; (SIZE.x * SIZE.y * 4) as usize] }
}

#[test]
fn the_feed_reads_only_while_an_output_runs_and_a_slot_is_due() {
    let dir = tempfile::tempdir().unwrap();
    let service = Arc::new(StreamService::new().unwrap());
    // One frame a second: two offers in a row always fall in one slot, however slow the machine.
    service.set_program(Program { size: SIZE, fps: (1, 1) });
    let mut feed = ProgramFeed::new(Arc::clone(&service));
    let reads = std::cell::Cell::new(0);
    let read = || {
        reads.set(reads.get() + 1);
        Ok::<_, Infallible>(picture())
    };
    assert_eq!(feed.offer(read), Ok(Offer::Idle));
    assert!(!feed.wanted());

    let plan = RecordPlan {
        dir: dir.path().to_owned(),
        stem: "take".into(),
        format: RecordFormat::PngSequence,
        tracks: Vec::new(),
        hold: None,
        guard: None,
    };
    service.record_start(plan).unwrap();
    assert!(feed.wanted());
    assert_eq!(feed.offer(read), Ok(Offer::Pushed(PushOutcome::Queued)));
    assert_eq!(feed.offer(read), Ok(Offer::NotDue));
    assert_eq!(reads.get(), 1, "the pixels were read once");

    service.record_stop().unwrap();
    assert_eq!(feed.offer(read), Ok(Offer::Idle));
    assert_eq!(reads.get(), 1);
}

#[test]
fn a_failed_read_pushes_nothing_and_leaves_the_slot_due() {
    let dir = tempfile::tempdir().unwrap();
    let service = Arc::new(StreamService::new().unwrap());
    service.set_program(Program { size: SIZE, fps: (1, 1) });
    let mut feed = ProgramFeed::new(Arc::clone(&service));
    let plan = RecordPlan {
        dir: dir.path().to_owned(),
        stem: "take".into(),
        format: RecordFormat::PngSequence,
        tracks: Vec::new(),
        hold: None,
        guard: None,
    };
    service.record_start(plan).unwrap();
    assert_eq!(feed.offer(|| Err::<Image, _>("lost device")), Err("lost device"));
    assert_eq!(service.counters().pushed, 0);
    assert_eq!(feed.offer(|| Ok::<_, &str>(picture())), Ok(Offer::Pushed(PushOutcome::Queued)));
    service.record_stop().unwrap();
}
