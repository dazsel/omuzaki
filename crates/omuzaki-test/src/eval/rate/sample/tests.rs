//! Sampling on synthetic histories: stratified across tasks, skipping what is rated, held to the weekly budget.

use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use super::*;
use crate::eval::clock::utc_minute;
use crate::eval::history::{SCHEMA, Usage};
use crate::eval::rate::Answer;
use crate::eval::{Discovery, TaskFile};

fn task(id: &str, questions: &[&str]) -> TaskFile {
    TaskFile {
        path: PathBuf::from(format!("crates/c/evals/{id}.toml")),
        crate_name: "c".to_owned(),
        task: toml::from_str(&format!(
            "id='{id}'\nfeature='QG-03'\ntitle='t'\nprompt='Prompt of {id}.'\nfixture='f'\nscopes=['read']\nrubric_for_people={questions:?}\n"
        ))
        .unwrap(),
        bytes: Vec::new(),
        scripts: Vec::new(),
    }
}

fn discovery() -> Discovery {
    Discovery {
        tasks: vec![
            task("a.one", &["Is it round?"]),
            task("b.two", &["Is it blue?", "Is it tall?"]),
            task("c.none", &[]),
        ],
        ..Discovery::default()
    }
}

fn record(task: &str, attempt: u32, outcome: AttemptOutcome, driver: &str) -> AttemptRecord {
    AttemptRecord {
        v: SCHEMA,
        run: "2026-10-13T10:00Z-nightly".to_owned(),
        commit: "c".to_owned(),
        family: "task".to_owned(),
        driver: driver.to_owned(),
        model: Some("m".to_owned()),
        effort: None,
        task: task.to_owned(),
        task_version: 1,
        task_hash: "b3:x".to_owned(),
        feature: "QG-03".to_owned(),
        attempt,
        outcome,
        reason: None,
        checks: Vec::new(),
        turns: 1,
        tool_calls: 1,
        tool_errors: 0,
        retries: 0,
        usage: Usage::default(),
        usd_micros: 0,
        wall_ms: 0,
        tools_hash: String::new(),
        prompt_hash: String::new(),
        harness: String::new(),
        transcript: String::new(),
        script: None,
    }
}

fn rated(item: &str, at: &str) -> Rating {
    Rating {
        item: item.to_owned(),
        task: "a.one".to_owned(),
        attempt: 1,
        question: "Is it round?".to_owned(),
        answer: Answer::Yes,
        rater: "r".to_owned(),
        at: at.to_owned(),
    }
}

fn ids(items: &[Item]) -> Vec<String> {
    items.iter().map(|i| format!("{}#{}", i.task, i.attempt)).collect()
}

#[test]
fn the_sample_takes_one_task_after_another_so_none_fills_it() {
    // Four attempts of the first task, two of the second: asked for four, two of each come out in turn, newest first.
    let mut history = Vec::new();
    for attempt in 1..=4 {
        history.push(record("a.one", attempt, AttemptOutcome::Pass, "reference"));
    }
    for attempt in 1..=2 {
        history.push(record("b.two", attempt, AttemptOutcome::Fail, "reference"));
    }
    let sample = sample(&discovery(), &history, &[], 4);
    assert_eq!(ids(&sample), ["a.one#4", "b.two#2", "a.one#3", "b.two#1"]);
    assert_eq!(sample[1].questions, ["Is it blue?", "Is it tall?"]);
    assert_eq!(sample[0].prompt, "Prompt of a.one.");
    assert_eq!(sample[0].id, "2026-10-13T10:00Z-nightly/a.one/4");
    // Asked for more than there are, it returns what there is.
    assert_eq!(super::sample(&discovery(), &history, &[], 50).len(), 6);
}

#[test]
fn rated_items_and_unfit_attempts_are_skipped() {
    let mut history = vec![
        record("a.one", 1, AttemptOutcome::Pass, "reference"),
        record("a.one", 2, AttemptOutcome::Pass, "reference"),
        record("a.one", 3, AttemptOutcome::Error, "reference"),
        record("a.one", 4, AttemptOutcome::Refusal, "reference"),
        record("a.one", 5, AttemptOutcome::Pass, "scripted"),
        record("c.none", 1, AttemptOutcome::Pass, "reference"),
        record("gone.task", 1, AttemptOutcome::Pass, "reference"),
    ];
    let already = rated("2026-10-13T10:00Z-nightly/a.one/2", "2026-10-13T10:00Z");
    assert_eq!(ids(&sample(&discovery(), &history, std::slice::from_ref(&already), 10)), ["a.one#1"]);
    history.truncate(2);
    assert_eq!(ids(&sample(&discovery(), &history, &[already], 10)), ["a.one#1"]);
    assert_eq!(sample(&discovery(), &history, &[], 0).len(), 0);
}

#[test]
fn a_week_allows_twenty_items_and_a_week_old_rating_no_longer_counts() {
    let now = UNIX_EPOCH + Duration::from_secs(1_790_000_000);
    let ago = |days: u64| utc_minute(now - Duration::from_secs(days * 86_400));
    let ratings: Vec<Rating> = (0..8)
        .map(|n| rated(&format!("run/t/{n}"), &ago(if n < 5 { 1 } else { 9 })))
        .chain([rated("run/t/0", &ago(1))])
        .collect();
    // Five distinct items in the last week (a second question on one item is not another item).
    assert_eq!(budget_left(&ratings, now), WEEKLY_ITEMS - 5);
    assert_eq!(budget_left(&[], now), WEEKLY_ITEMS);
    // A rating with an unreadable time counts as recent.
    assert_eq!(budget_left(&[rated("x/y/1", "yesterday")], now), WEEKLY_ITEMS - 1);
}
