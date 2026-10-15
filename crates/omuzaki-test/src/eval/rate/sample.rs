//! Which results to ask a rater about (design 10.3): reference attempts on the tasks that have questions for people,
//! unrated, spread across the tasks, and no more than a week's worth.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, SystemTime};

use super::{Rating, WEEK_SECS, WEEKLY_ITEMS};
use crate::eval::Discovery;
use crate::eval::clock::run_started;
use crate::eval::history::{AttemptOutcome, AttemptRecord};

/// A result to rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// `<run>/<task>/<attempt>`.
    pub id: String,
    /// The run.
    pub run: String,
    /// The task.
    pub task: String,
    /// The attempt number.
    pub attempt: u32,
    /// What the person typed.
    pub prompt: String,
    /// The yes/no questions to ask.
    pub questions: Vec<String>,
}

/// The id of a result.
#[must_use]
pub fn item_id(run: &str, task: &str, attempt: u32) -> String {
    format!("{run}/{task}/{attempt}")
}

/// How many more items may be rated this week: [`WEEKLY_ITEMS`] less those rated in the last seven days as of `now`.
/// A rating whose time cannot be read counts as recent, so the budget never grows by a malformed line.
#[must_use]
pub fn budget_left(ratings: &[Rating], now: SystemTime) -> usize {
    let week_ago = now.checked_sub(Duration::from_secs(WEEK_SECS));
    let recent: BTreeSet<&str> = ratings
        .iter()
        .filter(|r| run_started(&r.at).is_none_or(|at| week_ago.is_none_or(|ago| at >= ago)))
        .map(|r| r.item.as_str())
        .collect();
    WEEKLY_ITEMS.saturating_sub(recent.len())
}

/// Up to `count` unrated results, one task after another in turn (tasks in id order, each task's newest attempt first), so
/// that no task fills the sample while another is never asked about.
///
/// Considered are scored (`pass`/`fail`) attempts of the reference driver whose task has `rubric_for_people` and
/// whose item has no rating in `ratings`. `count` is held to the weekly budget by the caller ([`budget_left`]).
#[must_use]
pub fn sample(discovery: &Discovery, history: &[AttemptRecord], ratings: &[Rating], count: usize) -> Vec<Item> {
    let rated: BTreeSet<&str> = ratings.iter().map(|r| r.item.as_str()).collect();
    let mut per_task: BTreeMap<&str, Vec<Item>> = BTreeMap::new();
    for record in history.iter().rev() {
        if record.driver != "reference" || !matches!(record.outcome, AttemptOutcome::Pass | AttemptOutcome::Fail) {
            continue;
        }
        let Some(file) = discovery.tasks.iter().find(|t| t.task.id == record.task) else { continue };
        if file.task.rubric_for_people.is_empty() {
            continue;
        }
        let id = item_id(&record.run, &record.task, record.attempt);
        if rated.contains(id.as_str()) {
            continue;
        }
        per_task.entry(file.task.id.as_str()).or_default().push(Item {
            id,
            run: record.run.clone(),
            task: record.task.clone(),
            attempt: record.attempt,
            prompt: file.task.prompt.clone(),
            questions: file.task.rubric_for_people.clone(),
        });
    }
    let mut queues: Vec<std::vec::IntoIter<Item>> = per_task.into_values().map(Vec::into_iter).collect();
    let mut chosen = Vec::new();
    while chosen.len() < count {
        let before = chosen.len();
        for queue in &mut queues {
            if chosen.len() < count {
                chosen.extend(queue.next());
            }
        }
        if chosen.len() == before {
            break;
        }
    }
    chosen
}

#[cfg(test)]
mod tests;
