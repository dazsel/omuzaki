//! `cond.audio_level` to its acceptance (#1275), through the node harness: meter readings come from scripted
//! `audio.meters` replies in that op's shape.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use std::cell::RefCell;
use std::rc::Rc;

use omuzaki_core::time::Seconds;
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::ParamErrors;
use omuzaki_live_api::value::Val;
use omuzaki_live_mock::harness::NodeHarness;
use omuzaki_ops_api::OpError;
use serde_json::{Value, json};

fn node(params: Value) -> Result<NodeHarness, ParamErrors> {
    omuzaki_live_nodes::link();
    NodeHarness::by_id("cond.audio_level", params)
}

fn ports(h: &NodeHarness) -> Vec<String> {
    h.fired().iter().map(|f| f.port.to_string()).collect()
}

/// A node whose `audio.meters` replies read the shared level, in dB.
fn metered(params: Value) -> (NodeHarness, Rc<RefCell<f64>>) {
    let level = Rc::new(RefCell::new(f64::NEG_INFINITY));
    let mut h = node(params).unwrap();
    let read = Rc::clone(&level);
    h.respond("audio.meters", Seconds(0.0), move |args| {
        let db = *read.borrow();
        let db = if db.is_finite() { json!(db) } else { Value::Null };
        Ok(Some(json!({ "meters": [{ "bus": args["bus"], "peak_db": db, "rms_db": db }] })))
    });
    (h, level)
}

#[test]
fn a_loud_stretch_rises_once_dithering_adds_nothing_and_a_drop_falls_once() {
    let (mut h, level) = metered(json!({ "source": { "bus": "mic" }, "above_db": -30, "for_ms": 300 }));
    h.advance(Seconds(1.0));
    assert_eq!(h.fired().len(), 0, "silence");

    *level.borrow_mut() = -20.0;
    h.advance(Seconds(0.4));
    assert_eq!(ports(&h), ["rose"]);
    assert_eq!(h.read("value").unwrap(), Val::Bool(true));

    // ±2 dB around the threshold: the hysteresis (3 dB) holds it loud.
    for i in 0..20 {
        *level.borrow_mut() = if i % 2 == 0 { -32.0 } else { -28.0 };
        h.advance(Seconds(0.1));
    }
    assert_eq!(ports(&h), ["rose"]);

    *level.borrow_mut() = -35.0;
    h.advance(Seconds(0.5));
    assert_eq!(ports(&h), ["rose", "fell"]);
    assert_eq!(h.read("value").unwrap(), Val::Bool(false));

    // Dithering below the threshold while quiet never stays up long enough to rise.
    for i in 0..20 {
        *level.borrow_mut() = if i % 2 == 0 { -32.0 } else { -28.0 };
        h.advance(Seconds(0.1));
    }
    assert_eq!(ports(&h), ["rose", "fell"]);
}

#[test]
fn a_short_burst_doesnt_rise() {
    let (mut h, level) = metered(json!({ "above_db": -30, "for_ms": 300 }));
    *level.borrow_mut() = -10.0;
    h.advance(Seconds(0.25));
    *level.borrow_mut() = -50.0;
    h.advance(Seconds(1.0));
    assert_eq!(h.fired().len(), 0);
}

#[test]
fn polls_run_only_while_the_show_does_600_a_minute_at_100_ms() {
    let (mut h, _) = metered(json!({ "source": { "input": "mic" }, "poll_ms": 100 }));
    assert_eq!(h.calls().len(), 0, "nothing before the start");
    h.advance(Seconds(60.0));
    assert_eq!(h.calls().len(), 600);
    assert_eq!(h.calls()[0].args, json!({ "input": "mic" }));
    // Stop cancels the next poll; the harness refuses to run time past a stop, so the cancel is the check.
    h.stop();
    assert_eq!(h.calls().len(), 600);
}

#[test]
fn a_failed_poll_keeps_the_state_and_is_counted() {
    let mut h = node(json!({ "above_db": -30, "for_ms": 0 })).unwrap();
    let calls = Rc::new(RefCell::new(0_u32));
    let count = Rc::clone(&calls);
    h.respond("audio.meters", Seconds(0.0), move |_| {
        *count.borrow_mut() += 1;
        if *count.borrow() == 1 {
            Ok(Some(json!({ "meters": [{ "bus": "mic", "peak_db": -10.0, "rms_db": -10.0 }] })))
        } else {
            Err(OpError::not_found("no bus mic"))
        }
    });
    h.advance(Seconds(0.5));
    assert_eq!(ports(&h), ["rose"]);
    assert_eq!(h.read("value").unwrap(), Val::Bool(true), "still loud");
    assert_eq!(h.read("failed_polls").unwrap(), Val::Number(4.0));
}

#[test]
fn it_needs_only_the_meters_query_and_checks_its_numbers() {
    let h = node(json!({})).unwrap();
    assert_eq!(h.needs(), [Need::op("audio.meters")]);
    let Err(errors) = node(json!({ "poll_ms": 10, "hysteresis_db": -1, "source": { "bus": "" } })) else {
        panic!("took bad parameters")
    };
    let mut paths: Vec<_> = errors.0.iter().map(|e| e.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, ["hysteresis_db", "poll_ms", "source"]);
}
