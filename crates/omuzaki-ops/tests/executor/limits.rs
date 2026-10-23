//! Per-session rate limits (#244, design `agent-sessions.md` §8.2): token buckets on the runner's clock, `Limited`
//! with the exact time to wait, never on persons; and the measurement that checks the defaults against a realistic
//! agent session.

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{Actor, ErrorCode, ErrorDetail, Grants, Limits, Mode, Request, Runner, SessionSpec, Sessions};
use serde_json::json;

use super::{Rig, agent, agent_request, request, rig, session_spec};

fn edit(actor: &Actor, value: u64) -> Request {
    Request { actor: actor.clone(), ..agent_request("test.set", json!({"path": "a", "value": value})) }
}

fn retry_after(error: &omuzaki_ops_api::OpError) -> (f32, String) {
    assert_eq!(error.code, ErrorCode::Limited, "{error:?}");
    match error.detail.as_deref() {
        Some(ErrorDetail::RetryAfter { seconds, limit }) => (*seconds, limit.clone()),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_burst_passes_the_next_waits_exactly_and_runs_count_apart() {
    let rig = rig(json!({"a": 0}));
    for value in 0..100 {
        rig.exec.run(edit(&agent(), value)).unwrap();
    }
    let refused = rig.exec.run(edit(&agent(), 100)).unwrap_err();
    let (seconds, limit) = retry_after(&refused);
    assert_eq!(limit, "edits_per_minute = 600");
    assert!(refused.message.contains("edits_per_minute = 600"), "{refused}");
    assert!((f64::from(seconds) - 0.1).abs() < 1e-6, "one token at 10 a second: {seconds}");
    assert_eq!(rig.at("a"), Some(json!(99)), "nothing ran");
    // Queries and dry-runs draw from their own bucket.
    rig.exec.run(Request { mode: Mode::DryRun, ..edit(&agent(), 7) }).unwrap();
    rig.exec.run(Request { actor: agent(), ..agent_request("test.get", json!({"path": "a"})) }).unwrap();
    // Transaction calls draw from the edits: the bucket is empty, so the begin hands back an id that is never open
    // and the commit is limited too.
    let tx = rig.exec.begin("t", &agent());
    assert_eq!(rig.exec.commit(tx, &agent()).unwrap_err().code, ErrorCode::Limited);
    assert!(rig.exec.session_of(&agent()).unwrap().open_tx.is_none());
    // The begin can't say no, so the reason is kept for the surface to report: the same wait.
    let refusal = rig.exec.begin_refusal(tx).expect("the refused begin's reason is kept");
    assert_eq!(retry_after(&refusal).1, "edits_per_minute = 600");

    rig.clock.advance(Seconds(f64::from(seconds)));
    rig.exec.run(edit(&agent(), 101)).unwrap();
    assert_eq!(rig.exec.run(edit(&agent(), 102)).unwrap_err().code, ErrorCode::Limited);
}

#[test]
fn persons_are_never_limited() {
    let rig = rig(json!({"a": 0}));
    for value in 0..10_000 {
        rig.exec.run(request("test.set", json!({"path": "a", "value": value}))).unwrap();
    }
    assert_eq!(rig.at("a"), Some(json!(9_999)));
}

#[test]
fn a_limit_of_zero_is_refused_naming_the_field() {
    let rig = rig(json!({}));
    let spec = SessionSpec { limits: Limits { edits_per_minute: 0, ..Limits::default() }, ..session_spec("c") };
    let e = rig.exec.open(spec).unwrap_err();
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("limits.edits_per_minute")));
}

/// A deterministic stream of think times between 2 and 20 seconds (xorshift, fixed seed).
struct Think(u64);

impl Think {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        #[allow(clippy::cast_precision_loss)]
        let unit = (self.0 % 10_000) as f64 / 10_000.0;
        2.0 + unit * 18.0
    }
}

/// What a scripted character build sends, with when: 40 transactions of 5 queries, 3 dry-runs and 8 edits, a think
/// time before each call, and one 50-edit burst (weight painting) with no think time in it.
fn scripted_build() -> Vec<(f64, bool)> {
    // Each transaction: begin, 5 queries, 3 dry-runs, 8 edits, commit (true is an edit).
    let shape = std::iter::once(true)
        .chain(std::iter::repeat_n(false, 8))
        .chain(std::iter::repeat_n(true, 8))
        .chain(std::iter::once(true));
    let shape: Vec<bool> = shape.collect();
    let mut think = Think(0x2106);
    let (mut at, mut calls) = (0.0, Vec::new());
    for tx in 0..40 {
        for (i, edit) in shape.iter().enumerate() {
            at += think.next();
            calls.push((at, *edit));
            if tx == 20 && i == shape.len() - 2 {
                // The weight-painting burst: 50 edits at one instant, after a think, before the commit.
                at += think.next();
                calls.extend(std::iter::repeat_n((at, true), 50));
            }
        }
    }
    calls
}

/// The most calls of each kind in any 60-second window, and the largest number at one instant.
fn peaks(calls: &[(f64, bool)]) -> (usize, usize, usize) {
    let in_window =
        |edit: bool, from: f64| calls.iter().filter(|(at, e)| *e == edit && *at >= from && *at < from + 60.0).count();
    let edits = calls.iter().map(|(at, _)| in_window(true, *at)).max().unwrap_or(0);
    let runs = calls.iter().map(|(at, _)| in_window(false, *at)).max().unwrap_or(0);
    // Calls in one instant share the very same timestamp, so the bit pattern says which instant a call is in.
    let burst = calls.iter().map(|(at, _)| calls.iter().filter(|(t, _)| t.to_bits() == at.to_bits()).count()).max();
    let burst = burst.unwrap_or(0);
    (edits, runs, burst)
}

/// The measurement #244 asks for: the defaults stay if every peak is under half its limit. The per-minute peaks are far
/// under; the 50-edit burst is, by the scenario's construction, at most half the burst of 100 (all 50 in one instant,
/// where a real client's round trips let the bucket refill at 10 a second). The numbers are printed for the PR
/// (`cargo test -p omuzaki-ops --test executor limits::the_defaults -- --nocapture`).
#[test]
fn the_defaults_hold_a_scripted_character_build_with_room_to_spare() {
    let calls = scripted_build();
    let (edits, runs, burst) = peaks(&calls);
    let limits = Limits::default();
    println!(
        "scripted build: {} calls over {:.0} s; peak edits/min {edits} (limit {}), peak runs/min {runs} (limit {}), \
         largest burst {burst} (edit burst {})",
        calls.len(),
        calls.last().map_or(0.0, |c| c.0),
        limits.edits_per_minute,
        limits.runs_per_minute,
        limits.edit_burst,
    );
    let half = |peak: usize| u32::try_from(peak * 2).unwrap();
    assert!(half(edits) < limits.edits_per_minute && half(runs) < limits.runs_per_minute);
    assert!(half(burst) <= limits.edit_burst);

    // And through the executor, on its clock: nothing is limited.
    let rig = rig(json!({"a": 0}));
    let agent = open_builder(&rig);
    let mut now = 0.0;
    for (i, (at, edit)) in calls.iter().enumerate() {
        rig.clock.advance(Seconds(at - now));
        now = *at;
        let mode = if *edit { Mode::Apply } else { Mode::DryRun };
        let req = Request { actor: agent.clone(), mode, ..agent_request("test.set", json!({"path": "a", "value": i})) };
        rig.exec.run(req).unwrap();
    }
}

fn open_builder(rig: &Rig) -> Actor {
    rig.exec.open(SessionSpec { grants: Grants::agent_default(), ..session_spec("builder") }).unwrap().actor
}
