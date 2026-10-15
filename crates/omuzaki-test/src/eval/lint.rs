//! `omuzaki-test eval lint` (design 3.3, 11.2): every rule, as a list of findings rather than a first error.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use serde_json::Value;

use super::provenance::{PrivateList, fixture_refusals, refusals};
use super::{Discovery, TaskFile};

/// The shortest run of shared words that counts as a prompt appearing in a tool description.
pub const LEAK_WORDS: usize = 8;

/// One thing wrong, and the file it is wrong in.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    /// The file, relative to the workspace root.
    pub file: PathBuf,
    /// What is wrong.
    pub message: String,
}

fn finding(file: &std::path::Path, message: impl Into<String>) -> Finding {
    Finding { file: file.to_owned(), message: message.into() }
}

/// A piece of text an agent reads before it works: a tool description or the server instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolText {
    /// Where it comes from, such as `tool face_set_profile` or `server instructions`.
    pub source: String,
    /// The text.
    pub text: String,
}

/// Reads a `--tools` dump: an array of `{name, description}` objects, or an object with `tools` (that array) and an
/// optional `instructions` string.
///
/// # Errors
/// A message when the JSON is not one of those shapes.
pub fn tool_texts_from_json(json: &str) -> Result<Vec<ToolText>, String> {
    let value: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let (tools, instructions) = match &value {
        Value::Array(_) => (&value, None),
        Value::Object(map) => (map.get("tools").unwrap_or(&Value::Null), map.get("instructions")),
        _ => return Err("expected an array of tools or an object with `tools`".into()),
    };
    let Value::Array(tools) = tools else { return Err("expected an array of tools".into()) };
    let mut texts = Vec::new();
    if let Some(Value::String(text)) = instructions {
        texts.push(ToolText { source: "server instructions".into(), text: text.clone() });
    }
    for tool in tools {
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("?");
        let description = tool.get("description").and_then(Value::as_str).unwrap_or_default();
        texts.push(ToolText { source: format!("tool {name}"), text: description.to_owned() });
    }
    Ok(texts)
}

/// The feature IDs in the spec's tables (`| CB-10 | name | P1 | notes |`), parsed the way the backlog sync does:
/// two capitals, a dash, two digits, then a full table row.
#[must_use]
pub fn spec_feature_ids(spec: &str) -> BTreeSet<String> {
    spec.lines().filter_map(feature_id_of_row).collect()
}

fn feature_id_of_row(line: &str) -> Option<String> {
    let rest = line.strip_prefix("| ")?;
    let id = rest.get(..5)?;
    let bytes = id.as_bytes();
    let shaped =
        bytes[..2].iter().all(u8::is_ascii_uppercase) && bytes[2] == b'-' && bytes[3..].iter().all(u8::is_ascii_digit);
    (shaped && rest[5..].starts_with(" |") && line.trim_end().ends_with('|')).then(|| id.to_owned())
}

/// Lowercased alphanumeric words: the form in which prompts and descriptions are compared.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// All runs of [`LEAK_WORDS`] words in `text`, joined by a space.
fn windows(text: &str) -> Vec<String> {
    let words = words(text);
    words.windows(LEAK_WORDS).map(|w| w.join(" ")).collect()
}

/// Everything the lint needs besides the files themselves.
pub struct Inputs<'a> {
    /// What was found under `evals/`.
    pub discovery: &'a Discovery,
    /// Feature IDs of the spec.
    pub features: &'a BTreeSet<String>,
    /// The private list.
    pub private: &'a PrivateList,
    /// Tool descriptions and instructions, or `None` when the prompt-leak check cannot run.
    pub tool_texts: Option<&'a [ToolText]>,
}

/// Runs every rule; the result is sorted by file, then message.
#[must_use]
pub fn lint(inputs: &Inputs<'_>) -> Vec<Finding> {
    let d = inputs.discovery;
    let mut out = Vec::new();
    out.extend(unparsed_findings(d));
    out.extend(duplicate_ids(d));
    for task in &d.tasks {
        out.extend(task_findings(inputs, task));
    }
    out.extend(provenance_findings(d, inputs.private));
    if let Some(texts) = inputs.tool_texts {
        out.extend(leak_findings(d, texts));
    }
    out.sort();
    out
}

/// A file that did not parse; an unknown check kind gets its own message because it is the likeliest mistake.
fn unparsed_findings(d: &Discovery) -> Vec<Finding> {
    let mut out = Vec::new();
    for bad in &d.unparsed {
        if bad.unknown_kinds.is_empty() {
            out.push(finding(&bad.path, format!("does not parse: {}", bad.reason)));
        }
        out.extend(bad.unknown_kinds.iter().map(|k| finding(&bad.path, format!("unknown check kind `{k}`"))));
    }
    out
}

fn duplicate_ids(d: &Discovery) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut check = |what: &str, entries: Vec<(&str, &std::path::Path)>| {
        let mut seen: HashMap<&str, &std::path::Path> = HashMap::new();
        for (id, path) in entries {
            if let Some(first) = seen.insert(id, path) {
                out.push(finding(path, format!("duplicate {what} id `{id}` (also in {})", first.display())));
            }
        }
    };
    check("task", d.tasks.iter().map(|t| (t.task.id.as_str(), t.path.as_path())).collect());
    check("fixture", d.fixtures.iter().map(|f| (f.fixture.id.as_str(), f.path.as_path())).collect());
    check("eval set", d.set_files.iter().map(|(p, s)| (s.id.as_str(), p.as_path())).collect());
    out
}

fn task_findings(inputs: &Inputs<'_>, file: &TaskFile) -> Vec<Finding> {
    let task = &file.task;
    let mut out = Vec::new();
    let mut say = |message: String| out.push(finding(&file.path, message));
    if task.id.is_empty()
        || !task.id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
    {
        say(format!("task id `{}` must be lowercase letters, digits, dots and dashes", task.id));
    }
    if inputs.discovery.fixture(&task.fixture).is_none() {
        say(format!("unknown fixture `{}`", task.fixture));
    }
    if !inputs.features.contains(&task.feature) {
        say(format!("feature `{}` is not a feature ID in the spec", task.feature));
    }
    for (expect_pass, word) in [(true, "pass"), (false, "fail")] {
        if !file.scripts.iter().any(|s| s.expect_pass == expect_pass) {
            say(format!("no `{}.<name>.{word}.jsonl` script beside the task", stem(file)));
        }
    }
    if !task.checks.iter().any(|c| c.hard) {
        say("needs at least one hard check".into());
    }
    let mut ids = HashSet::new();
    for check in &task.checks {
        if !ids.insert(check.id.as_str()) {
            say(format!("duplicate check id `{}`", check.id));
        }
    }
    out
}

fn stem(file: &TaskFile) -> String {
    file.path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_owned()
}

/// Provenance (design 11.2): every item of every set, and every set item a fixture refers to.
fn provenance_findings(d: &Discovery, private: &PrivateList) -> Vec<Finding> {
    let mut out = Vec::new();
    for (path, set) in &d.set_files {
        for item in &set.items {
            out.extend(refusals(item, private).into_iter().map(|m| finding(path, format!("set `{}`: {m}", set.id))));
        }
    }
    for fixture in &d.fixtures {
        out.extend(fixture_refusals(&fixture.fixture, &d.sets, private).into_iter().map(|m| finding(&fixture.path, m)));
    }
    out
}

/// A prompt must not share [`LEAK_WORDS`] words in a row with anything an agent reads.
fn leak_findings(d: &Discovery, texts: &[ToolText]) -> Vec<Finding> {
    let mut index: BTreeMap<String, &str> = BTreeMap::new();
    for text in texts {
        for window in windows(&text.text) {
            index.entry(window).or_insert(&text.source);
        }
    }
    let mut out = Vec::new();
    for task in &d.tasks {
        if let Some((window, source)) =
            windows(&task.task.prompt).into_iter().find_map(|w| index.get(&w).map(|s| (w, *s)))
        {
            out.push(finding(
                &task.path,
                format!("prompt shares {LEAK_WORDS}+ words with {source}: \"{window}\"; a description that quotes a task measures the quote"),
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests;
