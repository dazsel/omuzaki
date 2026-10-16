//! Putting an avatar's counts against a limit table, and turning the result into findings.

use omuzaki_io_api::{Finding, Report};

use super::stats::{AvatarStats, Fix, stat_def};
use super::table::{LimitTable, StatLimit};

/// Where one stat falls in its table.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum StatTier {
    /// Fits tier `n` (an index into the table's tiers) and no better.
    Within(usize),
    /// Past the last tier.
    Beyond,
    /// The platform strips this component: it does not count.
    Unsupported,
}

/// One stat's measured value and where it falls.
#[derive(Clone, Debug, PartialEq)]
pub struct StatVerdict {
    /// The stat's key.
    pub key: String,
    /// The avatar's value, in the stat's unit. Not a number (NaN) for a key the registry does not know.
    pub value: f64,
    /// Where the value falls.
    pub tier: StatTier,
}

/// An avatar against one table.
#[derive(Clone, Debug, PartialEq)]
pub struct Evaluation {
    /// The tier the worst supported stat decides; `None` is past the last tier. An avatar with no supported stat
    /// fits the first tier.
    pub tier: Option<usize>,
    /// One verdict per stat the table lists, in the table's order.
    pub stats: Vec<StatVerdict>,
}

impl Evaluation {
    /// What the platform calls this avatar: the name of its tier, or the table's `beyond` label past the last one
    /// (`Very Poor`, `refused`).
    pub fn label<'a>(&self, table: &'a LimitTable) -> &'a str {
        self.tier.and_then(|tier| table.tiers.get(tier)).map_or(table.beyond.as_str(), String::as_str)
    }
}

/// Evaluates `stats` against `table`: each listed stat gets the best tier it fits, and the worst supported stat sets
/// the avatar's tier. A stat key the stat registry does not know (a hand-built table; a parsed one cannot have it)
/// is [`StatTier::Beyond`], so an unchecked stat never passes.
pub fn evaluate(stats: &AvatarStats, table: &LimitTable) -> Evaluation {
    let verdicts: Vec<StatVerdict> = table.stats.iter().map(|limit| verdict(stats, table, limit)).collect();
    let past_last = table.tiers.len();
    let worst = verdicts
        .iter()
        .filter_map(|v| match v.tier {
            StatTier::Within(i) => Some(i),
            StatTier::Beyond => Some(past_last),
            StatTier::Unsupported => None,
        })
        .max()
        .unwrap_or(0);
    Evaluation { tier: (worst < past_last).then_some(worst), stats: verdicts }
}

fn verdict(stats: &AvatarStats, table: &LimitTable, limit: &StatLimit) -> StatVerdict {
    let Some(value) = stats.get(&limit.key, &table.texture) else {
        return StatVerdict { key: limit.key.clone(), value: f64::NAN, tier: StatTier::Beyond };
    };
    let tier = if limit.unsupported {
        StatTier::Unsupported
    } else {
        // A tier with no limit takes any value; a NaN value fits no limit and falls past the last tier.
        limit
            .per_tier
            .iter()
            .position(|cap| cap.is_none_or(|cap| value <= cap))
            .map_or(StatTier::Beyond, StatTier::Within)
    };
    StatVerdict { key: limit.key.clone(), value, tier }
}

/// The findings for an evaluation against `target_tier`: an error for each stat over that tier's limit, a warning for
/// each stat past an advisory tier but within the target, and an info for each stripped component the avatar has
/// anyway. Rule ids are `limits.<platform>.<stat>`; a hint names the fit
/// stage that would help (`fit.decimate`, `fit.atlas`, `fit.meshes`, `fit.bones`, `fit.textures`, `fit.strip`) or says
/// what to change by hand. A target past the last tier is the last tier.
pub fn findings(eval: &Evaluation, table: &LimitTable, target_tier: usize) -> Report {
    let target = target_tier.min(table.tiers.len().saturating_sub(1));
    let mut report = Report::new();
    for verdict in &eval.stats {
        let unit = table.stat(&verdict.key).map_or("", |limit| limit.unit.suffix());
        let value = format!("{}{unit}", format_number(verdict.value));
        let rule = format!("limits.{}.{}", table.platform, verdict.key);
        match verdict.tier {
            StatTier::Unsupported if verdict.value > 0.0 => {
                let note = table.unsupported_note.as_deref().unwrap_or("not supported on this platform");
                report.push(
                    Finding::info(rule, format!("{} {value}: {note}", verdict.key)).at(&verdict.key).hint("fit.strip"),
                );
            }
            StatTier::Within(i) if i > target => report.push(over(verdict, table, target, &value, rule)),
            StatTier::Beyond => report.push(over(verdict, table, target, &value, rule)),
            StatTier::Within(i) => {
                if let Some(advisory) = past_advisory(table, i) {
                    report.push(past(verdict, table, advisory, &value, rule));
                }
            }
            StatTier::Unsupported => {}
        }
    }
    report
}

/// The last advisory tier below tier `i`, which a stat in tier `i` is past, if the table has one.
fn past_advisory(table: &LimitTable, i: usize) -> Option<usize> {
    (0..i).rev().find(|&tier| table.tiers.get(tier).is_some_and(|name| table.advisory.contains(name)))
}

/// A warning for a stat past an advisory tier, which is a recommendation and not a limit.
fn past(verdict: &StatVerdict, table: &LimitTable, advisory: usize, value: &str, rule: String) -> Finding {
    let tier = table.tiers.get(advisory).map_or("the advisory tier", String::as_str);
    let cap = table.stat(&verdict.key).and_then(|limit| limit.per_tier.get(advisory).copied().flatten());
    let message = match (cap, table.stat(&verdict.key)) {
        (Some(cap), Some(limit)) => {
            format!("{} {value}, past the {tier} size ({}{})", verdict.key, format_number(cap), limit.unit.suffix())
        }
        _ => format!("{} {value}, past the {tier} size", verdict.key),
    };
    hinted(Finding::warning(rule, message).at(&verdict.key), &verdict.key)
}

fn over(verdict: &StatVerdict, table: &LimitTable, target: usize, value: &str, rule: String) -> Finding {
    let tier = table.tiers.get(target).map_or("the target tier", String::as_str);
    let cap = table.stat(&verdict.key).and_then(|limit| limit.per_tier.get(target).copied().flatten());
    let message = match (cap, table.stat(&verdict.key)) {
        (Some(cap), Some(limit)) => {
            format!("{} {value}, over {tier} ({}{})", verdict.key, format_number(cap), limit.unit.suffix())
        }
        _ => format!("{} {value}, over {tier}", verdict.key),
    };
    hinted(Finding::error(rule, message).at(&verdict.key), &verdict.key)
}

/// The finding with the hint of the stat's fix, if the registry knows the stat.
fn hinted(finding: Finding, key: &str) -> Finding {
    match stat_def(key).map(|def| def.fix) {
        Some(Fix::Stage(hint) | Fix::Hand(hint)) => finding.hint(hint),
        None => finding,
    }
}

/// A value for a message: whole numbers with thousands separators (`120,000`), others to two decimals without
/// trailing zeros (`2.6`).
fn format_number(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    if value.fract() == 0.0 && value.abs() < 1e15 {
        // The guard above keeps the value exactly representable as an integer.
        #[allow(clippy::cast_possible_truncation)]
        let whole = value as i64;
        let digits = whole.unsigned_abs().to_string();
        let mut grouped = String::new();
        for (i, digit) in digits.chars().enumerate() {
            if i > 0 && (digits.len() - i).is_multiple_of(3) {
                grouped.push(',');
            }
            grouped.push(digit);
        }
        return if whole < 0 { format!("-{grouped}") } else { grouped };
    }
    let text = format!("{value:.2}");
    text.trim_end_matches('0').trim_end_matches('.').to_string()
}
