//! `cargo xtask docs-index` and `cargo xtask lint-docs-index`: the docs index, generated from each doc's front matter
//! (ADR 0053 §3).
//!
//! Why: the hand-written index in `docs/README.md` was edited by every pull request that added a doc, so parallel work
//! collided on it. Each doc now starts with a front matter block (`title`, `summary`, `area`), `docs-index` prints the
//! index from those, and the lint keeps `docs/README.md` to its introduction. Like `crate-table`, the output is printed,
//! never committed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use xtask::registry::Error as ReadError;

use crate::error::Result;
use crate::workspace::Workspace;

/// The introduction that must not grow an index list.
const README: &str = "docs/README.md";
/// The folder the index covers.
const DOCS: &str = "docs";
/// The fields every doc declares, in the order they are written.
const FIELDS: [&str; 3] = ["title", "summary", "area"];
/// Areas listed first, in this order; any other area follows alphabetically.
const AREA_ORDER: [&str; 6] = ["product", "design", "research", "decisions", "licensing", "maintainers"];

/// One doc under `docs/`, as its front matter describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Path relative to `docs/`, with `/` separators.
    pub path: String,
    /// `title`, if declared and not blank.
    pub title: Option<String>,
    /// `summary`, if declared and not blank.
    pub summary: Option<String>,
    /// `area`, if declared and not blank.
    pub area: Option<String>,
    /// Why the front matter can't be read, if it can't (no block, never closed, a line that is not `key: value`).
    pub problem: Option<String>,
}

/// The front matter of `text`: the fields it declares, or why there is none.
fn parse_front_matter(text: &str) -> std::result::Result<Vec<(String, String)>, String> {
    let mut lines = text.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return Err("no front matter (the file must start with a `---` line)".to_owned());
    }
    let mut fields = Vec::new();
    for line in lines {
        if line.trim_end() == "---" {
            return Ok(fields);
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(format!("front matter line {line:?} is not `key: value`"));
        };
        fields.push((key.trim().to_owned(), unquote(value.trim())));
    }
    Err("front matter is never closed by a `---` line".to_owned())
}

/// A value as written: plain text, or a double-quoted string with `\"` and `\\` escapes.
fn unquote(value: &str) -> String {
    let Some(inner) = value.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) else {
        return value.to_owned();
    };
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some(next @ ('"' | '\\'))) => {
                out.push(next);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// The entry for the doc at `path` (relative to `docs/`) whose text is `text`.
pub fn entry(path: &str, text: &str) -> Entry {
    let (fields, problem) = match parse_front_matter(text) {
        Ok(fields) => (fields, None),
        Err(problem) => (Vec::new(), Some(problem)),
    };
    let field = |name: &str| {
        fields.iter().find(|(key, _)| key == name).map(|(_, value)| value.clone()).filter(|v| !v.trim().is_empty())
    };
    Entry { path: path.to_owned(), title: field("title"), summary: field("summary"), area: field("area"), problem }
}

/// Every Markdown file under `dir` except `skip`, sorted so the output is deterministic.
fn markdown_files(dir: &Path, skip: &Path, found: &mut Vec<PathBuf>) -> Result<()> {
    let read = |source| ReadError::Read { path: dir.to_path_buf(), source };
    let mut children: Vec<PathBuf> = Vec::new();
    for child in fs::read_dir(dir).map_err(read)? {
        children.push(child.map_err(read)?.path());
    }
    children.sort();
    for path in children {
        if path.is_dir() {
            markdown_files(&path, skip, found)?;
        } else if is_markdown(&path.to_string_lossy()) && path != skip {
            found.push(path);
        }
    }
    Ok(())
}

/// The entries for every doc under `docs/` except `docs/README.md`, in path order.
///
/// # Errors
/// A folder or file that can't be read.
pub fn entries(workspace: &Workspace) -> Result<Vec<Entry>> {
    let docs = workspace.root.join(DOCS);
    let mut files = Vec::new();
    markdown_files(&docs, &workspace.root.join(README), &mut files)?;
    let mut entries = Vec::with_capacity(files.len());
    for file in files {
        let text = fs::read_to_string(&file).map_err(|source| ReadError::Read { path: file.clone(), source })?;
        let relative = file.strip_prefix(&docs).unwrap_or(&file);
        let path = relative.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
        entries.push(entry(&path, &text));
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// The area's heading: its name with a capital.
fn heading(area: &str) -> String {
    let mut chars = area.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

/// Text made safe for one link label: a bracket would end it, and a line break would end the list item.
fn label(text: &str) -> String {
    text.replace('[', "\\[").replace(']', "\\]").replace(['\r', '\n'], " ")
}

/// The Markdown index: one section per area, one list item per doc. Docs without all three fields are left out (the
/// lint reports them).
pub fn index(entries: &[Entry]) -> String {
    let mut areas: Vec<&str> = entries.iter().filter_map(|e| e.area.as_deref()).collect();
    areas.sort_by_key(|area| (AREA_ORDER.iter().position(|known| known == area).unwrap_or(AREA_ORDER.len()), *area));
    areas.dedup();
    let mut out = String::new();
    for area in areas {
        out += &["## ", &heading(area), "\n"].concat();
        for e in entries.iter().filter(|e| e.area.as_deref() == Some(area)) {
            if let (Some(title), Some(summary)) = (&e.title, &e.summary) {
                let summary = summary.replace(['\r', '\n'], " ");
                out += &["- [", &label(title), "](", &e.path, "): ", &summary, "\n"].concat();
            }
        }
        out.push('\n');
    }
    out.truncate(out.trim_end().len());
    out.push('\n');
    out
}

/// Whether `path` names a Markdown file.
fn is_markdown(path: &str) -> bool {
    Path::new(path).extension().is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

/// Whether `line` is a list item that starts like an index entry: a path or folder in backticks, or a link to a doc.
fn is_index_item(line: &str) -> bool {
    let trimmed = line.trim_start();
    let body = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")).or_else(|| {
        trimmed
            .split_once(". ")
            .filter(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .map(|(_, rest)| rest)
    });
    let Some(body) = body else { return false };
    if let Some(quoted) = body.strip_prefix('`') {
        let name = quoted.split('`').next().unwrap_or("");
        return is_markdown(name) || name.ends_with('/');
    }
    body.strip_prefix('[').and_then(|rest| rest.split_once("](")).is_some_and(|(_, target)| {
        let target = target.split(')').next().unwrap_or("");
        !target.contains("://") && is_markdown(target.split('#').next().unwrap_or(""))
    })
}

/// Every problem: a doc whose front matter is missing or incomplete, an index list in the README.
pub fn violations(entries: &[Entry], readme: &str) -> Vec<String> {
    let mut found = Vec::new();
    for e in entries {
        if let Some(problem) = &e.problem {
            found.push(format!("{DOCS}/{}: {problem}", e.path));
            continue;
        }
        let present = [&e.title, &e.summary, &e.area];
        for (name, value) in FIELDS.iter().zip(present) {
            if value.is_none() {
                found.push(format!("{DOCS}/{}: front matter has no `{name}`", e.path));
            }
        }
    }
    for (number, line) in readme.lines().enumerate() {
        if is_index_item(line) {
            found.push(format!(
                "{README}:{}: a hand-written index entry; the index is generated (`cargo xtask docs-index`)",
                number + 1
            ));
        }
    }
    found
}

/// The `docs-index` command: prints the index.
///
/// # Errors
/// A doc that can't be read.
pub fn print(workspace: &Workspace) -> Result<ExitCode> {
    print!("{}", index(&entries(workspace)?));
    Ok(ExitCode::SUCCESS)
}

/// The README under the workspace `root`. It must exist: a missing file would read as an empty README and pass the
/// lint.
fn read_readme(root: &Path) -> Result<String> {
    let path = root.join(README);
    fs::read_to_string(&path).map_err(|source| ReadError::Read { path, source }.into())
}

/// The `lint-docs-index` command.
///
/// # Errors
/// A doc or the README that can't be read.
pub fn lint(workspace: &Workspace) -> Result<ExitCode> {
    let entries = entries(workspace)?;
    let readme = read_readme(&workspace.root)?;
    let found = violations(&entries, &readme);
    for violation in &found {
        println!("{violation}");
    }
    if found.is_empty() {
        println!("lint-docs-index: ok ({} docs)", entries.len());
        Ok(ExitCode::SUCCESS)
    } else {
        println!("lint-docs-index: {} violation(s); each doc describes itself in its front matter", found.len());
        Ok(ExitCode::FAILURE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "---\ntitle: \"A: \\\"b\\\"\"\nsummary: one line\narea: design\n---\n\n# A\n";

    fn doc(path: &str, title: &str, summary: &str, area: &str) -> Entry {
        entry(path, &format!("---\ntitle: {title}\nsummary: {summary}\narea: {area}\n---\n# x\n"))
    }

    #[test]
    fn front_matter_reads_plain_and_quoted_values() {
        let e = entry("design/a.md", GOOD);
        assert_eq!(e.title.as_deref(), Some("A: \"b\""));
        assert_eq!(e.summary.as_deref(), Some("one line"));
        assert_eq!(e.area.as_deref(), Some("design"));
        assert_eq!(e.problem, None);
    }

    #[test]
    fn missing_unclosed_and_blank_front_matter_are_caught() {
        let none = entry("a.md", "# A\n");
        let open = entry("b.md", "---\ntitle: x\n");
        let blank = entry("c.md", "---\ntitle: x\nsummary: \"  \"\n---\n");
        assert_eq!(
            violations(&[none, open, blank], ""),
            [
                "docs/a.md: no front matter (the file must start with a `---` line)",
                "docs/b.md: front matter is never closed by a `---` line",
                "docs/c.md: front matter has no `summary`",
                "docs/c.md: front matter has no `area`",
            ]
        );
    }

    #[test]
    fn the_index_groups_by_area_in_a_fixed_order_then_path_order() {
        let entries = [
            doc("decisions/0001.md", "0001", "first", "decisions"),
            doc("design/b.md", "B", "bee", "design"),
            doc("design/a.md", "A", "ay", "design"),
            doc("zeta/z.md", "Z", "zed", "zeta"),
            doc("product/p.md", "P", "pee", "product"),
        ];
        assert_eq!(
            index(&entries),
            "## Product\n- [P](product/p.md): pee\n\n## Design\n- [B](design/b.md): bee\n- [A](design/a.md): ay\n\n\
             ## Decisions\n- [0001](decisions/0001.md): first\n\n## Zeta\n- [Z](zeta/z.md): zed\n"
        );
    }

    #[test]
    fn a_bracket_in_a_title_stays_inside_its_label() {
        assert_eq!(index(&[doc("a.md", "a [b]", "s", "x")]), "## X\n- [a \\[b\\]](a.md): s\n");
    }

    #[test]
    fn a_hand_written_list_in_the_readme_is_caught_but_prose_is_not() {
        let readme = "# Docs index\n\nSee the index.\n- `design/a.md`: x\n1. `product/p.md`: y\n* [A](design/a.md): z\n\
                      - `decisions/`: w\n- a point about `x.md`\n- [site](https://example.com/a.md)\n";
        assert_eq!(
            violations(&[], readme).iter().map(|v| v.split(": a hand").next().unwrap_or("")).collect::<Vec<_>>(),
            ["docs/README.md:4", "docs/README.md:5", "docs/README.md:6", "docs/README.md:7"]
        );
    }

    #[test]
    fn a_missing_readme_fails_instead_of_passing_as_empty() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let error = read_readme(root.path()).expect_err("no README under the root");
        assert!(error.to_string().contains("docs/README.md"), "{error}");
    }
}
