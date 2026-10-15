//! The licence files (`docs/licensing/deps/<name>.toml`, one per item) and the register page generated from them.

use std::fs;
use std::path::Path;

use serde::Deserialize;

/// The verdicts `docs/licensing/policy.md` defines.
pub const STATUSES: &[&str] = &["yes", "conditions", "external", "NO", "unverified", "private-test"];

/// The verdicts that bar the item from anything in use (policy: `unverified` counts as `NO`; `private-test` material is
/// never committed). A file with one of these may only list the item, never claim a crate, tool or path.
const BARRED: &[&str] = &["NO", "unverified", "private-test"];

/// The verdict for a separately installed tool; only a tool may carry it.
const EXTERNAL: &str = "external";

/// The `kind` an `external` file must have.
const TOOL_KIND: &str = "tool";

/// The line in `register.md` that the table of one section replaces: `<!-- licence-table: <section> -->`.
const MARKER: (&str, &str) = ("<!-- licence-table: ", " -->");

/// The table's head; the columns are the fields of [`Entry`] in this order.
const HEAD: &str = "| Item | Kind | Used for | Status | Key condition | Audit |\n|---|---|---|---|---|---|";

/// One licence file: a row of the register plus what it covers, so the lint can tell it is still in use.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    /// The register section the row is listed under (the name in its `licence-table` marker).
    pub section: String,
    /// What it is, as the register names it.
    pub item: String,
    /// `crate`, `tool`, `model`, `font`, ...
    pub kind: String,
    /// Why we use it.
    pub used_for: String,
    /// The verdict, one of [`STATUSES`].
    pub status: String,
    /// The conditions of using it, if any.
    #[serde(default)]
    pub condition: String,
    /// Who checked it and where: the audit section or the source read.
    #[serde(default)]
    pub audit: String,
    /// The Cargo packages this file covers.
    #[serde(default)]
    pub crates: Vec<String>,
    /// The tools CI installs (a workflow's `tools:` input) this file covers.
    #[serde(default)]
    pub tools: Vec<String>,
    /// The repository files this file covers, each named exactly: a folder (a path ending in `/`) is a problem, so a
    /// file added later has no claim until someone audits it.
    #[serde(default)]
    pub paths: Vec<String>,
}

impl Entry {
    /// The table cells, in column order.
    fn cells(&self) -> [&str; 6] {
        [&self.item, &self.kind, &self.used_for, &self.status, &self.condition, &self.audit]
    }

    /// What is wrong with the row itself, as messages.
    pub fn problems(&self) -> Vec<String> {
        let mut found = Vec::new();
        if !STATUSES.contains(&self.status.as_str()) {
            found.push(format!("status `{}` is not one of {}", self.status, STATUSES.join(", ")));
        }
        if BARRED.contains(&self.status.as_str())
            && !(self.crates.is_empty() && self.tools.is_empty() && self.paths.is_empty())
        {
            found.push(format!(
                "status `{}` bars the item from use, yet the file claims a crate, tool or path that is in use",
                self.status
            ));
        }
        if self.status == EXTERNAL && self.kind != TOOL_KIND {
            found.push(format!("status `external` is for tools installed separately, yet the kind is `{}`", self.kind));
        }
        if self.status == EXTERNAL && !(self.crates.is_empty() && self.paths.is_empty()) {
            found.push(
                "status `external` is for tools installed separately, yet the file claims a crate or a path".to_owned(),
            );
        }
        found.extend(
            self.paths
                .iter()
                .filter(|path| path.ends_with('/'))
                .map(|path| format!("path `{path}` is a folder; name each file, so a new one needs an audit")),
        );
        if self.cells().iter().any(|cell| cell.contains(['|', '\n'])) {
            found.push("a field holds `|` or a line break, which would break the table".to_owned());
        }
        found
    }

    /// Whether `path` (relative, `/`-separated) is exactly one of this file's paths.
    pub fn covers_path(&self, path: &str) -> bool {
        self.paths.iter().any(|own| own == path)
    }

    fn row(&self) -> String {
        let cells: Vec<String> = self
            .cells()
            .iter()
            .map(|cell| if cell.is_empty() { " ".to_owned() } else { format!(" {cell} ") })
            .collect();
        format!("|{}|", cells.join("|"))
    }
}

/// A licence file and its name (the file stem), which orders the rows.
pub type Named = (String, Entry);

/// Reads every `*.toml` in `dir`, sorted by name. Files that don't parse come back as `(name, message)`.
pub fn load(dir: &Path) -> (Vec<Named>, Vec<(String, String)>) {
    let mut entries = Vec::new();
    let mut broken = Vec::new();
    let mut files: Vec<_> = match fs::read_dir(dir) {
        Ok(read) => read.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect(),
        Err(error) => return (entries, vec![(dir.display().to_string(), error.to_string())]),
    };
    files.sort();
    for path in files.iter().filter(|path| path.extension().is_some_and(|ext| ext == "toml")) {
        let name = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
        let parsed = fs::read_to_string(path).map_err(|e| e.to_string());
        match parsed.and_then(|text| toml::from_str(&text).map_err(|e: toml::de::Error| e.message().to_owned())) {
            Ok(entry) => entries.push((name, entry)),
            Err(message) => broken.push((name, message)),
        }
    }
    (entries, broken)
}

/// The section a marker line names, if the line is a marker.
pub fn marker(line: &str) -> Option<&str> {
    line.trim().strip_prefix(MARKER.0)?.strip_suffix(MARKER.1)
}

/// `page` with every marker replaced by its section's table, rows in file-name order.
pub fn render(page: &str, entries: &[Named]) -> String {
    let mut out = String::with_capacity(page.len() + entries.len() * 200);
    for line in page.split_inclusive('\n') {
        match marker(line) {
            Some(section) => {
                out.push_str(HEAD);
                for (_, entry) in entries.iter().filter(|(_, entry)| entry.section == section) {
                    out.push('\n');
                    out.push_str(&entry.row());
                }
                out.push_str(if line.ends_with('\n') { "\n" } else { "" });
            }
            None => out.push_str(line),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(section: &str, item: &str, condition: &str) -> Entry {
        Entry {
            section: section.into(),
            item: item.into(),
            kind: "crate".into(),
            used_for: "tests".into(),
            status: "yes".into(),
            condition: condition.into(),
            audit: "B1".into(),
            crates: Vec::new(),
            tools: Vec::new(),
            paths: Vec::new(),
        }
    }

    #[test]
    fn markers_become_tables_and_empty_cells_keep_the_register_spacing() {
        let page = "# R\n\n## A\n\n<!-- licence-table: a -->\n\n## B\n\n<!-- licence-table: b -->\n";
        let entries = [("x".into(), entry("a", "x", "")), ("y".into(), entry("b", "y", "keep notice"))];
        let got = render(page, &entries);
        assert_eq!(
            got,
            format!(
                "# R\n\n## A\n\n{HEAD}\n| x | crate | tests | yes | | B1 |\n\n## B\n\n{HEAD}\n| y | crate | tests | yes | keep notice | B1 |\n"
            )
        );
    }

    #[test]
    fn a_path_covers_exactly_its_file_and_a_folder_is_a_problem() {
        let mut e = entry("a", "x", "");
        e.paths = vec!["fonts/a.ttf".into(), "logo.png".into()];
        assert!(e.covers_path("fonts/a.ttf") && e.covers_path("logo.png") && e.problems().is_empty());
        assert!(!e.covers_path("fonts/b.ttf") && !e.covers_path("logo.png.bak"));
        e.paths = vec!["fonts/".into()];
        assert!(!e.covers_path("fonts/a.ttf"), "a folder claims nothing");
        assert_eq!(e.problems().len(), 1, "{:?}", e.problems());
    }

    #[test]
    fn external_is_for_tools_only() {
        let mut e = entry("a", "x", "");
        e.status = "external".into();
        e.kind = "tool".into();
        e.tools = vec!["git".into()];
        assert!(e.problems().is_empty(), "{:?}", e.problems());
        for kind in ["model", "native", "crate"] {
            e.kind = kind.into();
            assert_eq!(e.problems().len(), 1, "{kind}: {:?}", e.problems());
        }
        e.kind = "tool".into();
        e.crates = vec!["x".into()];
        assert_eq!(e.problems().len(), 1, "{:?}", e.problems());
        e.crates.clear();
        e.paths = vec!["a/b.png".into()];
        assert_eq!(e.problems().len(), 1, "{:?}", e.problems());
    }

    #[test]
    fn a_barred_verdict_may_not_claim_anything_in_use() {
        for status in BARRED {
            let mut e = entry("a", "x", "");
            e.status = (*status).into();
            assert!(e.problems().is_empty(), "{status}: a planned item is fine");
            e.crates = vec!["x".into()];
            assert_eq!(e.problems().len(), 1, "{status}: {:?}", e.problems());
            e.crates.clear();
            e.paths = vec!["a/b.png".into()];
            assert_eq!(e.problems().len(), 1, "{status}");
        }
    }

    #[test]
    fn a_bad_status_or_a_pipe_is_a_problem() {
        let mut e = entry("a", "x | y", "");
        e.status = "maybe".into();
        assert_eq!(e.problems().len(), 2, "{:?}", e.problems());
    }
}
