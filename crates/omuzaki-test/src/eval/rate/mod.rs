//! The human-rated review set (design 10.3): a few rendered results a week, rated yes, no or unsure on each task's
//! `rubric_for_people` questions, through a page served on `127.0.0.1` only (nothing is uploaded).
//!
//! The ratings are the ground truth judges are calibrated against ([`agreement`]) and give a human pass rate for
//! open-ended tasks. They are appended to `<evals folder>/ratings/<set>.jsonl`, one line per answered question, in the
//! assets folder and never in git.
//!
//! - [`sample`]: which results to ask about: unrated, stratified across tasks, at most 20 a week
//! - [`server`]: the local page and the three requests it makes
//! - [`agreement`]: how often a judge's answers equal the raters', on the items they were sure about
//!
//! An attempt's folder may hold `before.png` and `after.png` (the renders of the fixture and of the result); the page
//! shows them when they are there. The scripted driver writes none; the reference driver's render checks do.

pub mod agreement;
pub mod sample;
pub mod server;

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Items a rater is asked about in one week, at most (design 10.3).
pub const WEEKLY_ITEMS: usize = 20;
/// The seconds in the week the budget is counted over.
pub const WEEK_SECS: u64 = 7 * 86_400;

/// A rating answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// The rubric question holds.
    Yes,
    /// It does not.
    No,
    /// The rater could not tell: left out of agreement.
    Unsure,
}

/// One answered question: a line of the ratings file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rating {
    /// The result rated: `<run>/<task>/<attempt>`.
    pub item: String,
    /// The task.
    pub task: String,
    /// The attempt number.
    pub attempt: u32,
    /// The question, as the task's `rubric_for_people` words it.
    pub question: String,
    /// The answer.
    pub answer: Answer,
    /// Who rated.
    pub rater: String,
    /// When: the minute, as a run id begins (`2026-10-13T10:00Z`).
    pub at: String,
}

/// Why the ratings could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum RateError {
    /// A file could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// A line is not a rating.
    #[error("{}:{line}: {reason}", path.display())]
    Line {
        /// The file.
        path: PathBuf,
        /// 1-based line number.
        line: usize,
        /// What is wrong.
        reason: String,
    },
    /// A set name or request that cannot be used.
    #[error("{0}")]
    Invalid(String),
}

/// The ratings file of a set, inside the evals folder.
///
/// # Errors
/// [`RateError::Invalid`] when `set` is not lowercase words joined by dashes: it becomes a file name.
pub fn ratings_path(evals: &Path, set: &str) -> Result<PathBuf, RateError> {
    let word = |w: &str| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if !set.split('-').all(word) {
        return Err(RateError::Invalid(format!(
            "`{set}` is not a review-set name: lowercase letters and digits, joined by dashes"
        )));
    }
    Ok(evals.join("ratings").join(format!("{set}.jsonl")))
}

/// Every rating in `path`, oldest first. A missing file is an empty set.
///
/// # Errors
/// [`RateError`] for a file that cannot be read or a line that is not a rating: a ratings file is the ground truth, so
/// a line that cannot be read is not skipped.
pub fn read(path: &Path) -> Result<Vec<Rating>, RateError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(RateError::Io { path: path.to_owned(), source }),
    };
    text.lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|e| RateError::Line {
                path: path.to_owned(),
                line: index + 1,
                reason: e.to_string(),
            })
        })
        .collect()
}

/// Appends a rating as one line, with one `write` of the whole line to a file in append mode.
///
/// # Errors
/// [`RateError::Io`] when the file cannot be written.
pub fn append(path: &Path, rating: &Rating) -> Result<(), RateError> {
    append_all(path, std::slice::from_ref(rating))
}

/// Appends `ratings` in one write: the lines are put together first and written with a single call to a file opened for
/// appending, so a result's answers reach the file together or, when the write fails, not at all as far as a reader can
/// tell (a partial write leaves a line that does not parse, which [`read`] reports instead of counting).
///
/// # Errors
/// [`RateError::Io`] when the file cannot be written.
pub fn append_all(path: &Path, ratings: &[Rating]) -> Result<(), RateError> {
    let io_err = |source| RateError::Io { path: path.to_owned(), source };
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(io_err)?;
    }
    let mut lines = String::new();
    for rating in ratings {
        lines.push_str(&serde_json::to_string(rating).map_err(|e| io_err(io::Error::other(e)))?);
        lines.push('\n');
    }
    let mut file = OpenOptions::new().create(true).append(true).open(path).map_err(io_err)?;
    file.write_all(lines.as_bytes()).map_err(io_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rating(item: &str, question: &str, answer: Answer) -> Rating {
        Rating {
            item: item.to_owned(),
            task: "t".to_owned(),
            attempt: 1,
            question: question.to_owned(),
            answer,
            rater: "r".to_owned(),
            at: "2026-10-13T10:00Z".to_owned(),
        }
    }

    #[test]
    fn ratings_append_one_line_each_and_read_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = ratings_path(dir.path(), "review-1").unwrap();
        assert_eq!(read(&path).unwrap().len(), 0);
        append(&path, &rating("a/t/1", "Q1?", Answer::Yes)).unwrap();
        append(&path, &rating("a/t/1", "Q2?", Answer::Unsure)).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert!(text.lines().next().unwrap().contains("\"answer\":\"yes\""));
        let back = read(&path).unwrap();
        assert_eq!((back[0].answer, back[1].answer), (Answer::Yes, Answer::Unsure));
    }

    #[test]
    fn a_line_that_is_not_a_rating_is_an_error_and_a_set_name_is_a_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = ratings_path(dir.path(), "review-1").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{\"item\": 1}\n").unwrap();
        assert!(matches!(read(&path), Err(RateError::Line { line: 1, .. })));
        for bad in ["", "Review", "a b", "../x", "a--b", "a/b"] {
            assert!(ratings_path(dir.path(), bad).is_err(), "{bad:?}");
        }
    }
}
