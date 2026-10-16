//! The `doc.*` checks: the result document, and how it moved from the fixture, read through `Runner::get` snapshots.
//!
//! Both documents are read once per checker, whole, as the checker (`read` and `render` only). Paths and globs are
//! [`docglob`](super::docglob)'s. What reads the **result** and finds the agent's work missing is a fail; what reads
//! the **fixture** and finds the task's premise missing is an error, because the fixture is not the agent's.

use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{Grants, Scope};
use serde_json::Value;

use super::Checker;
use super::docglob::{Segments, at, covers, differences, join, matches, split};
use super::number::{Num, in_range, moved, same_value};
use super::report::Verdict;
use super::value::result_failure;
use crate::eval::task::Change;

/// The most differing paths a detail line names.
const SHOWN: usize = 3;

impl Checker {
    /// The whole document of the fixture (`result == false`) or the result, read once.
    fn snapshot(&self, result: bool) -> Result<&Value, Verdict> {
        let (cell, runner) = if result { (&self.result_doc, &self.result) } else { (&self.fixture_doc, &self.fixture) };
        cell.get_or_init(|| {
            let grants = Grants::of([Scope::Read, Scope::Render]);
            match runner.get(&DocPath::root(), &grants) {
                Ok(doc) => Ok(doc.unwrap_or(Value::Null)),
                Err(e) if result => Err(result_failure("the document", &e)),
                Err(e) => Err(Verdict::error(format!("the document of the fixture: {e}"))),
            }
        })
        .as_ref()
        .map_err(Clone::clone)
    }

    /// `doc.equals`: every value `path` reaches in the result equals `value`, and there is at least one.
    pub(super) fn doc_equals(&self, path: &str, value: &Value) -> Verdict {
        let found = match self.reached(path) {
            Ok(found) => found,
            Err(verdict) => return verdict,
        };
        if let Some((at, got)) = found.iter().find(|(_, got)| !same_value(value, got)) {
            return Verdict::fail(format!("`{}` is {got}, expected {value}", join(at)))
                .seen(None, Some((*got).clone()));
        }
        let first = found.values().next().map(|v| (*v).clone());
        Verdict::pass(format!("{} value(s) at `{path}` equal {value}", found.len())).seen(None, first)
    }

    /// `doc.range`: every value `path` reaches is a number in `min..=max` (either bound may be left out).
    pub(super) fn doc_range(&self, path: &str, min: Option<f64>, max: Option<f64>) -> Verdict {
        let found = match self.reached(path) {
            Ok(found) => found,
            Err(verdict) => return verdict,
        };
        let (low, high) = (min.unwrap_or(f64::NEG_INFINITY), max.unwrap_or(f64::INFINITY));
        for (at, got) in &found {
            match Num::of(got) {
                Some(n) if in_range(n, low, high) => {}
                Some(_) => return Verdict::fail(format!("`{}` is {got}, outside {low}..={high}", join(at))),
                None => return Verdict::fail(format!("`{}` is {got}, not a number", join(at))),
            }
        }
        let first = found.values().next().map(|v| (*v).clone());
        Verdict::pass(format!("{} value(s) at `{path}` lie in {low}..={high}", found.len())).seen(None, first)
    }

    /// `doc.exists` (`want == true`) and `doc.absent` (`want == false`).
    pub(super) fn doc_exists(&self, path: &str, want: bool) -> Verdict {
        let result = match self.snapshot(true) {
            Ok(doc) => doc,
            Err(verdict) => return verdict,
        };
        let count = matches(result, &split(path)).len();
        match (want, count) {
            (true, 0) => Verdict::fail(format!("nothing at `{path}`")),
            (true, n) => Verdict::pass(format!("{n} value(s) at `{path}`")),
            (false, 0) => Verdict::pass(format!("nothing at `{path}`")),
            (false, n) => Verdict::fail(format!("{n} value(s) at `{path}`, expected none")),
        }
    }

    /// `doc.change`: the one value at `path` moved as `change` says between the fixture and the result.
    pub(super) fn doc_change(&self, path: &str, change: Change, at_least: Option<f64>, relative: bool) -> Verdict {
        let segments = split(path);
        let before = match self.snapshot(false) {
            Ok(doc) => doc,
            Err(verdict) => return verdict,
        };
        let before = match one(before, &segments, path) {
            Ok(value) => value.clone(),
            Err(why) => return Verdict::error(format!("on the fixture: {why}")),
        };
        let result = match self.snapshot(true) {
            Ok(doc) => doc,
            Err(verdict) => return verdict,
        };
        let after = match one(result, &segments, path) {
            Ok(value) => value.clone(),
            Err(why) => return Verdict::fail(why).seen(Some(before), None),
        };
        let verdict = match (Num::of(&before), Num::of(&after)) {
            (Some(b), Some(a)) => Verdict::when(
                moved(b, a, change, at_least, relative),
                format!("`{path}` went from {before} to {after}; asked for {change:?} by the amount given"),
            ),
            _ if matches!(change, Change::None | Change::Any) => {
                let same = same_value(&before, &after);
                Verdict::when(same == (change == Change::None), format!("`{path}` went from {before} to {after}"))
            }
            (None, _) => Verdict::error(format!("change needs a number on the fixture, found {before}")),
            (Some(_), None) => Verdict::fail(format!("change needs a number, found {before} then {after}")),
        };
        verdict.seen(Some(before), Some(after))
    }

    /// `doc.changed_only`: every difference between the fixture and the result lies under one of `paths`.
    pub(super) fn doc_changed_only(&self, paths: &[String]) -> Verdict {
        let (before, after) = match self.both() {
            Ok(both) => both,
            Err(verdict) => return verdict,
        };
        let allowed: Vec<Segments> = paths.iter().map(|p| split(p)).collect();
        let outside: Vec<Segments> =
            differences(before, after).into_iter().filter(|d| !allowed.iter().any(|p| covers(p, d))).collect();
        report_differences(
            &outside,
            "changed outside the allowed paths",
            "every difference lies under the allowed paths",
        )
    }

    /// `doc.unchanged`: nothing under `paths` (the whole document when empty) differs.
    pub(super) fn doc_unchanged(&self, paths: &[String]) -> Verdict {
        let (before, after) = match self.both() {
            Ok(both) => both,
            Err(verdict) => return verdict,
        };
        let mut changed: Vec<Segments> = Vec::new();
        if paths.is_empty() {
            changed = differences(before, after);
        } else {
            // The pattern is read on both sides, so a value the agent removed is found through the fixture and one it
            // added through the result: a path counts as changed when its two sides differ.
            for pattern in paths.iter().map(|p| split(p)) {
                let mut reached: Vec<Segments> = matches(before, &pattern).into_keys().collect();
                reached.extend(matches(after, &pattern).into_keys());
                reached.sort();
                reached.dedup();
                for path in reached {
                    let (b, a) = (at(before, &path), at(after, &path));
                    let same = match (b, a) {
                        (Some(b), Some(a)) => differences(b, a).is_empty(),
                        (None, None) => true,
                        _ => false,
                    };
                    if !same {
                        changed.push(path);
                    }
                }
            }
            changed.sort();
            changed.dedup();
        }
        report_differences(&changed, "changed", "nothing under the paths changed")
    }

    /// The values `path` reaches in the result: at least one, else a fail.
    fn reached(&self, path: &str) -> Result<std::collections::BTreeMap<Segments, &Value>, Verdict> {
        let result = self.snapshot(true)?;
        let found = matches(result, &split(path));
        if found.is_empty() { Err(Verdict::fail(format!("nothing at `{path}`"))) } else { Ok(found) }
    }

    fn both(&self) -> Result<(&Value, &Value), Verdict> {
        Ok((self.snapshot(false)?, self.snapshot(true)?))
    }
}

/// The single value a path reaches, or why it does not.
fn one<'a>(doc: &'a Value, segments: &Segments, path: &str) -> Result<&'a Value, String> {
    let found = matches(doc, segments);
    let mut values = found.values();
    match (values.next(), values.next()) {
        (Some(value), None) => Ok(value),
        (None, _) => Err(format!("nothing at `{path}`")),
        _ => Err(format!("`{path}` reached {} values; a change reads exactly one", found.len())),
    }
}

fn report_differences(paths: &[Segments], failing: &str, passing: &str) -> Verdict {
    if paths.is_empty() {
        return Verdict::pass(passing);
    }
    let shown: Vec<String> = paths.iter().take(SHOWN).map(|p| format!("`{}`", join(p))).collect();
    let more = paths.len().saturating_sub(SHOWN);
    let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
    Verdict::fail(format!("{} path(s) {failing}: {}{tail}", paths.len(), shown.join(", ")))
}
