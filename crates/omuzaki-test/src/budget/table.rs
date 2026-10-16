//! The budget table, parsed from spec §10 so the numbers are never copied by hand.

use super::error::BudgetError;

/// Heading that opens the section holding the table.
const SECTION: &str = "## 10.";
/// First cell of the table's header row.
const HEADER_KEY: &str = "| Key |";
/// Cells per row: key, item, budget, tier, statistic, gate.
const COLUMNS: usize = 6;
/// The one row whose megabytes are decimal (§10, "Units"); everywhere else MB means MiB.
const DECIMAL_MB_KEY: &str = "installer.size";

/// What a budget limit is measured in. Times are canonicalised to milliseconds, sizes to bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// Milliseconds.
    Ms,
    /// Percent of the reference CPU basis.
    Percent,
    /// Bytes.
    Bytes,
    /// A plain count (frames).
    Count,
}

/// Which of a metric's statistics a budget judges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Statistic {
    /// The 95th percentile.
    P95,
    /// The mean.
    Mean,
    /// The maximum, also for "worst of N", counts and exact sizes.
    Max,
    /// A statistic the harness does not compute (for example "p95 over repainted frames"); never compared.
    Other,
}

impl Statistic {
    /// The statistic is the cell's first word: qualifiers after it ("p95 over repainted frames", "mean over 5 min",
    /// "worst of 5 launches") say over what, not which.
    fn parse(text: &str) -> Self {
        let first = text.split_whitespace().next().unwrap_or_default().to_ascii_lowercase();
        match first.as_str() {
            "p95" => Self::P95,
            "mean" => Self::Mean,
            "max" | "worst" | "count" | "exact" => Self::Max,
            _ => Self::Other,
        }
    }
}

/// A budget limit as `≤ value unit`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limit {
    /// The largest passing value, in `unit`.
    pub value: f64,
    /// Its unit.
    pub unit: Unit,
}

/// One row of the §10 table.
#[derive(Debug, Clone, PartialEq)]
pub struct Budget {
    /// The row key, for example `physics.step`.
    pub key: String,
    /// The budget cell as written.
    pub budget_text: String,
    /// The tier cell as written.
    pub tier: String,
    /// The statistic the budget judges.
    pub statistic: Statistic,
    /// The parsed limit, `None` for rows that are not a single `≤ number unit` (for example two limits in one cell).
    pub limit: Option<Limit>,
}

/// Parse the §10 budget table out of the spec's text.
///
/// # Errors
/// [`BudgetError::Spec`] when the section, the table or a row is missing or malformed, or a key repeats.
pub fn parse_spec(text: &str) -> Result<Vec<Budget>, BudgetError> {
    let mut lines = text.lines().skip_while(|l| !l.starts_with(SECTION));
    if lines.next().is_none() {
        return Err(spec_err(format!("no `{SECTION}` section")));
    }
    let mut section = lines.take_while(|l| !l.starts_with("## "));
    if !section.any(|l| l.trim_start().starts_with(HEADER_KEY)) {
        return Err(spec_err(format!("no table with a `{HEADER_KEY}` header in the section")));
    }
    let mut budgets: Vec<Budget> = Vec::new();
    for row in section.skip(1).take_while(|l| l.trim_start().starts_with('|')) {
        let cells: Vec<&str> = row.trim().trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() != COLUMNS {
            return Err(spec_err(format!("row has {} cells, expected {COLUMNS}: {row}", cells.len())));
        }
        let key = cells[0].trim_matches('`').to_owned();
        if budgets.iter().any(|b| b.key == key) {
            return Err(spec_err(format!("key `{key}` appears twice")));
        }
        budgets.push(Budget {
            limit: parse_limit(&key, cells[2]),
            budget_text: cells[2].to_owned(),
            tier: cells[3].to_owned(),
            statistic: Statistic::parse(cells[4]),
            key,
        });
    }
    if budgets.is_empty() {
        return Err(spec_err("the table has no rows".to_owned()));
    }
    Ok(budgets)
}

fn spec_err(msg: String) -> BudgetError {
    BudgetError::Spec(msg)
}

fn parse_limit(key: &str, cell: &str) -> Option<Limit> {
    let rest = cell.strip_prefix('≤')?;
    let mut parts = rest.split_whitespace();
    let (num, unit, extra) = (parts.next()?, parts.next()?, parts.next());
    if extra.is_some() {
        return None;
    }
    let value: f64 = num.parse().ok()?;
    let mb = if key == DECIMAL_MB_KEY { 1e6 } else { 1024.0 * 1024.0 };
    let (factor, unit) = match unit {
        "ms" => (1.0, Unit::Ms),
        "s" => (1000.0, Unit::Ms),
        "%" => (1.0, Unit::Percent),
        "MB" => (mb, Unit::Bytes),
        "GB" => (1024.0 * 1024.0 * 1024.0, Unit::Bytes),
        "frames" => (1.0, Unit::Count),
        _ => return None,
    };
    Some(Limit { value: value * factor, unit })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "## 9. Crates\n| Key |\n## 10. Platforms\ntext\n\n| Key | Item | Budget | Tier | Statistic | Gate |\n|---|---|---|---|---|---|\n\
| `gpu.frame` | GPU | ≤ 1.5 ms | `R` | p95 | g |\n| `startup.live` | s | ≤ 2 s | `R` | worst of 5 launches | g |\n\
| `installer.size` | i | ≤ 150 MB | any | exact | g |\n| `ram.editor` | r | ≤ 1.5 GB | `R` | max over 5 min | g |\n\
| `asr.live` | a | CPU ≤ 1.5 %, GPU ≤ 1 ms | `R` | mean over 5 min | g |\n\n## 11. MCP\n| `late` | x | ≤ 1 ms | R | p95 | g |\n";

    #[test]
    fn parses_rows_and_converts_units() {
        let b = parse_spec(SPEC).unwrap();
        let keys: Vec<_> = b.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, ["gpu.frame", "startup.live", "installer.size", "ram.editor", "asr.live"]);
        assert_eq!(b[0].limit, Some(Limit { value: 1.5, unit: Unit::Ms }));
        assert_eq!(b[0].statistic, Statistic::P95);
        assert_eq!(b[1].limit, Some(Limit { value: 2000.0, unit: Unit::Ms }));
        assert_eq!(b[1].statistic, Statistic::Max);
        assert_eq!(b[2].limit, Some(Limit { value: 150e6, unit: Unit::Bytes }));
        assert_eq!(b[3].limit, Some(Limit { value: 1.5 * 1024.0 * 1024.0 * 1024.0, unit: Unit::Bytes }));
        assert_eq!(b[4].limit, None);
    }

    #[test]
    fn missing_or_broken_tables_fail_closed() {
        assert!(parse_spec("## 11. nothing").is_err());
        assert!(parse_spec("## 10. x\nno table\n").is_err());
        assert!(parse_spec("## 10. x\n| Key | a |\n|---|---|\n| `k` | 1 |\n").is_err());
        let dup = "## 10. x\n| Key | Item | Budget | Tier | Statistic | Gate |\n|-|-|-|-|-|-|\n| `a` | i | ≤ 1 ms | R | p95 | g |\n| `a` | i | ≤ 1 ms | R | p95 | g |\n";
        assert!(parse_spec(dup).is_err());
    }

    #[test]
    fn the_real_spec_parses() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/product/02-studio-spec-v3.md");
        let text = std::fs::read_to_string(path).unwrap();
        let b = parse_spec(&text).unwrap();
        let find = |k: &str| b.iter().find(|b| b.key == k).unwrap();
        assert_eq!(find("physics.step").limit, Some(Limit { value: 0.5, unit: Unit::Ms }));
        assert_eq!(find("cpu.total").statistic, Statistic::Mean);
        assert!(b.len() >= 19);
        // Row-kind sweep: every statistic cell and every limit cell of the real spec is understood, except the one row
        // with two limits in its cell.
        let other: Vec<_> = b.iter().filter(|b| b.statistic == Statistic::Other).map(|b| &b.key).collect();
        assert!(other.is_empty(), "statistics the harness does not read: {other:?}");
        let unparsed: Vec<_> = b.iter().filter(|b| b.limit.is_none()).map(|b| b.key.as_str()).collect();
        assert_eq!(unparsed, ["asr.live"]);
        assert_eq!(find("gpu.face_paint").statistic, Statistic::P95);
    }
}
