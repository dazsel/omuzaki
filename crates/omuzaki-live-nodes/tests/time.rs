//! The time nodes to their acceptance (#733), through the node harness on a fake clock and a fake wall.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::float_cmp)]

use jiff::Timestamp;
use jiff::civil::DateTime;
use jiff::tz::TimeZone;
use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::UtcMillis;
use omuzaki_live_api::node::ParamErrors;
use omuzaki_live_api::value::Val;
use omuzaki_live_mock::harness::NodeHarness;
use serde_json::{Value, json};

const HOUR: f64 = 3_600.0;

fn node(kind: &str, params: Value) -> Result<NodeHarness, ParamErrors> {
    omuzaki_live_nodes::link();
    NodeHarness::by_id(kind, params)
}

/// The instant `local` (`YYYY-MM-DDTHH:MM`) in `tz`, the earlier one when it happens twice.
fn instant(local: &str, tz: &str) -> Timestamp {
    let local: DateTime = local.parse().unwrap();
    TimeZone::get(tz).unwrap().to_ambiguous_timestamp(local).earlier().unwrap()
}

/// `kind` with `params`, its wall starting at `local` in `tz` and its clock at 0.
fn at_wall(kind: &str, params: Value, local: &str, tz: &str) -> NodeHarness {
    let start = instant(local, tz).as_millisecond();
    node(kind, params).unwrap().starting_at(Seconds(0.0), UtcMillis(start))
}

/// The wall instants of every firing, given the wall at clock 0.
fn fired_at(h: &NodeHarness, start: Timestamp) -> Vec<Timestamp> {
    h.fired()
        .iter()
        .map(|f| {
            #[allow(clippy::cast_possible_truncation)]
            let ms = (f.at.0 * 1_000.0).round() as i64;
            Timestamp::from_millisecond(start.as_millisecond() + ms).unwrap()
        })
        .collect()
}

// ── timers ─────────────────────────────────────────────────────────────────────────────────────────────────────

#[test]
fn every_second_fires_3600_times_in_an_hour_each_on_the_second() {
    let mut h = node("timer.every", json!({ "seconds": 1 })).unwrap();
    h.advance(Seconds(HOUR));
    assert_eq!(h.fired().len(), 3_600);
    for (i, fired) in h.fired().iter().enumerate() {
        let n = f64::from(u32::try_from(i + 1).unwrap());
        assert_eq!(fired.at.0, n, "tick {n}");
        assert_eq!(
            fired.values.iter().find(|(p, _)| p.as_str() == "count").map(|(_, v)| v.clone()),
            Some(Val::Number(n))
        );
    }
}

#[test]
fn every_stops_and_starts_again_from_now() {
    let mut h = node("timer.every", json!({ "seconds": 2 })).unwrap();
    h.advance(Seconds(5.0));
    h.send("stop");
    h.advance(Seconds(10.0));
    assert_eq!(h.fired().len(), 2, "ticks at 2 and 4, then none");
    h.send("start");
    h.advance(Seconds(2.0));
    assert_eq!(h.fired().len(), 3);
    assert_eq!(h.fired()[2].at.0, 17.0);
}

#[test]
fn once_fires_once_and_in_restarts_it() {
    let mut h = node("timer.once", json!({ "seconds": 3 })).unwrap();
    h.advance(Seconds(10.0));
    assert_eq!(h.fired().iter().map(|f| f.at.0).collect::<Vec<_>>(), [3.0]);
    h.send("in");
    h.advance(Seconds(1.0));
    h.send("in");
    h.advance(Seconds(10.0));
    assert_eq!(h.fired().iter().map(|f| f.at.0).collect::<Vec<_>>(), [3.0, 14.0]);
}

#[test]
fn the_stopwatch_measures_between_start_and_stop() {
    let mut h = node("timer.stopwatch", json!({})).unwrap();
    h.advance_to(Seconds(1.0));
    h.send("start");
    h.advance_to(Seconds(2.0));
    assert_eq!(h.read("elapsed").unwrap(), Val::Number(1.0));
    h.advance_to(Seconds(3.5));
    h.send("stop");
    assert_eq!(h.fired().len(), 1);
    h.advance_to(Seconds(10.0));
    assert_eq!(h.read("elapsed").unwrap(), Val::Number(2.5), "stopped, it holds");
    h.send("reset");
    assert_eq!(h.read("elapsed").unwrap(), Val::Number(0.0));
}

// ── schedule.at and DST ────────────────────────────────────────────────────────────────────────────────────────

/// Runs a `time` schedule in `tz` for the 24 hours from local midnight of `day`, and returns its firings.
fn one_day(time: &str, tz: &str, day: &str) -> Vec<Timestamp> {
    let midnight = format!("{day}T00:00");
    let mut h = at_wall("schedule.at", json!({ "time": time, "tz": tz }), &midnight, tz);
    h.advance(Seconds(24.0 * HOUR));
    fired_at(&h, instant(&midnight, tz))
}

#[test]
fn a_repeated_hour_fires_once_at_the_first() {
    // 2026-10-25 London and 2026-11-01 New York go back an hour at 02:00 summer time.
    assert_eq!(one_day("01:30", "Europe/London", "2026-10-25"), ["2026-10-25T00:30:00Z".parse::<Timestamp>().unwrap()]);
    assert_eq!(
        one_day("01:30", "America/New_York", "2026-11-01"),
        ["2026-11-01T05:30:00Z".parse::<Timestamp>().unwrap()]
    );
}

#[test]
fn a_skipped_time_fires_once_at_the_end_of_the_gap() {
    // 2026-03-08 New York skips 02:00-03:00 (03:00 EDT is 07:00Z); 2026-03-29 London skips 01:00-02:00 (01:00Z).
    assert_eq!(
        one_day("02:30", "America/New_York", "2026-03-08"),
        ["2026-03-08T07:00:00Z".parse::<Timestamp>().unwrap()]
    );
    assert_eq!(one_day("01:30", "Europe/London", "2026-03-29"), ["2026-03-29T01:00:00Z".parse::<Timestamp>().unwrap()]);
}

#[test]
fn an_ordinary_day_fires_on_the_minute_and_only_on_chosen_days() {
    // 2026-10-21 is a Wednesday.
    assert_eq!(one_day("21:30", "Asia/Tokyo", "2026-10-21"), ["2026-10-21T12:30:00Z".parse::<Timestamp>().unwrap()]);
    let mut h = at_wall(
        "schedule.at",
        json!({ "time": "21:30", "days": ["sat", "sun"], "tz": "UTC" }),
        "2026-10-21T00:00",
        "UTC",
    );
    h.advance(Seconds(7.0 * 24.0 * HOUR));
    let start = instant("2026-10-21T00:00", "UTC");
    assert_eq!(
        fired_at(&h, start),
        ["2026-10-24T21:30:00Z", "2026-10-25T21:30:00Z"].map(|t| t.parse::<Timestamp>().unwrap())
    );
}

#[test]
fn a_wall_jump_forward_fires_once_and_a_jump_back_doesnt_fire_again() {
    let mut h = at_wall("schedule.at", json!({ "time": "09:00", "tz": "UTC" }), "2026-10-21T08:00", "UTC");
    h.advance(Seconds(10.0));
    h.wall().jump(2 * 3_600_000);
    h.advance(Seconds(60.0));
    assert_eq!(h.fired().len(), 1, "the jump past 09:00 fires it");
    h.advance(Seconds(HOUR));
    h.wall().jump(-3_600_000);
    h.advance(Seconds(3.0 * HOUR));
    assert_eq!(h.fired().len(), 1, "back over 09:00 the same day doesn't fire again");
    h.advance(Seconds(24.0 * HOUR));
    assert_eq!(h.fired().len(), 2, "the next day does");
}

// ── conditions ─────────────────────────────────────────────────────────────────────────────────────────────────

fn value_at(kind: &str, params: &Value, local: &str, tz: &str) -> Val {
    at_wall(kind, params.clone(), local, tz).read("value").unwrap()
}

#[test]
fn time_between_runs_over_midnight() {
    let late = json!({ "from": "22:00", "to": "02:00", "tz": "Europe/Berlin" });
    for (local, want) in [
        ("2026-10-21T23:59", true),
        ("2026-10-22T01:59", true),
        ("2026-10-22T02:00", false),
        ("2026-10-22T21:59", false),
        ("2026-10-22T22:00", true),
    ] {
        assert_eq!(value_at("cond.time_between", &late, local, "Europe/Berlin"), Val::Bool(want), "{local}");
    }
    // After midnight belongs to the day the range started: Friday night's range holds early Saturday.
    let friday = json!({ "from": "22:00", "to": "02:00", "days": ["fri"], "tz": "UTC" });
    assert_eq!(value_at("cond.time_between", &friday, "2026-10-24T01:00", "UTC"), Val::Bool(true));
    assert_eq!(value_at("cond.time_between", &friday, "2026-10-23T01:00", "UTC"), Val::Bool(false));
}

#[test]
fn date_ranges_repeat_yearly_or_hold_once() {
    let winter = json!({ "from": "12-20", "to": "01-05", "tz": "UTC" });
    for (local, want) in [("2026-12-31T12:00", true), ("2027-01-05T23:00", true), ("2027-01-06T00:00", false)] {
        assert_eq!(value_at("cond.date", &winter, local, "UTC"), Val::Bool(want), "{local}");
    }
    let once = json!({ "from": "2026-12-20", "to": "2027-01-05", "tz": "UTC" });
    assert_eq!(value_at("cond.date", &once, "2026-12-25T00:00", "UTC"), Val::Bool(true));
    assert_eq!(value_at("cond.date", &once, "2027-12-25T00:00", "UTC"), Val::Bool(false));
}

#[test]
fn check_refuses_bad_parameters_naming_them() {
    let paths = |kind: &str, params: Value| -> Vec<String> {
        let Err(errors) = node(kind, params) else { panic!("{kind} took bad parameters") };
        errors.0.iter().map(|e| e.path.clone()).collect()
    };
    assert_eq!(paths("timer.every", json!({ "seconds": 0.01 })), ["seconds"]);
    assert_eq!(
        paths("schedule.at", json!({ "time": "9:00", "days": [], "tz": "Mars/Olympus" })),
        ["time", "days", "tz"]
    );
    assert_eq!(paths("cond.time_between", json!({ "from": "25:00", "to": "02:00" })), ["from"]);
    assert_eq!(paths("cond.date", json!({ "from": "12-20", "to": "2027-01-05" })), ["to"]);
    assert_eq!(paths("cond.date", json!({ "from": "2027-01-05", "to": "2026-12-20" })), ["to"]);
}
