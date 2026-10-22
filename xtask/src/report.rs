//! The machine-readable form of a check scope (`cargo xtask check --json`), which the CI plan job turns into a job
//! matrix. Keeping the arithmetic here (and tested) means the workflow files only forward strings.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::affected::{Analysis, Scope};
use crate::scans::{self, Scan};
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
    /// Everything affected, direct or not, except the link-only crates. CI tests these.
    pub full: Vec<String>,
    /// Crates affected only through a dependency they hold for linking: clippy and their link gate test only.
    pub link_only: Vec<String>,
    /// `changed`, split for a test matrix.
    pub shards_changed: Vec<Vec<String>>,
    /// `full`, split for a test matrix.
    pub shards_full: Vec<Vec<String>>,
    /// The workspace-scan test targets (`scans.rs`). They are the same whatever changed, even when `empty`: a change
    /// in one crate can break a test that reads every crate, so the plan runs them on every pull request and merge group.
    pub scans: Vec<Scan>,
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
            scans: scans::SCANS.to_vec(),
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
    /// Crates split into exactly this many partitions regardless of weight. nextest's `hash:k/n` partition is a pure
    /// function of the test name, so a count that happens to separate a crate's slowest tests stays good.
    #[serde(default)]
    parts: BTreeMap<String, usize>,
    /// Measured weights of individual partitions, keyed by their label (`name@k/n`). Hash partitions are uneven when a
    /// crate has a few very slow tests; a partition not listed weighs an equal share of the crate's weight.
    #[serde(default, rename = "part_weights")]
    by_partition: BTreeMap<String, u64>,
    /// Test binaries that run as a shard item of their own, per crate, with their measured seconds (compile plus run).
    /// A binary with a test that outlasts a whole shard's fair share is the shard's floor wherever hashing drops it; as
    /// its own item it sits in one job, and the crate's partitions run everything else.
    #[serde(default)]
    isolated: BTreeMap<String, BTreeMap<String, u64>>,
}

impl Weights {
    /// The checked-in weights. A malformed file degrades to "every crate weighs the same" rather than failing the plan
    /// job (a test pins that the real file parses).
    pub fn embedded() -> Self {
        serde_json::from_str(include_str!("../../tools/ci/weights.json")).unwrap_or_else(|_| Self::uniform())
    }

    /// Every crate weighs the same, which makes [`shard`] an even split by count.
    pub fn uniform() -> Self {
        Self {
            default: 1,
            crates: BTreeMap::new(),
            parts: BTreeMap::new(),
            by_partition: BTreeMap::new(),
            isolated: BTreeMap::new(),
        }
    }

    /// Build weights from explicit pairs (tests).
    #[cfg(test)]
    fn from_pairs(default: u64, pairs: &[(&str, u64)]) -> Self {
        Self {
            default,
            crates: pairs.iter().map(|(name, weight)| ((*name).to_owned(), *weight)).collect(),
            parts: BTreeMap::new(),
            by_partition: BTreeMap::new(),
            isolated: BTreeMap::new(),
        }
    }

    /// Run `binary` of `name` as a shard item of its own, weighing `seconds` (tests).
    #[cfg(test)]
    fn with_isolated(mut self, name: &str, binary: &str, seconds: u64) -> Self {
        self.isolated.entry(name.to_owned()).or_default().insert(binary.to_owned(), seconds);
        self
    }

    /// Set the weight of one partition label (tests).
    #[cfg(test)]
    fn with_part_weight(mut self, label: &str, weight: u64) -> Self {
        self.by_partition.insert(label.to_owned(), weight);
        self
    }

    /// Pin the partition count of `name` (tests).
    #[cfg(test)]
    fn with_parts(mut self, name: &str, parts: usize) -> Self {
        self.parts.insert(name.to_owned(), parts);
        self
    }

    /// The weight of `name`, never zero so every shard attracts work.
    fn of(&self, name: &str) -> u64 {
        self.crates.get(name).copied().unwrap_or(self.default).max(1)
    }
}

/// A crate heavier than this is worth splitting; below it the extra compile of each part costs more than it saves.
const MIN_SPLIT_SECONDS: u64 = 120;

/// What a shard item runs of its crate.
enum Run {
    /// Every test of the crate.
    All,
    /// Partition `index` of `parts` (nextest's `--partition hash:index/parts`) of the tests outside `skip`, the
    /// crate's isolated binaries. Partitioning applies after the filter, so a test's partition does not depend on which
    /// binaries are skipped, and the partitions together hold exactly the tests outside `skip`.
    Part { index: usize, parts: usize, skip: Vec<String> },
    /// Only the tests of one isolated binary.
    Only(String),
}

/// One unit of work for a shard: a crate, or the share of it that `run` names.
struct Item {
    name: String,
    weight: u64,
    run: Run,
}

impl Item {
    /// `name` for a whole crate; `name@k/n` (1-based) for a partition, with `!bin,bin` after it when binaries are
    /// skipped; `name@=bin` for an isolated binary. `tools/ci/nextest-args.py` turns the label into nextest arguments.
    fn label(&self) -> String {
        match &self.run {
            Run::All => self.name.clone(),
            Run::Part { index, parts, skip } if skip.is_empty() => format!("{}@{index}/{parts}", self.name),
            Run::Part { index, parts, skip } => format!("{}@{index}/{parts}!{}", self.name, skip.join(",")),
            Run::Only(binary) => format!("{}@={binary}", self.name),
        }
    }

    /// Whether this is one of several partitions of its crate, which must not share a shard.
    fn is_part(&self) -> bool {
        matches!(self.run, Run::Part { .. })
    }
}

/// Packs `names` into at most `max_shards` non-empty groups balanced by measured weight (none for an empty list).
///
/// A crate heavier than one shard's fair share is split into parts, written `name@k/n`, that land in different shards;
/// the test job runs them with `cargo nextest run --partition hash:k/n`. A crate with isolated binaries
/// (`Weights::isolated`) gets one item per binary, `name@=bin`, and its parts run the other tests, `name@k/n!bin`: those
/// parts weigh the crate's weight less the isolated seconds, even when the crate is not split (`name@1/1!bin`). Items
/// are then packed longest-first into the lightest shard (ties go to the lowest index), so the result depends only on
/// the input. Each shard lists its items sorted.
pub fn shard(names: &[String], max_shards: usize, weights: &Weights) -> Vec<Vec<String>> {
    pack(names, max_shards, weights)
        .into_iter()
        .map(|(_, held)| {
            let mut labels: Vec<String> = held.iter().map(Item::label).collect();
            labels.sort();
            labels
        })
        .collect()
}

/// [`shard`]'s packing: each non-empty shard with its total weight and its items.
fn pack(names: &[String], max_shards: usize, weights: &Weights) -> Vec<(u64, Vec<Item>)> {
    let max_shards = max_shards.max(1);
    let total: u64 = names.iter().map(|name| weights.of(name)).sum();
    let share = (total / u64::try_from(max_shards).unwrap_or(1)).max(MIN_SPLIT_SECONDS);
    let mut items = Vec::new();
    for name in names {
        let isolated = weights.isolated.get(name).filter(|binaries| !binaries.is_empty());
        let skip: Vec<String> = isolated.map(|binaries| binaries.keys().cloned().collect()).unwrap_or_default();
        let isolated_seconds: u64 = isolated.map_or(0, |binaries| binaries.values().sum());
        for (binary, seconds) in isolated.into_iter().flatten() {
            items.push(Item { name: name.clone(), weight: (*seconds).max(1), run: Run::Only(binary.clone()) });
        }
        let weight = weights.of(name).saturating_sub(isolated_seconds).max(1);
        let wanted = match weights.parts.get(name) {
            Some(&fixed) => fixed,
            None if weight > share => usize::try_from(weight.div_ceil(share)).unwrap_or(max_shards),
            None => 1,
        };
        let parts = wanted.min(max_shards);
        if parts > 1 || !skip.is_empty() {
            for index in 1..=parts {
                let even = weight.div_ceil(u64::try_from(parts).unwrap_or(1));
                let measured = weights.by_partition.get(&format!("{name}@{index}/{parts}")).copied();
                let run = Run::Part { index, parts, skip: skip.clone() };
                items.push(Item { name: name.clone(), weight: measured.unwrap_or(even).max(1), run });
            }
        } else {
            items.push(Item { name: name.clone(), weight, run: Run::All });
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
            .filter(|(_, (_, held))| {
                !item.is_part() || !held.iter().any(|other| other.is_part() && other.name == item.name)
            })
            .min_by_key(|(index, (load, _))| (*load, *index))
            .map_or(0, |(index, _)| index);
        shards[target].0 += item.weight;
        shards[target].1.push(item);
    }
    shards.into_iter().filter(|(_, held)| !held.is_empty()).collect()
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
    fn a_pinned_partition_count_wins_over_the_weight_rule_and_is_capped_by_the_shards() {
        let all = names(&["pinned", "x", "y"]);
        let weights = Weights::from_pairs(10, &[("pinned", 50)]).with_parts("pinned", 3);
        let items = flat(&shard(&all, 6, &weights));
        assert_eq!(items, names(&["pinned@1/3", "pinned@2/3", "pinned@3/3", "x", "y"]));
        let capped = flat(&shard(&all, 2, &weights));
        assert!(
            capped.contains(&"pinned@2/2".to_owned()) && !capped.iter().any(|item| item.ends_with("/3")),
            "{capped:?}"
        );
    }

    #[test]
    fn a_measured_partition_weight_decides_how_much_shares_its_shard() {
        let all = names(&["pinned", "a", "b", "c", "d"]);
        let weights = Weights::from_pairs(60, &[("pinned", 200)])
            .with_parts("pinned", 2)
            .with_part_weight("pinned@1/2", 150)
            .with_part_weight("pinned@2/2", 50);
        let shards = shard(&all, 2, &weights);
        let heavy = shards.iter().find(|held| held.contains(&"pinned@1/2".to_owned()));
        assert_eq!(
            heavy.map(Vec::len),
            Some(2),
            "the 150 s partition takes one light crate, the 50 s one takes the rest: {shards:?}"
        );
    }

    #[test]
    fn an_isolated_binary_is_an_item_of_its_own_and_the_partitions_run_everything_else() {
        let all = names(&["big", "x", "y"]);
        let weights = Weights::from_pairs(30, &[("big", 900)])
            .with_parts("big", 2)
            .with_isolated("big", "slow_binary", 500)
            .with_part_weight("big@1/2", 250)
            .with_part_weight("big@2/2", 150);
        let shards = shard(&all, 4, &weights);
        let items = flat(&shards);
        assert_eq!(items, names(&["big@1/2!slow_binary", "big@2/2!slow_binary", "big@=slow_binary", "x", "y"]));
        assert!(!items.iter().any(|item| item == "big"), "the crate is only run through its items: {items:?}");
        let loads: Vec<u64> = pack(&all, 4, &weights).iter().map(|(load, _)| *load).collect();
        assert_eq!(loads.iter().max(), Some(&500), "the isolated binary is the longest shard: {loads:?}");
    }

    #[test]
    fn an_unsplit_crate_with_an_isolated_binary_still_runs_the_rest_as_partition_one_of_one() {
        let weights = Weights::from_pairs(30, &[("big", 200)]).with_isolated("big", "slow_binary", 120);
        let items = flat(&shard(&names(&["big"]), 6, &weights));
        assert_eq!(items, names(&["big@1/1!slow_binary", "big@=slow_binary"]));
    }

    #[test]
    fn the_isolated_item_may_share_a_shard_with_a_partition_of_its_crate_when_the_shards_run_out() {
        let weights = Weights::from_pairs(10, &[("big", 600)]).with_parts("big", 2).with_isolated("big", "slow", 100);
        let items = flat(&shard(&names(&["big"]), 2, &weights));
        assert_eq!(items, names(&["big@1/2!slow", "big@2/2!slow", "big@=slow"]), "nothing is dropped: {items:?}");
    }

    /// The crates of merge-queue run 38810711080 (PR #1945), the shape `tools/ci/weights.README.md` was measured on.
    const RECORDED_RUN: [&str; 24] = [
        "omuzaki-anim",
        "omuzaki-audio-api",
        "omuzaki-char",
        "omuzaki-cli",
        "omuzaki-face",
        "omuzaki-history-api",
        "omuzaki-history-mock",
        "omuzaki-identity-mock",
        "omuzaki-io-ops",
        "omuzaki-library-api",
        "omuzaki-link-probe",
        "omuzaki-mcp",
        "omuzaki-ops",
        "omuzaki-ops-api",
        "omuzaki-ops-mock",
        "omuzaki-script",
        "omuzaki-settings-api",
        "omuzaki-stage",
        "omuzaki-take-api",
        "omuzaki-test",
        "omuzaki-test-char",
        "omuzaki-test-face",
        "omuzaki-ui",
        "xtask",
    ];

    #[test]
    fn the_recorded_run_packs_into_six_shards_with_the_slowest_within_20_percent_of_the_mean() {
        let weights = Weights::embedded();
        let all = names(&RECORDED_RUN);
        let shards = pack(&all, 6, &weights);
        let loads: Vec<u64> = shards.iter().map(|(load, _)| *load).collect();
        let total: u64 = loads.iter().sum();
        let slowest = loads.iter().copied().max().unwrap_or(0);
        assert_eq!(loads.len(), 6, "{loads:?}");
        // slowest <= 1.2 * (total / 6), in integers.
        assert!(slowest * 5 <= total, "predicted shard seconds {loads:?}: slowest {slowest} of {total} in all");
        // The 7.8-minute palette_independence binary is an item of its own, and omuzaki-ui, whose tests take 20 s but
        // whose build takes minutes, is compiled once.
        let labels = flat(&shard(&all, 6, &weights));
        assert!(labels.contains(&"omuzaki-char@=palette_independence".to_owned()), "{labels:?}");
        assert!(labels.contains(&"omuzaki-ui".to_owned()), "ui is one item, built once: {labels:?}");
        let char_items = labels.iter().filter(|item| item.starts_with("omuzaki-char@")).count();
        assert_eq!(char_items, 5, "four partitions and the isolated binary: {labels:?}");
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
        assert!(weights.parts.get("omuzaki-char").is_some_and(|&parts| parts > 1), "omuzaki-char is pinned to a split");
        assert_eq!(weights.parts.get("omuzaki-ui"), Some(&1), "omuzaki-ui is built once: its tests take seconds");
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
