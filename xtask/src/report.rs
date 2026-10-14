//! The machine-readable form of a check scope (`cargo xtask check --json`), which the CI plan job turns into a job
//! matrix. Keeping the arithmetic here (and tested) means the workflow files only forward strings.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::affected::{Analysis, Scope};
use crate::workspace::Workspace;

/// What `check --json` prints: one JSON object on one line.
///
/// Every crate list holds package names, sorted. A shard list splits a set into at most `--shards` groups balanced by
/// measured test time, so each CI job compiles only the crates it tests. A shard entry is a package name, or
/// `name@k/n` when the package is split into `n` partitions and this shard runs partition `k`.
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
        Self::with_weights(workspace, analysis, max_shards, &Weights::embedded())
    }

    /// [`Report::new`] with explicit crate weights.
    pub fn with_weights(workspace: &Workspace, analysis: &Analysis, max_shards: usize, weights: &Weights) -> Self {
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
            shards_changed: shard(&changed, max_shards, weights),
            shards_full: shard(&full, max_shards, weights),
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

/// Seconds of test time per crate, measured from CI logs (`tools/ci/weights.json`, provenance in
/// `tools/ci/weights.README.md`). Embedded at build time so the plan job needs no extra file handling.
#[derive(Debug, Clone, Deserialize)]
pub struct Weights {
    /// Weight of a crate that is not listed.
    default: u64,
    crates: BTreeMap<String, u64>,
}

impl Weights {
    /// The checked-in weights. A malformed file degrades to "every crate weighs the same" rather than failing the plan
    /// job (a test pins that the real file parses).
    pub fn embedded() -> Self {
        serde_json::from_str(include_str!("../../tools/ci/weights.json")).unwrap_or_else(|_| Self::uniform())
    }

    /// Every crate weighs the same, which makes [`shard`] an even split by count.
    pub fn uniform() -> Self {
        Self { default: 1, crates: BTreeMap::new() }
    }

    /// Build weights from explicit pairs (tests).
    #[cfg(test)]
    fn from_pairs(default: u64, pairs: &[(&str, u64)]) -> Self {
        Self { default, crates: pairs.iter().map(|(name, weight)| ((*name).to_owned(), *weight)).collect() }
    }

    /// The weight of `name`, never zero so every shard attracts work.
    fn of(&self, name: &str) -> u64 {
        self.crates.get(name).copied().unwrap_or(self.default).max(1)
    }
}

/// A crate heavier than this is worth splitting; below it the extra compile of each part costs more than it saves.
const MIN_SPLIT_SECONDS: u64 = 120;

/// One unit of work for a shard: a whole crate or `part` of `parts` of it.
struct Item {
    name: String,
    weight: u64,
    part: Option<(usize, usize)>,
}

impl Item {
    /// `name` for a whole crate, `name@k/n` (1-based, nextest's `--partition hash:k/n`) for a part.
    fn label(&self) -> String {
        match self.part {
            Some((index, parts)) => format!("{}@{index}/{parts}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Packs `names` into at most `max_shards` non-empty groups balanced by measured weight (none for an empty list).
///
/// A crate heavier than one shard's fair share is split into parts, written `name@k/n`, that land in different shards;
/// the test job runs them with `cargo nextest run --partition hash:k/n`. Parts are then packed longest-first into the
/// lightest shard (ties go to the lowest index), so the result depends only on the input. Each shard lists its items
/// sorted.
pub fn shard(names: &[String], max_shards: usize, weights: &Weights) -> Vec<Vec<String>> {
    let max_shards = max_shards.max(1);
    let total: u64 = names.iter().map(|name| weights.of(name)).sum();
    let share = (total / u64::try_from(max_shards).unwrap_or(1)).max(MIN_SPLIT_SECONDS);
    let mut items = Vec::new();
    for name in names {
        let weight = weights.of(name);
        if weight > share && max_shards > 1 {
            let parts = usize::try_from(weight.div_ceil(share)).unwrap_or(max_shards).min(max_shards);
            for index in 1..=parts {
                items.push(Item {
                    name: name.clone(),
                    weight: weight.div_ceil(u64::try_from(parts).unwrap_or(1)),
                    part: Some((index, parts)),
                });
            }
        } else {
            items.push(Item { name: name.clone(), weight, part: None });
        }
    }
    items.sort_by(|a, b| b.weight.cmp(&a.weight).then_with(|| a.label().cmp(&b.label())));

    let count = max_shards.min(items.len());
    let mut shards: Vec<(u64, Vec<Item>)> = (0..count).map(|_| (0, Vec::new())).collect();
    for item in items {
        // Parts of one crate must not share a shard, or the split buys nothing; there are never more parts than shards.
        let target = shards
            .iter()
            .enumerate()
            .filter(|(_, (_, held))| item.part.is_none() || !held.iter().any(|other| other.name == item.name))
            .min_by_key(|(index, (load, _))| (*load, *index))
            .map_or(0, |(index, _)| index);
        shards[target].0 += item.weight;
        shards[target].1.push(item);
    }
    shards
        .into_iter()
        .filter(|(_, held)| !held.is_empty())
        .map(|(_, held)| {
            let mut labels: Vec<String> = held.iter().map(Item::label).collect();
            labels.sort();
            labels
        })
        .collect()
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

    fn flat(shards: &[Vec<String>]) -> Vec<String> {
        let mut all: Vec<String> = shards.iter().flatten().cloned().collect();
        all.sort();
        all
    }

    #[test]
    fn shards_never_exceed_the_cap_and_empty_input_means_no_shards() {
        let all = names(&["a", "b", "c", "d", "e"]);
        let weights = Weights::uniform();
        assert_eq!(shard(&all, 2, &weights).len(), 2);
        assert_eq!(shard(&all, 9, &weights).len(), 5, "never more shards than items");
        assert_eq!(shard(&all, 0, &weights).len(), 1, "a zero cap still yields one shard");
        assert!(shard(&[], 4, &weights).is_empty(), "no crates means no shards, so no empty matrix jobs");
    }

    #[test]
    fn packing_is_deterministic_and_balances_by_weight() {
        let all = names(&["a", "b", "c", "d", "e", "f"]);
        let weights = Weights::from_pairs(5, &[("a", 100), ("b", 90), ("c", 50), ("d", 40), ("e", 30), ("f", 10)]);
        let first = shard(&all, 3, &weights);
        assert_eq!(first, shard(&all, 3, &weights));
        let mut reversed = all.clone();
        reversed.reverse();
        assert_eq!(first, shard(&reversed, 3, &weights), "input order does not matter");
        // Longest-first into the lightest shard (lowest index on a tie): 100 | 90 | 50, then 40 joins 50, 30 joins 90, 10 joins 90.
        assert_eq!(first, [names(&["a"]), names(&["b", "e"]), names(&["c", "d", "f"])]);
    }

    #[test]
    fn a_heavy_crate_is_split_into_parts_in_different_shards() {
        let all = names(&["heavy", "light-a", "light-b", "light-c"]);
        let weights = Weights::from_pairs(30, &[("heavy", 1200)]);
        let shards = shard(&all, 4, &weights);
        assert_eq!(shards.len(), 4);
        let holders: Vec<usize> = shards
            .iter()
            .enumerate()
            .filter(|(_, held)| held.iter().any(|item| item.starts_with("heavy@")))
            .map(|(index, _)| index)
            .collect();
        let parts = flat(&shards).into_iter().filter(|item| item.starts_with("heavy@")).collect::<Vec<_>>();
        assert!(parts.len() > 1, "{shards:?}");
        assert_eq!(holders.len(), parts.len(), "every part sits in its own shard: {shards:?}");
        let count = parts.len();
        let expected: Vec<String> = (1..=count).map(|index| format!("heavy@{index}/{count}")).collect();
        assert_eq!(parts, expected);
    }

    #[test]
    fn every_crate_appears_once_or_as_all_of_its_partitions() {
        let all = names(&["a", "b", "c", "d", "huge", "x"]);
        let weights = Weights::from_pairs(20, &[("huge", 3000), ("a", 400)]);
        let items = flat(&shard(&all, 5, &weights));
        for name in ["a", "b", "c", "d", "x"] {
            assert_eq!(items.iter().filter(|item| item.as_str() == name).count(), 1, "{name} in {items:?}");
        }
        let parts: Vec<&String> = items.iter().filter(|item| item.starts_with("huge@")).collect();
        let n = parts.len();
        assert!((2..=5).contains(&n));
        for index in 1..=n {
            assert!(parts.contains(&&format!("huge@{index}/{n}")), "{items:?}");
        }
        assert!(!items.iter().any(|item| item == "huge"));
    }

    #[test]
    fn a_light_workload_is_never_split() {
        let weights = Weights::from_pairs(20, &[("a", 100)]);
        let shards = shard(&names(&["a"]), 6, &weights);
        assert_eq!(shards, [names(&["a"])], "below the split floor one job is cheaper than several");
    }

    #[test]
    fn the_checked_in_weights_parse_and_name_the_known_heavy_crate() {
        let weights = Weights::embedded();
        assert!(weights.of("omuzaki-char") > MIN_SPLIT_SECONDS, "weights.json failed to parse or lost omuzaki-char");
        assert!(weights.of("not-a-crate") >= 1);
    }

    #[test]
    fn a_leaf_change_tests_only_the_changed_crate_but_lints_its_direct_dependents() {
        let analysis = analyse_files(&sample(), &[PathBuf::from("crates/omuzaki-doc-api/src/lib.rs")]);
        let report = Report::with_weights(&sample(), &analysis, 4, &Weights::uniform());
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
        let report = Report::with_weights(&sample(), &analysis, 4, &Weights::uniform());
        assert!(report.empty);
        assert!(report.shards_changed.is_empty() && report.shards_full.is_empty());
    }

    #[test]
    fn a_workspace_wide_change_selects_every_crate_and_names_the_trigger() {
        let analysis = analyse_files(&sample(), &[PathBuf::from("Cargo.lock")]);
        let report = Report::with_weights(&sample(), &analysis, 2, &Weights::uniform());
        assert_eq!(report.mode, "workspace");
        assert_eq!(report.trigger.as_deref(), Some("Cargo.lock"));
        assert_eq!(report.changed.len(), 4);
        assert_eq!(report.direct.len(), 4);
        assert_eq!(report.shards_full.len(), 2);
    }

    #[test]
    fn the_report_serialises_to_one_json_line() {
        let analysis = analyse_files(&sample(), &[PathBuf::from("crates/omuzaki-core/src/lib.rs")]);
        let json = Report::with_weights(&sample(), &analysis, 1, &Weights::uniform()).to_json();
        assert!(!json.contains('\n'));
        assert!(
            json.starts_with(r#"{"mode":"crates","trigger":null,"empty":false,"changed":["omuzaki-core"]"#),
            "{json}"
        );
    }
}
