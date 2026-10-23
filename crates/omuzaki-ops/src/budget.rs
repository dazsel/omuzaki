//! Session budgets (design `agent-sessions.md` §8.1): money in integer micro-dollars, LLM tokens and active time.
//!
//! Money is charged at apply from the plan's `cost.usd`; tokens come from whoever pays for the model
//! (`Sessions::charge`); time is the runner's clock since the session opened, minus the time it was paused. Every sum
//! is an integer, so a budget decides the same way on every machine.

use omuzaki_ops_api::{Budget, CostHint, OpError, Spend};

/// Micro-dollars in a dollar.
const MICROS: u64 = 1_000_000;

/// What a plan's `cost.usd` comes to in micro-dollars, rounded up. Read through the cost's shortest decimal form (the
/// one `Display` prints), so the `0.1` an op wrote is exactly 100,000 and not the 100,000.0015 its `f32` holds: sums
/// of costs are exact. `None`, or no `usd`, is free.
///
/// # Errors
/// `Internal`, naming `op`, for a NaN, infinite or negative cost: the op has a bug, and charging nothing (or
/// everything) for it would hide that.
pub(crate) fn usd_micros(op: &str, cost: Option<&CostHint>) -> Result<u64, OpError> {
    let Some(usd) = cost.and_then(|c| c.usd) else { return Ok(0) };
    if !usd.is_finite() || usd < 0.0 {
        return Err(OpError::internal(format!("{op} planned a cost of {usd} dollars"))
            .hint("this is a bug in the op: a cost is a finite, non-negative number of dollars"));
    }
    Ok(decimal_micros(&usd.to_string()))
}

/// `text`, a non-negative decimal without an exponent (what `f32`'s `Display` prints), in micro-dollars rounded up.
/// Saturates at `u64::MAX`, which is past every budget.
fn decimal_micros(text: &str) -> u64 {
    let (whole, fraction) = text.split_once('.').unwrap_or((text, ""));
    let dollars = whole.bytes().fold(0u64, |sum, digit| sum.saturating_mul(10).saturating_add(digit_value(digit)));
    let mut micros = 0u64;
    let mut digits = fraction.bytes();
    for digit in digits.by_ref().take(6) {
        micros = micros * 10 + digit_value(digit);
    }
    let places = fraction.len().min(6);
    micros *= 10u64.pow(6 - u32::try_from(places).unwrap_or(6));
    // Anything past the sixth place rounds up to the next micro-dollar.
    let rest = u64::from(digits.any(|digit| digit != b'0'));
    dollars.saturating_mul(MICROS).saturating_add(micros).saturating_add(rest)
}

fn digit_value(digit: u8) -> u64 {
    u64::from(digit.wrapping_sub(b'0').min(9))
}

/// What a session's budget stands at: whether `cost` more micro-dollars may be spent now, given `spent` (with the
/// active time already counted in). `None` when everything fits; otherwise what the request needs and what is left,
/// for [`Need::Budget`](omuzaki_ops_api::Need::Budget). Tokens and time past their caps block even a free request.
pub(crate) fn exceeded(budget: &Budget, spent: &Spend, cost: u64) -> Option<(Spend, Spend)> {
    let money = spent.usd_micros.saturating_add(cost) > budget.usd_micros;
    let tokens = budget.tokens.is_some_and(|cap| spent.tokens > cap);
    let seconds = budget.seconds.is_some_and(|cap| spent.seconds > cap);
    (money || tokens || seconds).then(|| (Spend { usd_micros: cost, ..Spend::default() }, left(budget, spent)))
}

/// Whether `spent` is past any cap of `budget`: what a charge from outside checks after adding.
pub(crate) fn crossed(budget: &Budget, spent: &Spend) -> bool {
    exceeded(budget, spent, 0).is_some()
}

/// What `budget` still allows after `spent`. A field with no cap is `u64::MAX`.
pub(crate) fn left(budget: &Budget, spent: &Spend) -> Spend {
    let remaining = |cap: Option<u64>, used: u64| cap.map_or(u64::MAX, |cap| cap.saturating_sub(used));
    Spend {
        usd_micros: budget.usd_micros.saturating_sub(spent.usd_micros),
        tokens: remaining(budget.tokens, spent.tokens),
        seconds: remaining(budget.seconds, spent.seconds),
    }
}

/// Spells `micros` as dollars for messages: `$0.40`, `$0.000001`.
pub(crate) fn dollars(micros: u64) -> String {
    let (whole, fraction) = (micros / MICROS, micros % MICROS);
    let fraction = format!("{fraction:06}");
    let trimmed = fraction.trim_end_matches('0');
    let shown = if trimmed.len() < 2 { &fraction[..2] } else { trimmed };
    format!("${whole}.{shown}")
}

#[cfg(test)]
mod tests {
    use omuzaki_ops_api::ErrorCode;

    use super::*;

    fn cost(usd: f32) -> CostHint {
        CostHint { usd: Some(usd), ..CostHint::default() }
    }

    #[test]
    fn costs_convert_exactly_and_round_up() {
        let micros = |usd: f32| usd_micros("test.paid", Some(&cost(usd))).unwrap();
        assert_eq!(micros(0.1) + micros(0.2), 300_000);
        assert_eq!(micros(0.4) * 2, 800_000);
        assert_eq!(micros(0.12), 120_000);
        assert_eq!(micros(0.000_000_1), 1, "a fraction of a micro-dollar rounds up");
        assert_eq!(micros(0.0), 0);
        assert_eq!(micros(12.5), 12_500_000);
        assert_eq!(micros(f32::MAX), u64::MAX, "saturates past every budget");
        assert_eq!(usd_micros("test.free", None).unwrap(), 0);
        for bad in [f32::NAN, f32::INFINITY, -0.5] {
            let e = usd_micros("test.paid", Some(&cost(bad))).unwrap_err();
            assert!(e.code == ErrorCode::Internal && e.message.contains("test.paid"), "{e:?}");
        }
    }

    #[test]
    fn a_budget_blocks_what_does_not_fit_and_time_or_tokens_past_their_caps() {
        let budget = Budget { usd_micros: 1_000_000, tokens: Some(100), seconds: Some(60) };
        let spent = Spend { usd_micros: 800_000, tokens: 100, seconds: 60 };
        assert_eq!(exceeded(&budget, &spent, 200_000), None, "at the cap is not past it");
        let (needed, remaining) = exceeded(&budget, &spent, 400_000).unwrap();
        assert_eq!((needed.usd_micros, remaining.usd_micros), (400_000, 200_000));
        assert!(exceeded(&budget, &Spend { tokens: 101, ..spent }, 0).is_some());
        assert!(exceeded(&budget, &Spend { seconds: 61, ..spent }, 0).is_some());
        assert_eq!(left(&Budget::default(), &spent).tokens, u64::MAX, "no cap");
    }

    #[test]
    fn dollars_read_naturally() {
        assert_eq!(dollars(400_000), "$0.40");
        assert_eq!(dollars(2_000_000), "$2.00");
        assert_eq!(dollars(1), "$0.000001");
        assert_eq!(dollars(120_500), "$0.1205");
    }
}
