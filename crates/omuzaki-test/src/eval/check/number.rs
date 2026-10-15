//! Numbers, the one place the checks compare, order and measure them.
//!
//! A JSON number is an exact integer or a float. Two integral operands (a JSON integer, or a float with no fraction
//! small enough to lose nothing) are compared and subtracted as integers, so `9007199254740992` to `9007199254740993`
//! is a move up of exactly one; `f64` is for the operands that are not integral. `1` and `1.0` are the same number.
//! A relative change from an exact zero is infinitely large, and from anything else is `|after - before| / |before|`,
//! however small the baseline.

use std::cmp::Ordering;

use serde_json::Value;

use crate::eval::task::Change;

/// Floats whole and smaller than this count as integers: far past `u64`, far inside `i128`, so sums never overflow.
const INTEGER_LIMIT: f64 = 1.0e30;

/// A number as the checks read it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Num {
    /// An integral number, exactly.
    Int(i128),
    /// Any other: a fraction, or a float too large to be an integer here.
    Float(f64),
}

impl Num {
    /// The number a JSON value holds, if it holds one.
    pub(super) fn of(value: &Value) -> Option<Self> {
        let Value::Number(n) = value else { return None };
        if let Some(i) = n.as_i64() {
            return Some(Self::Int(i128::from(i)));
        }
        if let Some(u) = n.as_u64() {
            return Some(Self::Int(i128::from(u)));
        }
        n.as_f64().map(Self::float)
    }

    /// A float as a number: an integer when it is whole and small enough, else itself.
    #[allow(clippy::cast_possible_truncation)] // the guard keeps the float whole and far inside `i128`
    pub(super) fn float(f: f64) -> Self {
        if f.is_finite() && f.fract() == 0.0 && f.abs() < INTEGER_LIMIT { Self::Int(f as i128) } else { Self::Float(f) }
    }

    #[allow(clippy::cast_precision_loss)] // only for the operands that are not both integers
    fn approx(self) -> f64 {
        match self {
            Self::Int(i) => i as f64,
            Self::Float(f) => f,
        }
    }

    fn is_zero(self) -> bool {
        match self {
            Self::Int(i) => i == 0,
            Self::Float(f) => f == 0.0,
        }
    }

    /// How this number orders against another, exactly; `None` when either is not a number (`NaN`).
    pub(super) fn order(self, other: Self) -> Option<Ordering> {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => Some(a.cmp(&b)),
            (Self::Float(a), Self::Float(b)) => a.partial_cmp(&b),
            (Self::Int(i), Self::Float(f)) => order_int_float(i, f),
            (Self::Float(f), Self::Int(i)) => order_int_float(i, f).map(Ordering::reverse),
        }
    }

    /// Whether the two are the same number.
    pub(super) fn equals(self, other: Self) -> bool {
        self.order(other) == Some(Ordering::Equal)
    }

    /// The distance between two numbers: exact when both are integers.
    fn distance(self, other: Self) -> Self {
        if let (Self::Int(a), Self::Int(b)) = (self, other)
            && let Some(d) = a.checked_sub(b)
        {
            return Self::Int(d.abs());
        }
        Self::Float((self.approx() - other.approx()).abs())
    }
}

/// An integer against a float, exactly: the float's whole part decides, and its fraction breaks a tie.
#[allow(clippy::cast_possible_truncation)] // the float is inside the integer limit when it is floored
fn order_int_float(i: i128, f: f64) -> Option<Ordering> {
    if f.is_nan() {
        return None;
    }
    if f >= INTEGER_LIMIT {
        return Some(Ordering::Less);
    }
    if f <= -INTEGER_LIMIT {
        return Some(Ordering::Greater);
    }
    let whole = f.floor();
    Some(match i.cmp(&(whole as i128)) {
        Ordering::Equal if f.fract() != 0.0 => Ordering::Less,
        other => other,
    })
}

/// Whether two JSON values are equal: numbers as numbers (`1` is `1.0`, and integers are exact), anything else as
/// JSON.
pub(super) fn same_value(left: &Value, right: &Value) -> bool {
    match (Num::of(left), Num::of(right)) {
        (Some(l), Some(r)) => l.equals(r),
        _ => left == right,
    }
}

/// Whether `n` lies in `min..=max`.
pub(super) fn in_range(n: Num, min: f64, max: f64) -> bool {
    matches!(n.order(Num::float(min)), Some(Ordering::Greater | Ordering::Equal))
        && matches!(n.order(Num::float(max)), Some(Ordering::Less | Ordering::Equal))
}

/// Whether a number moved from `before` to `after` the way `change` says, by at least `at_least` (a fraction of
/// `before` when `relative`).
pub(super) fn moved(before: Num, after: Num, change: Change, at_least: Option<f64>, relative: bool) -> bool {
    let Some(order) = after.order(before) else { return false };
    let direction = match change {
        Change::Up => order == Ordering::Greater,
        Change::Down => order == Ordering::Less,
        Change::Any => order != Ordering::Equal,
        Change::None => return order == Ordering::Equal,
    };
    direction && at_least.is_none_or(|least| big_enough(before, after, least, relative))
}

/// Whether the move is at least `least`, in absolute terms or as a fraction of `before`.
fn big_enough(before: Num, after: Num, least: f64, relative: bool) -> bool {
    let distance = before.distance(after);
    if !relative {
        return matches!(distance.order(Num::float(least)), Some(Ordering::Greater | Ordering::Equal));
    }
    // Only an exact zero baseline makes a relative move infinite.
    before.is_zero() || distance.approx() / before.approx().abs() >= least
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn n(text: &str) -> Num {
        Num::of(&serde_json::from_str::<Value>(text).unwrap()).unwrap()
    }

    #[test]
    fn equality_is_exact_for_integers_and_still_reads_one_as_one_point_zero() {
        let v = |text: &str| serde_json::from_str::<Value>(text).unwrap();
        assert!(same_value(&v("1"), &v("1.0")));
        assert!(same_value(&v("9007199254740993"), &v("9007199254740993")));
        assert!(!same_value(&v("9007199254740992"), &v("9007199254740993")));
        assert!(!same_value(&v("9007199254740993"), &v("9007199254740992")));
        assert!(!same_value(&v("9007199254740991"), &v("9007199254740992")));
        assert!(!same_value(&v("9007199254740993"), &v("9007199254740992.0")));
        assert!(same_value(&v("9007199254740992"), &v("9007199254740992.0")));
        assert!(!same_value(&v("18446744073709551615"), &v("18446744073709551614")));
        assert!(!same_value(&v("-9223372036854775808"), &v("-9223372036854775807")));
        assert!(!same_value(&v("1"), &v("1.0000000000000002")));
        assert!(!same_value(&v("0.1"), &v("0.10000000000000002")));
        assert!(same_value(&v("0.5"), &v("0.5")));
        assert!(!same_value(&v("1"), &json!("1")));
    }

    #[test]
    fn integers_order_exactly_against_integers_and_floats() {
        assert_eq!(n("9007199254740993").order(n("9007199254740992")), Some(Ordering::Greater));
        assert_eq!(n("2251799813685249").order(n("2251799813685248.5")), Some(Ordering::Greater));
        assert_eq!(n("2251799813685248").order(n("2251799813685248.5")), Some(Ordering::Less));
        assert_eq!(n("9007199254740993").order(n("9007199254740992.0")), Some(Ordering::Greater));
        assert_eq!(n("-3").order(n("-2.5")), Some(Ordering::Less));
        assert_eq!(n("2").order(n("1e300")), Some(Ordering::Less));
        assert_eq!(n("1e300").order(n("2")), Some(Ordering::Greater));
    }

    #[test]
    fn integer_moves_are_measured_exactly() {
        let big = n("9007199254740992");
        let next = n("9007199254740993");
        assert!(moved(big, next, Change::Up, None, false));
        assert!(!moved(big, next, Change::None, None, false));
        assert!(moved(next, big, Change::Down, Some(1.0), false));
        assert!(!moved(big, next, Change::Up, Some(2.0), false));
        assert!(moved(big, next, Change::Any, Some(1.0), false));
        assert!(moved(big, next, Change::Up, Some(1.0e-16), true));
        assert!(!moved(big, next, Change::Up, Some(1.0e-15), true));
        assert!(moved(big, big, Change::None, None, false));
    }

    #[test]
    fn movement_honours_direction_size_and_relative_size() {
        assert!(moved(n("1"), n("2"), Change::Up, Some(1.0), false));
        assert!(!moved(n("1"), n("1.5"), Change::Up, Some(1.0), false));
        assert!(moved(n("10"), n("5"), Change::Down, Some(0.5), true));
        assert!(!moved(n("10"), n("9"), Change::Down, Some(0.5), true));
        assert!(moved(n("3"), n("3"), Change::None, None, false));
        assert!(!moved(n("3"), n("3.1"), Change::None, None, false));
    }

    #[test]
    fn only_an_exact_zero_baseline_makes_a_relative_move_infinite() {
        assert!(moved(n("0"), n("0.1"), Change::Any, Some(100.0), true));
        assert!(moved(n("0.0"), n("0.1"), Change::Up, Some(100.0), true));
        // 1e-20 to 1.1e-20 is a 10% move, however small the baseline.
        assert!(!moved(n("1e-20"), n("1.1e-20"), Change::Up, Some(1.0), true));
        assert!(moved(n("1e-20"), n("1.1e-20"), Change::Up, Some(0.09), true));
        assert!(!moved(n("1e-20"), n("1.1e-20"), Change::Up, Some(0.11), true));
        assert!(moved(n("-1e-20"), n("-1.1e-20"), Change::Down, Some(0.09), true));
    }

    #[test]
    fn ranges_include_both_ends_and_compare_exactly() {
        assert!(in_range(n("5"), 4.0, 6.0));
        assert!(in_range(n("4"), 4.0, 6.0));
        assert!(in_range(n("6"), 4.0, 6.0));
        assert!(!in_range(n("6.000000000000001"), 4.0, 6.0));
        assert!(!in_range(n("9007199254740993"), 0.0, 9_007_199_254_740_992.0));
        assert!(!in_range(n("5"), f64::NAN, 6.0));
    }
}
