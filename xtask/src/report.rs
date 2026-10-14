//! The machine-readable form of a check scope (`cargo xtask check --json`), which the CI plan job turns into a job
//! matrix. Keeping the arithmetic here (and tested) means the workflow files only forward strings.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::affected::{Analysis, Scope};
use crate::workspace::Workspace;

/// What `check --json` prints: one JSON object on one line.
///
/// Every crate list holds package names, sorted. A shard list splits a set into at most `--shards` groups, so each CI
/// job compiles only the crates it tests.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Report {
    /// `"workspace"` when the scope is the whole workspace, otherwise `"crates"`.
    pub mode: &'static str,
    /// The changed file that widened the scope to the whole workspace, if one did.
    pub trigger: Option<String>,
    /// True when no crate is affected (a docs-only change): there is nothing to build or test.
    pub empty: bool,
    /// Crates that own a changed file. Pull requests test these.
    pub changed: Vec<String>,
    /// `changed` plus the crates that depend on one directly. Pull requests run clippy on these.
    pub direct: Vec<String>,
    /// Everything affected, direct or not, except the link-only crates. The merge queue tests these.
    pub full: Vec<String>,
    /// Crates affected only through a dependency they hold for linking: clippy and their link gate test only.
    pub link_only: Vec<String>,
    /// `changed`, split for a test matrix.
    pub shards_changed: Vec<Vec<String>>,
    /// `full`, split for a test matrix.
    pub shards_full: Vec<Vec<String>>,
}

impl Report {
    /// The report for an analysed change, splitting test sets into at most `max_shards` groups.
    pub fn new(workspace: &Workspace, analysis: &Analysis, max_shards: usize) -> Self {
        let owned = |names: &BTreeSet<String>| names.iter().cloned().collect::<Vec<_>>();
        let (mode, trigger, full, link_only) = match &analysis.scope {
            Scope::Workspace { trigger } => (
                "workspace",
                trigger.as_ref().map(|file| file.display().to_string()),
                workspace.packages.iter().map(|package| package.name.clone()).collect::<Vec<_>>(),
                Vec::new(),
            ),
            Scope::Crates { full, link_only } => ("crates", None, owned(full), owned(link_only)),
        };
        let changed = owned(&analysis.changed);
        Self {
            mode,
            trigger,
            empty: analysis.scope.is_empty(),
            shards_changed: shard(&changed, max_shards),
            shards_full: shard(&full, max_shards),
            direct: owned(&analysis.direct),
            changed,
            full,
            link_only,
        }
    }
}

impl Report {
    /// The one-line JSON form. Plain data (strings, bools, lists) cannot fail to serialise.
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| String::from("{}"))
    }
}

/// Deals `names` round-robin into at most `max_shards` non-empty groups (none for an empty list).
///
/// Round-robin over sorted names, not a balance by test time: the shards only have to be roughly even, and a name-based
/// split needs no measured weights to stay current.
pub fn shard(names: &[String], max_shards: usize) -> Vec<Vec<String>> {
    let count = max_shards.max(1).min(names.len());
    let mut shards = vec![Vec::new(); count];
    for (index, name) in names.iter().enumerate() {
        shards[index % count].push(name.clone());
    }
    shards
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::affected::analyse_files;
    use crate::workspace::DepKind::Normal;
    use crate::workspace::testing::workspace;
    use std::path::PathBuf;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| (*name).to_owned()).collect()
    }

    fn sample() -> Workspace {
        workspace(&[
            ("omuzaki-core", &[]),
            ("omuzaki-doc-api", &[("omuzaki-core", Normal)]),
            ("omuzaki-render", &[("omuzaki-doc-api", Normal)]),
            ("omuzaki-cli", &[("omuzaki-render", Normal)]),
        ])
    }

    #[test]
    fn shards_deal_round_robin_and_never_exceed_the_cap() {
        let all = names(&["a", "b", "c", "d", "e"]);
        assert_eq!(shard(&all, 2), [names(&["a", "c", "e"]), names(&["b", "d"])]);
        assert_eq!(shard(&all, 9), all.iter().map(|name| vec![name.clone()]).collect::<Vec<_>>());
        assert_eq!(shard(&all, 0), std::slice::from_ref(&all), "a zero cap still yields one shard");
        assert!(shard(&[], 4).is_empty(), "no crates means no shards, so no empty matrix jobs");
    }

    #[test]
    fn a_leaf_change_tests_only_the_changed_crate_but_lints_its_direct_dependents() {
        let analysis = analyse_files(&sample(), &[PathBuf::from("crates/omuzaki-doc-api/src/lib.rs")]);
        let report = Report::new(&sample(), &analysis, 4);
        assert_eq!(report.mode, "crates");
        assert_eq!(report.changed, names(&["omuzaki-doc-api"]));
        assert_eq!(report.direct, names(&["omuzaki-doc-api", "omuzaki-render"]));
        assert_eq!(report.full, names(&["omuzaki-cli", "omuzaki-doc-api", "omuzaki-render"]));
        assert_eq!(report.shards_changed, [names(&["omuzaki-doc-api"])]);
        assert_eq!(report.shards_full.len(), 3);
        assert!(!report.empty);
    }

    #[test]
    fn a_docs_only_change_is_empty() {
        let analysis = analyse_files(&sample(), &[PathBuf::from("docs/README.md")]);
        let report = Report::new(&sample(), &analysis, 4);
        assert!(report.empty);
        assert!(report.shards_changed.is_empty() && report.shards_full.is_empty());
    }

    #[test]
    fn a_workspace_wide_change_selects_every_crate_and_names_the_trigger() {
        let analysis = analyse_files(&sample(), &[PathBuf::from("Cargo.lock")]);
        let report = Report::new(&sample(), &analysis, 2);
        assert_eq!(report.mode, "workspace");
        assert_eq!(report.trigger.as_deref(), Some("Cargo.lock"));
        assert_eq!(report.changed.len(), 4);
        assert_eq!(report.direct.len(), 4);
        assert_eq!(report.shards_full.len(), 2);
    }

    #[test]
    fn the_report_serialises_to_one_json_line() {
        let analysis = analyse_files(&sample(), &[PathBuf::from("crates/omuzaki-core/src/lib.rs")]);
        let json = Report::new(&sample(), &analysis, 1).to_json();
        assert!(!json.contains('\n'));
        assert!(
            json.starts_with(r#"{"mode":"crates","trigger":null,"empty":false,"changed":["omuzaki-core"]"#),
            "{json}"
        );
    }
}
