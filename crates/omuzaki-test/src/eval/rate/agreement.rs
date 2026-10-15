//! Judge agreement (design 10.3, 10.4): how often a judge says what the raters said, on the items they were sure about.
//!
//! A judge (an ML-11 critic, or the LLM rubric judge) may become a hard check only when it agrees with the people on at
//! least [`PROMOTE_AGREEMENT`] of the clear-cut items of the review set (the ones no rater marked unsure), over at least
//! [`PROMOTE_ITEMS`] items, and passes the known-negatives test. This module measures the first two; the known-negatives
//! test is the judge's own.
//!
//! The unit is a rated question: one rating on one question of one result. Two raters who both rated it are two data
//! points.

use std::collections::BTreeMap;

use serde::Serialize;

use super::sample::item_id;
use super::{Answer, RateError, Rating};
use crate::eval::Discovery;
use crate::eval::history::{AttemptRecord, CheckRecord, SCHEMA};
use crate::eval::task::CheckKind;

/// The share of clear-cut items a judge must agree on (design 10.4).
pub const PROMOTE_AGREEMENT: f64 = 0.9;
/// The fewest clear-cut items that count (design 10.4).
pub const PROMOTE_ITEMS: usize = 40;
/// The history family of an agreement record.
pub const FAMILY_JUDGE: &str = "judge";

/// How well a judge agrees with the raters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Agreement {
    /// Clear-cut ratings the judge answered: what the rate is over.
    pub items: usize,
    /// The results those ratings are about. The promotion threshold counts these, not ratings: ten results with four
    /// questions each are ten results.
    pub results: usize,
    /// Of those, the ones the judge answered as the rater did.
    pub agree: usize,
    /// Ratings left out because the rater was unsure.
    pub unsure: usize,
    /// Clear-cut ratings the judge gave no answer to (it did not run on that result).
    pub unjudged: usize,
}

impl Agreement {
    /// `agree / items`, or `None` when no clear-cut item was judged.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // counts of ratings are far below 2^52
    pub fn rate(&self) -> Option<f64> {
        (self.items > 0).then(|| self.agree as f64 / self.items as f64)
    }

    /// Whether the numbers allow promotion: at least [`PROMOTE_AGREEMENT`] over at least [`PROMOTE_ITEMS`] results.
    #[must_use]
    pub fn meets_threshold(&self) -> bool {
        self.results >= PROMOTE_ITEMS && self.rate().is_some_and(|rate| rate >= PROMOTE_AGREEMENT)
    }
}

/// The agreement of `judge` with `ratings`. `judge` answers a rating's question as the judge would (`true` for yes), or
/// `None` when it has no answer for that result.
pub fn agreement(ratings: &[Rating], judge: &dyn Fn(&Rating) -> Option<bool>) -> Agreement {
    let mut result = Agreement { items: 0, results: 0, agree: 0, unsure: 0, unjudged: 0 };
    let mut judged_results = std::collections::BTreeSet::new();
    for rating in ratings {
        let said = match rating.answer {
            Answer::Unsure => {
                result.unsure += 1;
                continue;
            }
            Answer::Yes => true,
            Answer::No => false,
        };
        match judge(rating) {
            None => result.unjudged += 1,
            Some(answer) => {
                result.items += 1;
                judged_results.insert(rating.item.as_str());
                result.agree += usize::from(answer == said);
            }
        }
    }
    result.results = judged_results.len();
    result
}

/// What a judge said about each result, read from the history, and the question it answers on each task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryJudge {
    answers: BTreeMap<String, bool>,
    questions: BTreeMap<String, String>,
}

impl HistoryJudge {
    /// The judge `id`: the check of that id in the history's attempts. A hard check answers yes when it passed, a soft one
    /// when its score is at least a half.
    ///
    /// The question it answers is its only rubric question when it is a `judge` check; any other check (a critic's
    /// `query`) needs `question`, which must be one of the task's `rubric_for_people`.
    ///
    /// # Errors
    /// [`RateError::Invalid`] when no task has a check called `id`, or the question cannot be told.
    pub fn from_history(
        discovery: &Discovery,
        history: &[AttemptRecord],
        id: &str,
        question: Option<&str>,
    ) -> Result<Self, RateError> {
        let mut questions = BTreeMap::new();
        for file in &discovery.tasks {
            let Some(check) = file.task.checks.iter().find(|c| c.id == id) else { continue };
            let wanted = match (&check.kind, question) {
                (_, Some(question)) => question.to_owned(),
                (CheckKind::Judge { rubric, .. }, None) if rubric.len() == 1 => rubric[0].clone(),
                _ => {
                    return Err(RateError::Invalid(format!(
                        "task `{}`: the check `{id}` does not say which question it answers; pass --question",
                        file.task.id
                    )));
                }
            };
            // A question the raters were never asked matches no rating: the measurement would be all "no judge answer".
            if !file.task.rubric_for_people.contains(&wanted) {
                return Err(RateError::Invalid(format!(
                    "task `{}`: `{wanted}` is not one of its `rubric_for_people` questions ({})",
                    file.task.id,
                    file.task.rubric_for_people.join(" | ")
                )));
            }
            questions.insert(file.task.id.clone(), wanted);
        }
        if questions.is_empty() {
            return Err(RateError::Invalid(format!("no task has a check called `{id}`")));
        }
        let mut answers = BTreeMap::new();
        for record in history.iter().filter(|r| r.driver == "reference") {
            if let Some(said) = record.checks.iter().find(|c| c.id == id).and_then(answer_of) {
                answers.insert(item_id(&record.run, &record.task, record.attempt), said);
            }
        }
        Ok(Self { answers, questions })
    }

    /// What the judge said to this rating's question, if it was asked that question about that result.
    #[must_use]
    pub fn answer(&self, rating: &Rating) -> Option<bool> {
        (self.questions.get(&rating.task)? == &rating.question).then(|| self.answers.get(&rating.item).copied())?
    }
}

fn answer_of(check: &CheckRecord) -> Option<bool> {
    check.passed.or_else(|| check.score.map(|score| score >= 0.5))
}

/// The line `eval agreement` appends to the history (`family = "judge"`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct JudgeRecord {
    /// [`SCHEMA`].
    pub v: u32,
    /// The run the measurement belongs to.
    pub run: String,
    /// The commit.
    pub commit: String,
    /// [`FAMILY_JUDGE`].
    pub family: &'static str,
    /// The judge: a check id or a critic op.
    pub judge: String,
    /// The review set.
    pub review_set: String,
    /// Clear-cut ratings the judge answered.
    pub items: usize,
    /// The results they are about.
    pub results: usize,
    /// Of those, the agreeing ones.
    pub agree: usize,
    /// Ratings left out as unsure.
    pub unsure: usize,
    /// Clear-cut ratings with no judge answer.
    pub unjudged: usize,
    /// `agree / items`, when there are items.
    pub agreement: Option<f64>,
    /// Whether the numbers meet [`PROMOTE_AGREEMENT`] over [`PROMOTE_ITEMS`] results.
    pub meets_threshold: bool,
}

impl JudgeRecord {
    /// The record of a measurement.
    #[must_use]
    pub fn new(run: &str, commit: &str, judge: &str, review_set: &str, measured: &Agreement) -> Self {
        Self {
            v: SCHEMA,
            run: run.to_owned(),
            commit: commit.to_owned(),
            family: FAMILY_JUDGE,
            judge: judge.to_owned(),
            review_set: review_set.to_owned(),
            items: measured.items,
            results: measured.results,
            agree: measured.agree,
            unsure: measured.unsure,
            unjudged: measured.unjudged,
            agreement: measured.rate(),
            meets_threshold: measured.meets_threshold(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rating(item: &str, answer: Answer) -> Rating {
        Rating {
            item: item.to_owned(),
            task: "t".to_owned(),
            attempt: 1,
            question: "Round?".to_owned(),
            answer,
            rater: "r".to_owned(),
            at: "2026-10-13T10:00Z".to_owned(),
        }
    }

    #[test]
    fn a_judge_agreeing_on_nine_of_ten_clear_items_scores_point_nine_over_ten() {
        // Ten clear-cut items the judge says "yes" to; the raters say yes to nine; one item the raters were unsure of.
        let mut ratings: Vec<Rating> = (0..9).map(|n| rating(&format!("r/t/{n}"), Answer::Yes)).collect();
        ratings.push(rating("r/t/9", Answer::No));
        ratings.push(rating("r/t/10", Answer::Unsure));
        let measured = agreement(&ratings, &|_| Some(true));
        assert_eq!(
            (measured.items, measured.results, measured.agree, measured.unsure, measured.unjudged),
            (10, 10, 9, 1, 0)
        );
        assert_eq!(measured.rate(), Some(0.9));
        assert!(!measured.meets_threshold(), "ten items are not forty");
    }

    #[test]
    fn unsure_items_are_left_out_whatever_the_judge_says_and_unjudged_ones_are_counted_apart() {
        let ratings = vec![rating("a", Answer::Unsure), rating("b", Answer::Yes), rating("c", Answer::No)];
        let measured = agreement(&ratings, &|r| (r.item != "c").then_some(true));
        assert_eq!((measured.items, measured.agree, measured.unsure, measured.unjudged), (1, 1, 1, 1));
        assert_eq!(agreement(&[], &|_| Some(true)).rate(), None);
        assert_eq!(agreement(&[rating("a", Answer::Unsure)], &|_| Some(true)).rate(), None);
    }

    #[test]
    fn the_threshold_is_ninety_percent_over_forty_clear_items() {
        let at = |agree, results| Agreement { items: results, results, agree, unsure: 0, unjudged: 0 };
        assert!(at(36, 40).meets_threshold());
        assert!(!at(35, 40).meets_threshold());
        assert!(!at(39, 39).meets_threshold());
        assert!(at(41, 41).meets_threshold());
        // Forty clear-cut ratings about ten results are ten results.
        assert!(!Agreement { items: 40, results: 10, agree: 40, unsure: 0, unjudged: 0 }.meets_threshold());
    }

    #[test]
    fn the_history_record_carries_the_numbers_and_the_family() {
        let record = JudgeRecord::new(
            "run",
            "abc1234",
            "reads-oval",
            "review-1",
            &Agreement { items: 10, results: 5, agree: 9, unsure: 2, unjudged: 1 },
        );
        let json: serde_json::Value = serde_json::to_value(&record).unwrap();
        assert_eq!(json["family"], "judge");
        assert_eq!(
            (json["items"].clone(), json["agree"].clone(), json["agreement"].clone()),
            (10.into(), 9.into(), 0.9.into())
        );
        assert_eq!(json["meets_threshold"], false);
    }
}
