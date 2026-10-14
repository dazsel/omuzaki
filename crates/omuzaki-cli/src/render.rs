//! Human-readable text for the CLI: the op table, an op's help page and an outcome with a readable diff.
//!
//! Pure functions from registry data to strings, so every output is snapshot-testable. Machine output (`--json`)
//! lives in `commands`; both are built from the same [`OpInfo`] and [`Outcome`] values.

use std::fmt::Write as _;

use omuzaki_doc_api::{Change, Value};
use omuzaki_ops_api::{CostHint, Mode, OpInfo, Outcome};
use serde::Serialize;

use crate::json;
use crate::schema_doc::{self, ArgDoc};

/// An enum's name as it is spelled in JSON (`edit`, `auto`), so the text and `--json` agree and nothing here has
/// to be updated when a variant is added.
pub fn serde_name<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => name,
        Ok(other) => other.to_string(),
        Err(_) => "?".to_owned(),
    }
}

/// `omuzaki ops`: one row per op.
pub fn ops_table(ops: &[OpInfo]) -> String {
    if ops.is_empty() {
        return "No ops are registered in this build.\n".to_owned();
    }
    let rows: Vec<[String; 4]> = ops
        .iter()
        .map(|op| [op.name.to_owned(), serde_name(&op.kind), op.scope.to_string(), op.summary.to_owned()])
        .collect();
    let mut text = table(&["NAME", "KIND", "SCOPE", "SUMMARY"], &rows);
    let _ = writeln!(text, "\n{} ops. `omuzaki help <op>` shows an op's arguments.", ops.len());
    text
}

/// Left-aligned columns; the last column is not padded, so lines never end in spaces.
fn table<const N: usize>(header: &[&str; N], rows: &[[String; N]]) -> String {
    let mut widths: [usize; N] = std::array::from_fn(|i| header[i].len());
    for row in rows {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let line = |cells: &[&str]| {
        let mut out = String::new();
        for (i, cell) in cells.iter().enumerate() {
            if i + 1 == N {
                out.push_str(cell);
            } else {
                let _ = write!(out, "{cell:<width$}  ", width = widths[i]);
            }
        }
        out.push('\n');
        out
    };
    let mut text = line(header);
    for row in rows {
        text.push_str(&line(&row.each_ref().map(String::as_str)));
    }
    text
}

/// `omuzaki help <op>`: facts about the op, then its arguments, all read from the registry entry and its schema.
pub fn op_help(info: &OpInfo) -> String {
    let mut text = format!("{}\n  {}\n", info.name, info.summary);
    if let Some(long) = schema_doc::description(&info.schema).filter(|d| d != info.summary) {
        text.push('\n');
        text.push_str(&indent(&long, "  "));
    }
    let _ = write!(
        text,
        "\n  kind   {}\n  scope  {}\n  undo   {}\n\nArguments:\n",
        serde_name(&info.kind),
        info.scope,
        serde_name(&info.undo)
    );
    let args = schema_doc::arguments(&info.schema);
    if args.is_empty() {
        text.push_str("  none\n");
    } else {
        text.push_str(&arguments_block(&args));
    }
    let _ = write!(
        text,
        "\nRun it:\n  omuzaki run {name} --args '<json>'\n  omuzaki run {name} --args '<json>' --dry-run\n\n\
         The full JSON Schema: omuzaki help {name} --json\n",
        name = info.name
    );
    text
}

fn arguments_block(args: &[ArgDoc]) -> String {
    let name_width = args.iter().map(|a| a.name.chars().count()).max().unwrap_or(0);
    let type_width = args.iter().map(|a| a.ty.chars().count()).max().unwrap_or(0);
    let mut text = String::new();
    for arg in args {
        let need = if arg.required { "required" } else { "optional" };
        let _ = writeln!(text, "  {:<name_width$}  {:<type_width$}  {need}", arg.name, arg.ty);
        if let Some(description) = &arg.description {
            let _ = writeln!(text, "      {description}");
        }
        for constraint in &arg.constraints {
            let _ = writeln!(text, "      {constraint}");
        }
        if !arg.choices.is_empty() {
            text.push_str("      one of:\n");
            let width = arg.choices.iter().map(|c| c.value.chars().count()).max().unwrap_or(0);
            for choice in &arg.choices {
                let what = choice.description.as_deref().unwrap_or("");
                let line = format!("{:<width$}  {what}", choice.value);
                let _ = writeln!(text, "        {}", line.trim_end());
            }
        }
    }
    text
}

/// `omuzaki run` without `--json`: what happened, then the patch as a diff, the op's answer and its cost.
pub fn outcome(op: &str, mode: Mode, outcome: &Outcome) -> String {
    let mut text = match (mode, &outcome.applied) {
        (Mode::DryRun, _) => format!("dry run of {op}: nothing was changed\n"),
        (Mode::Apply, Some(record)) => {
            format!("applied {op} (step {}, document version {})\n", record.seq, record.version.0)
        }
        (Mode::Apply, None) => format!("ran {op}\n"),
    };
    let changes: Vec<&Change> = outcome.plan.patch.changes.iter().filter(|c| c.before != c.after).collect();
    if changes.is_empty() {
        if outcome.plan.output.is_none() {
            text.push_str("no changes\n");
        }
    } else {
        let _ = writeln!(text, "\nchanges ({}):", changes.len());
        for change in changes {
            text.push_str(&change_diff(change));
        }
    }
    if let Some(output) = &outcome.plan.output {
        text.push_str("\noutput:\n");
        text.push_str(&indent(&json::render_value(output, true), "  "));
    }
    if let Some(cost) = outcome.plan.cost.as_ref().and_then(cost_summary) {
        let _ = write!(text, "\ncost: {cost}\n");
    }
    text
}

/// One change as a small diff: `+` creates, `-` removes, `~` replaces.
fn change_diff(change: &Change) -> String {
    let marker = match (&change.before, &change.after) {
        (None, _) => '+',
        (_, None) => '-',
        _ => '~',
    };
    let mut text = format!("  {marker} {}\n", change.path);
    if let Some(before) = &change.before {
        text.push_str(&indent(&json::render_value(before, true), "      - "));
    }
    if let Some(after) = &change.after {
        text.push_str(&indent(&json::render_value(after, true), "      + "));
    }
    text
}

fn cost_summary(cost: &CostHint) -> Option<String> {
    let parts: Vec<String> = [
        cost.seconds.map(|s| format!("about {s} s")),
        cost.gpu_mib.map(|m| format!("{m} MiB GPU memory")),
        cost.usd.map(|u| format!("${u:.2}")),
    ]
    .into_iter()
    .flatten()
    .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// Prefixes every non-blank line, and ends the block with a newline.
fn indent(block: &str, prefix: &str) -> String {
    block.lines().fold(String::new(), |mut out, line| {
        if !line.is_empty() {
            out.push_str(prefix);
            out.push_str(line);
        }
        out.push('\n');
        out
    })
}
