//! The decision layer's measure on the rule floor (#2348): every moment of the shipped labelled set, through the gates
//! the show uses. The floor is the default provider and the answer whenever a model misses, so its agreement is the
//! least the character ever does; a model provider is measured the same way with
//! `cargo run -p omuzaki-cohost --example decide_eval`.

use std::time::Duration;

use omuzaki_performer::decide::RuleFloor;
use omuzaki_performer::eval::{HOLDOUT, MOMENTS, parse, run};

#[test]
fn the_rule_floor_agrees_with_most_labelled_moments_and_never_fires_a_wrong_gesture() {
    let moments = parse(MOMENTS).unwrap_or_else(|e| panic!("{e}"));
    let report = run(&mut RuleFloor, &moments, Duration::from_millis(400));
    println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    assert_eq!(report.provider, "rules");
    assert_eq!(report.answered, report.moments);
    assert_eq!((report.timeouts, report.errors), (0, 0));
    // A wrong gesture is the costly mistake: the floor never makes one.
    assert_eq!(report.false_gestures, 0);
    // Floors under today's numbers, so a change that makes the floor worse fails here.
    assert!(report.face.rate() >= 0.8, "face agreement {:.2}", report.face.rate());
    assert!(report.gesture.rate() >= 0.95, "gesture agreement {:.2}", report.gesture.rate());
    // A ceiling that holds on any machine: the floor answers in microseconds.
    assert!(report.p90_ms.is_some_and(|p| p < 25.0), "p90 {:?}", report.p90_ms);
}

#[test]
fn the_rule_floor_on_moments_it_was_never_tuned_on() {
    let moments = parse(HOLDOUT).unwrap_or_else(|e| panic!("{e}"));
    let report = run(&mut RuleFloor, &moments, Duration::from_millis(400));
    println!("{}", serde_json::to_string_pretty(&report).unwrap_or_default());
    assert_eq!(report.false_gestures, 0);
    // The honest numbers: far under the tuned set's, which is why a model provider is worth its wait.
    assert!(report.face.rate() >= 0.4, "face agreement {:.2}", report.face.rate());
    assert!(report.gesture.rate() >= 0.9, "gesture agreement {:.2}", report.gesture.rate());
}
