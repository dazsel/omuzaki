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
    // A last line with no newline is a write that stopped half way (`append_all` cuts it before the next write). It is
    // not a rating yet, so it is not an error here: a restart after a crash can read the file and go on. Any other line
    // that cannot be read still is.
    let torn_tail = !text.is_empty() && !text.ends_with('\n');
    let lines: Vec<&str> = text.lines().collect();
    let complete = if torn_tail && serde_json::from_str::<Rating>(lines.last().copied().unwrap_or_default()).is_err() {
        &lines[..lines.len() - 1]
    } else {
        &lines[..]
    };
    let all: Vec<Rating> = complete
        .iter()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line).map_err(|e| RateError::Line {
                path: path.to_owned(),
                line: index + 1,
                reason: e.to_string(),
            })
        })
        .collect::<Result<_, _>>()?;
    // An answer counts once: the first line of a rater for an item and a question stands (a failed batch that was written
    // again leaves the same answers twice in an older file). Two raters' answers are both kept: each is a vote.
    let mut seen = std::collections::BTreeSet::new();
    Ok(all.into_iter().filter(|r| seen.insert(key_of(r))).collect())
}

/// What makes an answer the same answer: who gave it, to which question about which result.
fn key_of(rating: &Rating) -> (String, String, String) {
    (rating.item.clone(), rating.question.clone(), rating.rater.clone())
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
    let mut file = OpenOptions::new().read(true).append(true).create(true).open(path).map_err(io_err)?;
    let mut existing = Vec::new();
    io::Read::read_to_end(&mut file, &mut existing).map_err(io_err)?;
    // A write that stopped half way left a last line with no newline. Cut it before writing on, or the retry would be
    // glued to it and make a line that does not parse. (A complete rating that only lacks its newline is kept.)
    let mut lines = String::new();
    let mut kept = existing.len();
    if !existing.is_empty() && existing.last() != Some(&b'\n') {
        let start = existing.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        if serde_json::from_slice::<Rating>(&existing[start..]).is_ok() {
            lines.push('\n');
        } else {
            file.set_len(start as u64).map_err(io_err)?;
            kept = start;
        }
    }
    // Each answer is written once: a batch that failed part-way and is sent again finds its first rows already there.
    let present: std::collections::BTreeSet<(String, String, String)> = String::from_utf8_lossy(&existing[..kept])
        .lines()
        .filter_map(|l| serde_json::from_str::<Rating>(l).ok())
        .map(|r| key_of(&r))
        .collect();
    for rating in ratings.iter().filter(|r| !present.contains(&key_of(r))) {
        lines.push_str(&serde_json::to_string(rating).map_err(|e| io_err(io::Error::other(e)))?);
        lines.push('\n');
    }
    file.write_all(lines.as_bytes()).map_err(io_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_torn_last_line_is_cut_before_the_next_write_and_a_whole_one_is_kept() {
        let dir = std::env::temp_dir().join(format!("ratings-torn-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.jsonl");
        let one = rating("i/1", "Q?", Answer::Yes);
        let line = serde_json::to_string(&one).unwrap();
        // A good line, then half of another.
        fs::write(&path, format!("{line}\n{}", &line[..line.len() / 2])).unwrap();
        append_all(&path, &[rating("i/2", "Q?", Answer::No)]).unwrap();
        let read_back = read(&path).unwrap();
        assert_eq!(read_back.iter().map(|r| r.item.as_str()).collect::<Vec<_>>(), ["i/1", "i/2"]);
        // A whole rating that only lost its newline is not thrown away.
        fs::write(&path, &line).unwrap();
        append_all(&path, &[rating("i/3", "Q?", Answer::No)]).unwrap();
        assert_eq!(read(&path).unwrap().len(), 2);
        // Torn from the very first byte on.
        fs::write(&path, &line[..10]).unwrap();
        append_all(&path, &[rating("i/4", "Q?", Answer::No)]).unwrap();
        assert_eq!(read(&path).unwrap().len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_batch_that_failed_part_way_is_not_written_twice_when_it_is_sent_again() {
        let dir = std::env::temp_dir().join(format!("ratings-batch-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.jsonl");
        let batch =
            [rating("i/1", "A?", Answer::Yes), rating("i/1", "B?", Answer::No), rating("i/1", "C?", Answer::Yes)];
        let line = |r: &Rating| serde_json::to_string(r).unwrap();
        let questions = |path: &Path| read(path).unwrap().iter().map(|r| r.question.clone()).collect::<Vec<_>>();
        // The write failed after one complete row.
        fs::write(&path, format!("{}\n", line(&batch[0]))).unwrap();
        append_all(&path, &batch).unwrap();
        assert_eq!(questions(&path), ["A?", "B?", "C?"]);
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 3, "no row twice in the file");
        // The write failed in the middle of the next row.
        let second = line(&batch[1]);
        fs::write(&path, format!("{}\n{}", line(&batch[0]), &second[..second.len() / 2])).unwrap();
        append_all(&path, &batch).unwrap();
        assert_eq!(questions(&path), ["A?", "B?", "C?"]);
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 3);
        // Sent a third time, nothing is added.
        append_all(&path, &batch).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap().lines().count(), 3);
        // An older file with a row twice reads it once.
        fs::write(&path, format!("{}\n{}\n", line(&batch[0]), line(&batch[0]))).unwrap();
        assert_eq!(read(&path).unwrap().len(), 1);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn two_raters_answers_to_the_same_question_are_both_kept_and_a_torn_tail_is_not_an_error() {
        let dir = std::env::temp_dir().join(format!("ratings-raters-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("r.jsonl");
        let by = |rater: &str, answer: Answer| Rating { rater: rater.to_owned(), ..rating("i/1", "Q?", answer) };
        append_all(&path, &[by("ann", Answer::Yes)]).unwrap();
        append_all(&path, &[by("bob", Answer::No)]).unwrap();
        // The same rater again adds nothing.
        append_all(&path, &[by("ann", Answer::No)]).unwrap();
        let read_back = read(&path).unwrap();
        assert_eq!(
            read_back.iter().map(|r| (r.rater.as_str(), r.answer)).collect::<Vec<_>>(),
            [("ann", Answer::Yes), ("bob", Answer::No)]
        );
        // A restart after a write that stopped half way: the file reads, without the torn row.
        let torn = serde_json::to_string(&by("cy", Answer::Yes)).unwrap();
        let mut text = fs::read_to_string(&path).unwrap();
        text.push_str(&torn[..torn.len() / 2]);
        fs::write(&path, &text).unwrap();
        assert_eq!(read(&path).unwrap().len(), 2);
        // A bad line that is not the last one is still an error.
        fs::write(&path, format!("not json\n{}\n", serde_json::to_string(&by("ann", Answer::Yes)).unwrap())).unwrap();
        assert!(read(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }

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
