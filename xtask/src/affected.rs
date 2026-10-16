//! Which crates does a change touch? Changed files map to crates, then everything that depends on them joins in.
//!
//! Why: a PR should prove itself on the crates it can break, not on the whole workspace, so checks stay inside the
//! "green in under 10 minutes" budget while hundreds of PRs are in flight.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::Result;
use crate::process::capture;
use crate::workspace::{DepKind, Workspace};

/// Root files that configure every crate: touching one means "check everything".
const WORKSPACE_WIDE_FILES: &[&str] = &[
    "Cargo.toml",
    "Cargo.lock",
    "deny.toml",
    "rust-toolchain.toml",
    "rust-toolchain",
    "clippy.toml",
    ".clippy.toml",
    "rustfmt.toml",
    ".rustfmt.toml",
];

/// Root directories that configure every crate (cargo aliases, linker and target settings).
const WORKSPACE_WIDE_DIRS: &[&str] = &[".cargo"];

/// The crate whose tests run the licence-file lint on the repository (`lint_licences`).
const LICENCE_LINT_CRATE: &str = "xtask";

/// CI files outside every crate that run a crate's check, so a change to one alone still brings that crate into
/// scope: `ci.yml`'s WebAssembly link proof runs when `omuzaki-link-probe` is in the lint scope (ADR 0015).
const CI_STEP_CRATES: &[(&str, &str)] = &[(".github/workflows/ci.yml", "omuzaki-link-probe")];

/// The crates that own a changed file, plus `xtask` when a file is an input of the licence lint, plus the crate a
/// changed CI file checks (`CI_STEP_CRATES`).
fn changed_crates<'a>(workspace: &'a Workspace, files: &[PathBuf]) -> BTreeSet<&'a str> {
    let mut changed: BTreeSet<&str> = files.iter().filter_map(|file| owning_crate(workspace, file)).collect();
    if files.iter().any(|file| crate::lint_licences::is_input(file)) {
        changed.extend(workspace.package(LICENCE_LINT_CRATE).map(|package| package.name.as_str()));
    }
    for (ci_file, krate) in CI_STEP_CRATES {
        if files.iter().any(|file| file.as_os_str() == *ci_file) {
            changed.extend(workspace.package(krate).map(|package| package.name.as_str()));
        }
    }
    changed
}

/// What a check run covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Every crate. `trigger` is the changed file that forced it, or `None` when asked for with `--all`.
    Workspace {
        /// The file that widened the scope.
        trigger: Option<PathBuf>,
    },
    /// Exactly these crates (changed ones plus their dependents); both sets empty means nothing to check.
    Crates {
        /// Crates checked in full: fmt, clippy and every test.
        full: BTreeSet<String>,
        /// Crates in scope only because a `link_only` dependency changed (`Package::link_only`): checked for fmt,
        /// clippy and their link gate test, not their other tests. Disjoint from `full`.
        link_only: BTreeSet<String>,
    },
}

/// The label `--list` and the banner append to a crate that gets only its link gate.
const LINK_ONLY_LABEL: &str = "(link gate only)";

impl Scope {
    /// True when there is nothing to check (a docs-only change).
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Crates { full, link_only } if full.is_empty() && link_only.is_empty())
    }

    /// Every crate name in scope, sorted, however much of each is checked.
    pub fn names<'a>(&'a self, workspace: &'a Workspace) -> Vec<&'a str> {
        match self {
            Self::Workspace { .. } => workspace.packages.iter().map(|package| package.name.as_str()).collect(),
            Self::Crates { full, link_only } => full.union(link_only).map(String::as_str).collect(),
        }
    }

    /// [`names`](Self::names) as shown to people: a link-only crate reads `omuzaki-cli (link gate only)`.
    pub fn labels(&self, workspace: &Workspace) -> Vec<String> {
        let link_only = match self {
            Self::Crates { link_only, .. } => Some(link_only),
            Self::Workspace { .. } => None,
        };
        self.names(workspace)
            .into_iter()
            .map(|name| match link_only {
                Some(set) if set.contains(name) => format!("{name} {LINK_ONLY_LABEL}"),
                _ => name.to_owned(),
            })
            .collect()
    }
}

/// A [`Scope`] with the two smaller sets a pull request uses before the merge queue runs the full one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Analysis {
    /// The full scope: changed crates plus all their dependents.
    pub scope: Scope,
    /// Crates that own a changed file (every crate when the scope is the whole workspace).
    pub changed: BTreeSet<String>,
    /// `changed` plus the crates that depend on one of them directly, through any dependency kind.
    pub direct: BTreeSet<String>,
}

/// The scope of the change between `base` and the working tree, with the changed and direct sets (see [`Analysis`]).
pub fn analyse(workspace: &Workspace, base: &str) -> Result<Analysis> {
    Ok(analyse_files(workspace, &changed_files(&workspace.root, base)?))
}

/// The analysis of `--all`: every crate, with no file to blame.
pub fn whole_workspace(workspace: &Workspace) -> Analysis {
    let all: BTreeSet<String> = workspace.packages.iter().map(|package| package.name.clone()).collect();
    Analysis { scope: Scope::Workspace { trigger: None }, changed: all.clone(), direct: all }
}

/// [`analyse`] for an explicit list of changed files (paths relative to the workspace root).
pub fn analyse_files(workspace: &Workspace, files: &[PathBuf]) -> Analysis {
    let scope = scope_for_files(workspace, files);
    if matches!(scope, Scope::Workspace { .. }) {
        return Analysis { scope, ..whole_workspace(workspace) };
    }
    let changed = changed_crates(workspace, files);
    let mut direct: BTreeSet<String> = changed.iter().map(|name| (*name).to_owned()).collect();
    for package in &workspace.packages {
        if package.dependencies.iter().any(|dependency| changed.contains(dependency.name.as_str())) {
            direct.insert(package.name.clone());
        }
    }
    Analysis { scope, changed: changed.into_iter().map(str::to_owned).collect(), direct }
}

/// The scope for an explicit list of changed files (paths relative to the workspace root).
pub fn scope_for_files(workspace: &Workspace, files: &[PathBuf]) -> Scope {
    if let Some(trigger) = files.iter().find(|file| is_workspace_wide(file)) {
        return Scope::Workspace { trigger: Some(trigger.clone()) };
    }
    let changed = changed_crates(workspace, files);
    let (full, link_only) = with_dependents(workspace, &changed);
    Scope::Crates { full, link_only }
}

fn is_workspace_wide(file: &Path) -> bool {
    let at_root = file.components().count() == 1;
    (at_root && WORKSPACE_WIDE_FILES.iter().any(|name| file.as_os_str() == *name))
        || WORKSPACE_WIDE_DIRS.iter().any(|dir| file.starts_with(dir))
}

/// The crate whose directory contains `file`; the deepest one wins if directories nest.
fn owning_crate<'a>(workspace: &'a Workspace, file: &Path) -> Option<&'a str> {
    workspace
        .packages
        .iter()
        .filter(|package| file.starts_with(&package.dir))
        .max_by_key(|package| package.dir.components().count())
        .map(|package| package.name.as_str())
}

/// How a crate was reached from a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Only through a dependency its manifest marks `link_only`.
    LinkOnly,
    /// Changed itself, or depends on a changed crate in a way that can alter its behaviour.
    Full,
}

/// `seeds` plus every crate that depends on one of them, directly or not, through any dependency kind (a changed mock
/// can break the tests of the crates that dev-depend on it), split into `(full, link_only)`.
///
/// A crate is `link_only` when every route to it from a change ends in a dependency it lists in `link_only`; one normal
/// route (or a change to the crate itself) makes it `full`. Dependents of a link-only crate are still reached, as
/// `full` unless they are link-only dependents themselves, because this reads the manifests, not what the crate exports.
fn with_dependents(workspace: &Workspace, seeds: &BTreeSet<&str>) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut dependents: BTreeMap<&str, Vec<(&str, Reach)>> = BTreeMap::new();
    for package in &workspace.packages {
        for dependency in &package.dependencies {
            let held_for_linking = dependency.kind == DepKind::Normal && package.link_only.contains(&dependency.name);
            let reach = if held_for_linking { Reach::LinkOnly } else { Reach::Full };
            dependents.entry(dependency.name.as_str()).or_default().push((package.name.as_str(), reach));
        }
    }
    let mut reached: BTreeMap<&str, Reach> = seeds.iter().map(|name| (*name, Reach::Full)).collect();
    let mut pending: Vec<&str> = seeds.iter().copied().collect();
    while let Some(name) = pending.pop() {
        for (dependent, reach) in dependents.get(name).into_iter().flatten() {
            let upgraded = match reached.get(dependent) {
                None => true,
                Some(Reach::LinkOnly) => *reach == Reach::Full,
                Some(Reach::Full) => false,
            };
            if upgraded {
                reached.insert(dependent, *reach);
                pending.push(dependent);
            }
        }
    }
    let of_kind =
        |wanted| reached.iter().filter(|(_, kind)| **kind == wanted).map(|(name, _)| (*name).to_owned()).collect();
    (of_kind(Reach::Full), of_kind(Reach::LinkOnly))
}

/// Files that differ from the merge-base of `HEAD` and `base`: committed, staged, unstaged and untracked, so a check
/// before committing sees the same change a PR will. Renames count as a delete plus an add, so both crates are hit.
fn changed_files(root: &Path, base: &str) -> Result<Vec<PathBuf>> {
    let merge_base = git(root, &["merge-base", "HEAD", base]).map_err(|error| hint_fetch(error, base))?;
    let merge_base = String::from_utf8_lossy(&merge_base).trim().to_owned();
    let tracked = git(root, &["diff", "--name-only", "--no-renames", "-z", "--relative", &merge_base])?;
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
    Ok([tracked, untracked].iter().flat_map(|list| nul_separated_paths(list)).collect())
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    capture(Command::new("git").arg("-C").arg(root).args(args))
}

fn hint_fetch(error: crate::error::Error, base: &str) -> crate::error::Error {
    match error {
        crate::error::Error::Failed { command, stderr } => crate::error::Error::Failed {
            command,
            stderr: format!("{stderr} (is `{base}` fetched? try `git fetch origin`, or pass --base <ref>)"),
        },
        other => other,
    }
}

fn nul_separated_paths(list: &[u8]) -> impl Iterator<Item = PathBuf> + '_ {
    list.split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| PathBuf::from(String::from_utf8_lossy(path).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::DepKind::{Dev, Normal};
    use crate::workspace::testing::workspace;

    /// core <- doc-api <- doc-mock; render and audio implement against doc-api; cli wires render and audio.
    fn sample() -> Workspace {
        workspace(&[
            ("omuzaki-core", &[]),
            ("omuzaki-doc-api", &[("omuzaki-core", Normal)]),
            ("omuzaki-doc-mock", &[("omuzaki-doc-api", Normal)]),
            ("omuzaki-render", &[("omuzaki-doc-api", Normal), ("omuzaki-doc-mock", Dev)]),
            ("omuzaki-audio", &[("omuzaki-doc-api", Normal)]),
            ("omuzaki-cli", &[("omuzaki-render", Normal), ("omuzaki-audio", Normal)]),
            ("xtask", &[]),
        ])
    }

    fn files(paths: &[&str]) -> Vec<PathBuf> {
        paths.iter().map(PathBuf::from).collect()
    }

    fn crates(scope: &Scope) -> Vec<&str> {
        match scope {
            Scope::Crates { full, link_only } => {
                assert!(link_only.is_empty(), "unexpected link-only crates: {link_only:?}");
                full.iter().map(String::as_str).collect()
            }
            Scope::Workspace { .. } => panic!("expected a crate list, got {scope:?}"),
        }
    }

    /// `sample` where `omuzaki-cli` holds `omuzaki-render` and `omuzaki-audio` only to link them.
    fn linking_sample() -> Workspace {
        crate::workspace::testing::with_link_only(sample(), "omuzaki-cli", &["omuzaki-render", "omuzaki-audio"])
    }

    fn add_dependency(ws: &mut Workspace, package: &str, name: &str, kind: DepKind) {
        if let Some(package) = ws.packages.iter_mut().find(|candidate| candidate.name == package) {
            package.dependencies.push(crate::workspace::Dependency { name: name.to_owned(), kind });
        }
    }

    fn split(scope: &Scope) -> (Vec<&str>, Vec<&str>) {
        match scope {
            Scope::Crates { full, link_only } => {
                (full.iter().map(String::as_str).collect(), link_only.iter().map(String::as_str).collect())
            }
            Scope::Workspace { .. } => panic!("expected a crate list, got {scope:?}"),
        }
    }

    #[test]
    fn docs_only_changes_affect_nothing() {
        let scope =
            scope_for_files(&sample(), &files(&["docs/README.md", "tools/run.sh", ".changes/x.md", "README.md"]));
        assert!(scope.is_empty());
    }

    #[test]
    fn a_leaf_change_hits_the_crate_and_its_dependents() {
        let scope = scope_for_files(&sample(), &files(&["crates/omuzaki-render/src/lib.rs"]));
        assert_eq!(crates(&scope), ["omuzaki-cli", "omuzaki-render"]);
    }

    #[test]
    fn a_contract_change_hits_everything_built_on_it() {
        let scope = scope_for_files(&sample(), &files(&["crates/omuzaki-doc-api/src/lib.rs"]));
        assert_eq!(
            crates(&scope),
            ["omuzaki-audio", "omuzaki-cli", "omuzaki-doc-api", "omuzaki-doc-mock", "omuzaki-render"]
        );
    }

    #[test]
    fn dev_dependents_are_affected_too() {
        let scope = scope_for_files(&sample(), &files(&["crates/omuzaki-doc-mock/Cargo.toml"]));
        assert_eq!(crates(&scope), ["omuzaki-cli", "omuzaki-doc-mock", "omuzaki-render"]);
    }

    #[test]
    fn a_change_behind_a_link_only_dependency_gives_the_dependent_just_its_link_gate() {
        let scope = scope_for_files(&linking_sample(), &files(&["crates/omuzaki-render/src/lib.rs"]));
        assert_eq!(split(&scope), (vec!["omuzaki-render"], vec!["omuzaki-cli"]));
    }

    #[test]
    fn a_change_deep_below_a_link_only_dependency_still_only_links() {
        let scope = scope_for_files(&linking_sample(), &files(&["crates/omuzaki-doc-mock/src/lib.rs"]));
        assert_eq!(split(&scope), (vec!["omuzaki-doc-mock", "omuzaki-render"], vec!["omuzaki-cli"]));
    }

    #[test]
    fn a_contract_change_runs_the_dependent_in_full_even_when_it_also_links() {
        // doc-api reaches cli through render (link-only) and audio (link-only) only; make the contract a normal dep.
        let mut ws = linking_sample();
        add_dependency(&mut ws, "omuzaki-cli", "omuzaki-doc-api", Normal);
        let scope = scope_for_files(&ws, &files(&["crates/omuzaki-doc-api/src/lib.rs"]));
        assert_eq!(
            crates(&scope),
            ["omuzaki-audio", "omuzaki-cli", "omuzaki-doc-api", "omuzaki-doc-mock", "omuzaki-render"]
        );
    }

    #[test]
    fn a_change_to_the_linking_crate_itself_runs_in_full() {
        let scope = scope_for_files(&linking_sample(), &files(&["crates/omuzaki-cli/src/lib.rs"]));
        assert_eq!(crates(&scope), ["omuzaki-cli"]);
    }

    #[test]
    fn one_link_only_and_one_normal_route_make_a_full_run_whatever_the_order() {
        let mut ws = linking_sample();
        add_dependency(&mut ws, "omuzaki-cli", "omuzaki-audio", Dev);
        let both =
            scope_for_files(&ws, &files(&["crates/omuzaki-audio/src/lib.rs", "crates/omuzaki-render/src/lib.rs"]));
        assert_eq!(crates(&both), ["omuzaki-audio", "omuzaki-cli", "omuzaki-render"]);
        let normal_only = scope_for_files(&ws, &files(&["crates/omuzaki-audio/src/lib.rs"]));
        assert_eq!(crates(&normal_only), ["omuzaki-audio", "omuzaki-cli"]);
    }

    #[test]
    fn dependents_of_a_link_only_crate_are_still_reached_in_full() {
        let mut ws = crate::workspace::testing::workspace(&[
            ("omuzaki-op", &[]),
            ("omuzaki-cli", &[("omuzaki-op", Normal)]),
            ("omuzaki-tool", &[("omuzaki-cli", Normal)]),
        ]);
        ws = crate::workspace::testing::with_link_only(ws, "omuzaki-cli", &["omuzaki-op"]);
        let scope = scope_for_files(&ws, &files(&["crates/omuzaki-op/src/lib.rs"]));
        assert_eq!(split(&scope), (vec!["omuzaki-op", "omuzaki-tool"], vec!["omuzaki-cli"]));
    }

    #[test]
    fn labels_mark_the_link_only_crates() {
        let ws = linking_sample();
        let scope = scope_for_files(&ws, &files(&["crates/omuzaki-audio/src/lib.rs"]));
        assert_eq!(scope.labels(&ws), ["omuzaki-audio", "omuzaki-cli (link gate only)"]);
        assert_eq!(scope.names(&ws), ["omuzaki-audio", "omuzaki-cli"]);
    }

    #[test]
    fn xtask_changes_stay_in_xtask() {
        assert_eq!(crates(&scope_for_files(&sample(), &files(&["xtask/src/main.rs"]))), ["xtask"]);
    }

    #[test]
    fn a_licence_file_change_runs_the_xtask_tests_that_lint_it() {
        let scope = scope_for_files(&sample(), &files(&["docs/licensing/deps/serde.toml", "docs/licensing-notes.md"]));
        assert_eq!(crates(&scope), ["xtask"]);
    }

    #[test]
    fn licence_lint_inputs_outside_any_crate_run_the_xtask_tests() {
        for input in [".github/assets/new.png", ".github/workflows/ci.yml", "docs/licensing/policy.md"] {
            let analysis = analyse_files(&sample(), &files(&[input]));
            assert_eq!(crates(&analysis.scope), ["xtask"], "{input}");
            assert_eq!(analysis.changed, BTreeSet::from(["xtask".to_owned()]), "{input}");
            assert_eq!(analysis.direct, BTreeSet::from(["xtask".to_owned()]), "{input}");
        }
    }

    #[test]
    fn every_asset_type_the_lint_scans_schedules_xtask() {
        for ext in crate::lint_licences::scanned_extensions() {
            let input = format!("crates/omuzaki-render/assets/x.{ext}");
            let analysis = analyse_files(&sample(), &files(&[input.as_str()]));
            assert!(analysis.changed.contains("xtask"), "{input}");
        }
    }

    #[test]
    fn a_font_or_notices_file_keeps_its_crate_and_adds_xtask() {
        for input in ["crates/omuzaki-render/assets/a.ttf", "crates/omuzaki-render/NOTICES.md"] {
            let analysis = analyse_files(&sample(), &files(&[input]));
            assert_eq!(crates(&analysis.scope), ["omuzaki-cli", "omuzaki-render", "xtask"], "{input}");
            assert_eq!(analysis.changed, BTreeSet::from(["omuzaki-render".to_owned(), "xtask".to_owned()]), "{input}");
            assert!(analysis.direct.contains("xtask") && analysis.direct.contains("omuzaki-cli"), "{input}");
        }
    }

    #[test]
    fn a_ci_workflow_change_brings_in_the_crate_its_step_checks() {
        let ws =
            workspace(&[("omuzaki-core", &[]), ("omuzaki-link-probe", &[("omuzaki-core", Normal)]), ("xtask", &[])]);
        let analysis = analyse_files(&ws, &files(&[".github/workflows/ci.yml"]));
        assert!(analysis.direct.contains("omuzaki-link-probe"), "{:?}", analysis.direct);
        let other = analyse_files(&ws, &files(&[".github/workflows/nightly.yml"]));
        assert!(!other.direct.contains("omuzaki-link-probe"), "{:?}", other.direct);
        // A workspace without the probe is unaffected.
        assert!(!analyse_files(&sample(), &files(&[".github/workflows/ci.yml"])).direct.contains("omuzaki-link-probe"));
    }

    #[test]
    fn ordinary_crate_files_do_not_pull_in_xtask() {
        let analysis = analyse_files(&sample(), &files(&["crates/omuzaki-render/src/lib.rs"]));
        assert!(!analysis.changed.contains("xtask") && !analysis.direct.contains("xtask"));
    }

    #[test]
    fn a_directory_that_merely_shares_a_prefix_is_not_the_crate() {
        let scope = scope_for_files(&sample(), &files(&["crates/omuzaki-core-extras/src/lib.rs"]));
        assert!(scope.is_empty());
    }

    #[test]
    fn root_config_widens_to_the_whole_workspace() {
        for trigger in [
            "Cargo.toml",
            "Cargo.lock",
            "deny.toml",
            "rust-toolchain.toml",
            "clippy.toml",
            "rustfmt.toml",
            ".cargo/config.toml",
        ] {
            let scope = scope_for_files(&sample(), &files(&["docs/a.md", trigger]));
            assert_eq!(scope, Scope::Workspace { trigger: Some(PathBuf::from(trigger)) }, "{trigger}");
        }
    }

    #[test]
    fn same_named_files_inside_a_crate_do_not_widen() {
        let scope = scope_for_files(
            &sample(),
            &files(&["crates/omuzaki-audio/Cargo.toml", "crates/omuzaki-audio/clippy.toml"]),
        );
        assert_eq!(crates(&scope), ["omuzaki-audio", "omuzaki-cli"]);
    }

    #[test]
    fn nested_crates_resolve_to_the_deepest_owner() {
        let mut ws = sample();
        ws.packages.push(crate::workspace::Package {
            name: "omuzaki-render-inner".into(),
            dir: PathBuf::from("crates/omuzaki-render/inner"),
            dependencies: vec![],
            layer: None,
            link_only: vec![],
            wasm: true,
        });
        let scope = scope_for_files(&ws, &files(&["crates/omuzaki-render/inner/src/lib.rs"]));
        assert_eq!(crates(&scope), ["omuzaki-render-inner"]);
    }

    #[test]
    fn names_lists_the_whole_workspace_for_the_wide_scope() {
        let ws = sample();
        let all = Scope::Workspace { trigger: None };
        assert_eq!(all.names(&ws).len(), ws.packages.len());
        assert!(!all.is_empty());
    }

    #[test]
    fn splits_git_z_output() {
        let paths: Vec<PathBuf> = nul_separated_paths(b"a/b.rs\0c.md\0").collect();
        assert_eq!(paths, files(&["a/b.rs", "c.md"]));
        assert_eq!(nul_separated_paths(b"").count(), 0);
    }
}
