//! What a check says, validated before anything is graded.
//!
//! A typo in a check must stop the evaluation, not weaken it: a threshold that is negative, a modifier that means
//! nothing without the key it modifies, a range that cannot hold, a pointer that does not follow RFC 6901. Every such
//! form is a harness `error` here, once, so the graders (`judge` and the rest) read input that is already valid and
//! need not guard against it. [`kind`] runs for every check from [`super::resolve::check`], and [`bind`] for every
//! binding before it resolves.
//!
//! The grammar, key by key (the field names in the messages are the task file's):
//! - `query.expect` gives at least one of `equals`, `range`, `change`.
//! - `at_least` and `relative` modify `change` and mean nothing without it. `relative` needs `at_least`. `change = "none"`
//!   takes neither. `at_least` is finite and 0 or more.
//! - `range` has finite bounds with `min <= max`; `doc.range` the same for the bounds it gives, and gives at least one.
//! - `equals` can be a number when `range` or `change` is given (they read numbers), and then must lie in the `range`.
//! - `render.diff.expect` gives at least one threshold; each is a finite fraction in `0..=1`, and
//!   `changed_fraction_at_least <= changed_fraction_at_most`.
//! - `gate` names ops or reads the declared gates; op names are not empty.
//! - A binding's `op` is not empty, and its `at` is a pointer.

use super::report::Verdict;
use super::value::well_formed;
use crate::eval::task::{Bind, Change, CheckKind, QueryExpect, RenderDiffExpect};

fn invalid(what: impl std::fmt::Display) -> Verdict {
    Verdict::error(format!("invalid check: {what}"))
}

/// Checks one check's own fields.
///
/// # Errors
/// An `error` verdict naming the field that is malformed.
pub(super) fn kind(kind: &CheckKind) -> Result<(), Verdict> {
    match kind {
        CheckKind::Query { op, at, expect, .. } => {
            named("op", op)?;
            well_formed(at)?;
            query_expect(expect)
        }
        CheckKind::Gate { ops, declared } => {
            if ops.is_empty() && !declared {
                return Err(invalid("a gate check names no `ops` and does not set `declared`"));
            }
            ops.iter().try_for_each(|op| named("ops", op))
        }
        CheckKind::LogApplied { op } | CheckKind::LogNotApplied { op } => named("op", op),
        CheckKind::LogWithin { ops } => ops.iter().try_for_each(|op| named("ops", op)),
        CheckKind::DocRange { min, max, .. } => bounds("min", "max", *min, *max, true),
        CheckKind::DocChange { change, at_least, relative, .. } => movement(Some(*change), *at_least, *relative),
        CheckKind::RenderDiff { expect, .. } => render_expect(expect),
        _ => Ok(()),
    }
}

/// Checks a binding's own fields, before it resolves.
///
/// # Errors
/// An `error` verdict naming the field that is malformed.
pub(super) fn bind(bind: &Bind) -> Result<(), Verdict> {
    named("bind op", &bind.op)?;
    well_formed(&bind.at)
}

fn named(field: &str, name: &str) -> Result<(), Verdict> {
    if name.trim().is_empty() { Err(invalid(format!("`{field}` names an empty op"))) } else { Ok(()) }
}

fn finite(field: &str, value: f64) -> Result<(), Verdict> {
    if value.is_finite() { Ok(()) } else { Err(invalid(format!("`{field}` must be a finite number, found {value}"))) }
}

/// The bounds of a range, each finite, `min <= max`. `one_at_least` asks that a bound is given at all.
fn bounds(
    min_name: &str,
    max_name: &str,
    min: Option<f64>,
    max: Option<f64>,
    one_at_least: bool,
) -> Result<(), Verdict> {
    if one_at_least && min.is_none() && max.is_none() {
        return Err(invalid(format!("gives neither `{min_name}` nor `{max_name}`")));
    }
    if let Some(min) = min {
        finite(min_name, min)?;
    }
    if let Some(max) = max {
        finite(max_name, max)?;
    }
    if let (Some(min), Some(max)) = (min, max)
        && min > max
    {
        return Err(invalid(format!("`{min_name}` {min} is above `{max_name}` {max}: nothing can lie between")));
    }
    Ok(())
}

/// `change` with the modifiers that only mean something beside it.
fn movement(change: Option<Change>, at_least: Option<f64>, relative: bool) -> Result<(), Verdict> {
    let Some(change) = change else {
        if at_least.is_some() {
            return Err(invalid("`at_least` modifies `change`, which is not given"));
        }
        if relative {
            return Err(invalid("`relative` modifies `change`, which is not given"));
        }
        return Ok(());
    };
    if change == Change::None && (at_least.is_some() || relative) {
        return Err(invalid("`change = \"none\"` takes neither `at_least` nor `relative`"));
    }
    if let Some(least) = at_least {
        finite("at_least", least)?;
        if least < 0.0 {
            return Err(invalid(format!("`at_least` must be 0 or more, found {least}")));
        }
    }
    if relative && at_least.is_none() {
        return Err(invalid("`relative` needs `at_least`: it says what the size is relative to"));
    }
    Ok(())
}

fn query_expect(expect: &QueryExpect) -> Result<(), Verdict> {
    if expect.equals.is_none() && expect.range.is_none() && expect.change.is_none() {
        return Err(invalid(
            "`expect` names no condition (`equals`, `range` or `change`): a query that expects nothing passes whatever happens",
        ));
    }
    if let Some([min, max]) = expect.range {
        bounds("range[0]", "range[1]", Some(min), Some(max), false)?;
    }
    movement(expect.change, expect.at_least, expect.relative)?;
    if let Some(equals) = &expect.equals {
        let reads_numbers = expect.range.is_some() || expect.change.is_some();
        match (super::number::Num::of(equals), reads_numbers) {
            (None, true) => {
                return Err(invalid(format!(
                    "`equals` {equals} is not a number, but `range` and `change` read numbers"
                )));
            }
            (Some(n), _) => {
                if let Some([min, max]) = expect.range
                    && !super::number::in_range(n, min, max)
                {
                    return Err(invalid(format!("`equals` {equals} lies outside `range` {min}..={max}")));
                }
            }
            (None, false) => {}
        }
    }
    Ok(())
}

fn render_expect(expect: &RenderDiffExpect) -> Result<(), Verdict> {
    let fields = [
        ("changed_inside_at_least", expect.changed_inside_at_least),
        ("changed_fraction_at_least", expect.changed_fraction_at_least),
        ("changed_fraction_at_most", expect.changed_fraction_at_most),
    ];
    if fields.iter().all(|(_, value)| value.is_none()) {
        return Err(invalid(
            "`expect` gives no threshold: a render check that expects nothing passes whatever happens",
        ));
    }
    for (name, value) in fields {
        if let Some(value) = value {
            finite(name, value)?;
            if !(0.0..=1.0).contains(&value) {
                return Err(invalid(format!("`{name}` is a fraction in 0..=1, found {value}")));
            }
        }
    }
    bounds(
        "changed_fraction_at_least",
        "changed_fraction_at_most",
        expect.changed_fraction_at_least,
        expect.changed_fraction_at_most,
        false,
    )
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;

    fn query_with(expect: QueryExpect) -> CheckKind {
        CheckKind::Query { op: "q".to_owned(), args: Map::new(), at: "/x".to_owned(), expect }
    }

    /// A task file cannot carry `inf` or `NaN` to a check (the loader reads checks through JSON, which turns them into
    /// `null`), but a check built in code can, and must not reach a grader.
    #[test]
    fn numbers_that_are_not_finite_are_errors() {
        let up = |at_least: f64| QueryExpect {
            change: Some(Change::Up),
            at_least: Some(at_least),
            ..QueryExpect::default()
        };
        for bad in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN, -0.5] {
            let error = kind(&query_with(up(bad))).unwrap_err();
            assert!(error.detail.contains("at_least"), "{bad}: {}", error.detail);
        }
        for range in [[0.0, f64::INFINITY], [f64::NAN, 1.0], [f64::NEG_INFINITY, 0.0]] {
            let error = kind(&query_with(QueryExpect { range: Some(range), ..QueryExpect::default() })).unwrap_err();
            assert!(error.detail.contains("range"), "{range:?}: {}", error.detail);
        }
        let doc = CheckKind::DocRange { path: "p".to_owned(), min: Some(f64::NAN), max: None };
        assert!(kind(&doc).unwrap_err().detail.contains("min"));
        let render = RenderDiffExpect { changed_fraction_at_most: Some(f64::NAN), ..RenderDiffExpect::default() };
        let diff = CheckKind::RenderDiff {
            render: crate::eval::task::OpCall { op: "r".to_owned(), args: Map::new() },
            region: crate::eval::task::Region::Rect(vec![0.0; 4]),
            expect: render,
        };
        assert!(kind(&diff).unwrap_err().detail.contains("changed_fraction_at_most"));
        assert!(kind(&query_with(up(0.0))).is_ok());
    }
}
