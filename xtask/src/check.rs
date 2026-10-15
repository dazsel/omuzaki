//! `cargo xtask check`: the gates CI runs, scoped to what a change can break (spec v3 §39 rule 9).
//!
//! An agent proves its PR is green with `check --affected` before opening it; CI runs the same steps. Every step runs
//! with plain `cargo`, sharing one target directory so clippy and test reuse each other's compiled dependencies.

use std::process::{Command, ExitCode};
use std::time::{Duration, Instant};

use clap::Args;

use crate::affected::{self, Scope};
use crate::crate_table;
use crate::error::Result;
use crate::links;
use crate::lint_deps;
use crate::lint_seed;
use crate::lint_ui_writes;
use crate::process::{cargo, describe};
use crate::report::Report;
use crate::workspace::Workspace;

/// Options of `cargo xtask check`.
#[derive(Debug, Args)]
pub struct CheckArgs {
    /// Check the crates changed since the base plus everything that depends on them (the default mode).
    #[arg(long, conflicts_with = "all")]
    affected: bool,
    /// Check the whole workspace.
    #[arg(long)]
    all: bool,
    /// Print the crates that would be checked, one per line, and stop.
    #[arg(long)]
    list: bool,
    /// Print the scope in a machine-readable form and stop (implies `--list`). `json` is one JSON object (crate sets
    /// and test shards, see `report.rs`), which is what the CI plan job reads.
    #[arg(long, value_enum)]
    format: Option<Format>,
    /// With `--format json`, the most shards a test set is split into.
    #[arg(long, value_name = "N", default_value_t = 4)]
    shards: usize,
    /// The branch or commit the change is measured against (its merge-base with HEAD is used).
    #[arg(long, value_name = "REF", default_value = "origin/main")]
    base: String,
}

impl CheckArgs {
    fn whole_workspace(&self) -> bool {
        // `--affected` is the default; it exists so command lines read as what they mean.
        self.all && !self.affected
    }
}

/// A machine-readable output of `check --format`.
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Format {
    /// One JSON object on one line.
    Json,
}

/// One thing `check` runs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Step {
    name: &'static str,
    action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    /// A cargo subcommand and its arguments.
    Cargo(Vec<String>),
    /// The layering lint, run in this process.
    LintDeps,
    /// The `link()` wiring lint (ADR 0015), run in this process.
    LintLinks,
    /// The UI-writes lint (QG-11), run in this process.
    LintUiWrites,
    /// The crate-table lint (ADR 0053), run in this process.
    LintCrateTable,
    /// The seed lint (library D15, D16), run in this process when a file under `crates/*/library/` changed.
    LintSeed,
}

impl Step {
    fn cargo(name: &'static str, args: &[&str], selector: &[String], trailing: &[&str]) -> Self {
        let args = [args, &selector.iter().map(String::as_str).collect::<Vec<_>>(), trailing]
            .concat()
            .into_iter()
            .map(str::to_owned)
            .collect();
        Self { name, action: Action::Cargo(args) }
    }

    fn describe(&self) -> String {
        match &self.action {
            Action::Cargo(args) => format!("cargo {}", args.join(" ")),
            Action::LintDeps => "lint-deps".to_owned(),
            Action::LintLinks => "lint-links".to_owned(),
            Action::LintUiWrites => "lint-ui-writes".to_owned(),
            Action::LintCrateTable => "lint-crate-table".to_owned(),
            Action::LintSeed => "lint-seed".to_owned(),
        }
    }
}

/// The cargo feature that builds the `omuzaki-fixture` demo binary and compiles its end-to-end tests (`omuzaki-cli`'s
/// Cargo.toml). `cargo install` leaves it off, so the fixture never ships; the gates turn it on so the tests still run.
/// Cargo rejects a `package/feature` naming a package that isn't selected, so it is passed only when omuzaki-cli is.
const FIXTURE_FEATURE: &str = "omuzaki-cli/test-fixtures";

/// The integration test every linking App has for its link gate (ADR 0015, `tests/registrations.rs`). It is all a
/// crate in scope only through a `link_only` dependency runs of its tests.
const LINK_GATE_TEST: &str = "registrations";

/// The steps for `scope`, cheapest first, so the common mistakes fail early on screen.
///
/// `cargo deny` skips advisories: they come from a database that changes daily, which would make a check's result
/// depend on the date rather than on the code (the licence, ban and source rules are what the gate is for). It runs at
/// log level `error` because, until the allow-list has all its users, it prints dozens of unused-allowance warnings
/// that would bury the real output; every denial is still an error and still fails the step.
fn plan(scope: &Scope) -> Vec<Step> {
    let selected = Selected::of(scope);
    let mut steps = vec![
        Step::cargo("fmt", &["fmt", "--check"], &selected.fmt, &[]),
        Step::cargo("clippy", &["clippy"], &selected.clippy, &["--all-targets", "--", "-D", "warnings"]),
        Step::cargo("test", &["test"], &selected.test, &[]),
    ];
    // Building and running the gate test links the whole binary, which is what a changed op crate can break.
    steps.extend(selected.link_gates.iter().map(|package| {
        Step::cargo("link-gate", &["test"], &["-p".to_owned(), package.clone()], &["--test", LINK_GATE_TEST])
    }));
    steps.extend([
        Step::cargo("deny", &["deny", "--log-level", "error", "check", "licenses", "bans", "sources"], &[], &[]),
        Step { name: "lint-deps", action: Action::LintDeps },
        Step { name: "lint-links", action: Action::LintLinks },
        Step { name: "lint-ui-writes", action: Action::LintUiWrites },
        Step { name: "lint-crate-table", action: Action::LintCrateTable },
        Step { name: "lint-seed", action: Action::LintSeed },
    ]);
    steps
}

/// The cargo package selectors each step of a [`Scope`] uses.
struct Selected {
    /// For `cargo fmt`: every crate in scope.
    fmt: Vec<String>,
    /// For `cargo clippy`: every crate in scope, link-only ones included.
    clippy: Vec<String>,
    /// For `cargo test`: the crates checked in full.
    test: Vec<String>,
    /// The crates that run only their link gate test, one step each (`-p` cannot be combined with `--test`).
    link_gates: Vec<String>,
}

impl Selected {
    fn of(scope: &Scope) -> Self {
        match scope {
            Scope::Workspace { .. } => {
                let all = vec!["--workspace".to_owned(), "--features".to_owned(), FIXTURE_FEATURE.to_owned()];
                Self { fmt: vec!["--all".into()], clippy: all.clone(), test: all, link_gates: Vec::new() }
            }
            Scope::Crates { full, link_only } => {
                let flags = |names: &mut dyn Iterator<Item = &String>| -> Vec<String> {
                    names.flat_map(|name| ["-p".to_owned(), name.clone()]).collect()
                };
                let fixtures = |mut select: Vec<String>| {
                    if full.contains("omuzaki-cli") {
                        select.extend(["--features".to_owned(), FIXTURE_FEATURE.to_owned()]);
                    }
                    select
                };
                let everything = flags(&mut full.iter().chain(link_only));
                Self {
                    clippy: fixtures(everything.clone()),
                    fmt: everything,
                    test: fixtures(flags(&mut full.iter())),
                    link_gates: link_only.iter().cloned().collect(),
                }
            }
        }
    }
}

/// How one step went.
#[derive(Debug, Clone, PartialEq, Eq)]
struct StepResult {
    name: &'static str,
    passed: bool,
    elapsed: Duration,
}

/// Runs every step in order, timing each. A failure doesn't stop the run: one pass shows every problem, which is
/// cheaper than one slow round trip per problem.
fn execute(plan: &[Step], mut run: impl FnMut(&Step) -> bool) -> Vec<StepResult> {
    plan.iter()
        .map(|step| {
            let started = Instant::now();
            let passed = run(step);
            StepResult { name: step.name, passed, elapsed: started.elapsed() }
        })
        .collect()
}

/// The closing screen: one line per step, then the verdict.
fn summary(results: &[StepResult]) -> String {
    let mut lines = vec!["check summary".to_owned()];
    for result in results {
        let verdict = if result.passed { "ok" } else { "FAILED" };
        lines.push(format!("  {verdict:<7} {:<10} {:>6.1}s", result.name, result.elapsed.as_secs_f64()));
    }
    let failed: Vec<&str> = results.iter().filter(|result| !result.passed).map(|result| result.name).collect();
    let total: Duration = results.iter().map(|result| result.elapsed).sum();
    lines.push(if failed.is_empty() {
        format!("check: PASSED in {:.1}s", total.as_secs_f64())
    } else {
        format!("check: FAILED ({}) in {:.1}s", failed.join(", "), total.as_secs_f64())
    });
    lines.join("\n")
}

/// One line saying what is being checked and why.
fn banner(scope: &Scope, workspace: &Workspace, base: &str) -> String {
    match scope {
        Scope::Workspace { trigger: None } => "check: whole workspace (--all)".to_owned(),
        Scope::Workspace { trigger: Some(file) } => {
            format!("check: whole workspace ({} changed since {base})", file.display())
        }
        Scope::Crates { .. } if scope.is_empty() => format!("check: no crate affected since {base}"),
        Scope::Crates { .. } => {
            let labels = scope.labels(workspace);
            format!("check: {} affected crate(s) since {base}: {}", labels.len(), labels.join(", "))
        }
    }
}

/// The `check` command.
pub fn run(workspace: &Workspace, args: &CheckArgs) -> Result<ExitCode> {
    let analysis = if args.whole_workspace() {
        affected::whole_workspace(workspace)
    } else {
        affected::analyse(workspace, &args.base)?
    };
    let scope = analysis.scope.clone();
    if args.format.is_some() {
        eprintln!("{}", banner(&scope, workspace, &args.base));
        println!("{}", Report::new(workspace, &analysis, args.shards).to_json());
        return Ok(ExitCode::SUCCESS);
    }
    if args.list {
        // stdout stays plain package names for scripts (`tools/ci/pr-check.sh` passes each line to `cargo -p`); the
        // reason for a wide scope and the link-only marks go to stderr with the banner.
        eprintln!("{}", banner(&scope, workspace, &args.base));
        for name in scope.names(workspace) {
            println!("{name}");
        }
        return Ok(ExitCode::SUCCESS);
    }
    println!("{}", banner(&scope, workspace, &args.base));
    if scope.is_empty() {
        println!("check: nothing to run");
        return Ok(ExitCode::SUCCESS);
    }
    let results = execute(&plan(&scope), |step| run_step(workspace, step, &args.base));
    println!("{}", summary(&results));
    Ok(if results.iter().all(|result| result.passed) { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// Runs a step with its output going straight to the terminal; returns whether it passed.
fn run_step(workspace: &Workspace, step: &Step, base: &str) -> bool {
    println!("==> {}: {}", step.name, step.describe());
    match &step.action {
        Action::LintDeps => lint_deps::report(workspace),
        Action::LintLinks => links::report(workspace).unwrap_or_else(|error| {
            eprintln!("lint-links: {error}");
            false
        }),
        Action::LintUiWrites => lint_ui_writes::report(workspace).unwrap_or_else(|error| {
            eprintln!("lint-ui-writes: {error}");
            false
        }),
        Action::LintCrateTable => crate_table::lint(workspace).map_or_else(
            |error| {
                eprintln!("lint-crate-table: {error}");
                false
            },
            |code| code == ExitCode::SUCCESS,
        ),
        Action::LintSeed => lint_seed::check(workspace, base),
        Action::Cargo(args) => {
            let mut command = Command::new(cargo());
            command.args(args).current_dir(&workspace.root);
            match command.status() {
                Ok(status) => status.success(),
                Err(error) => {
                    eprintln!("cannot run `{}`: {error}", describe(&command));
                    false
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn crates(names: &[&str]) -> Scope {
        link_only(names, &[])
    }

    fn link_only(full: &[&str], link_only: &[&str]) -> Scope {
        let set = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect::<BTreeSet<_>>();
        Scope::Crates { full: set(full), link_only: set(link_only) }
    }

    fn cargo_args(step: &Step) -> String {
        match &step.action {
            Action::Cargo(args) => args.join(" "),
            Action::LintDeps => "<lint-deps>".to_owned(),
            Action::LintLinks => "<lint-links>".to_owned(),
            Action::LintUiWrites => "<lint-ui-writes>".to_owned(),
            Action::LintCrateTable => "<lint-crate-table>".to_owned(),
            Action::LintSeed => "<lint-seed>".to_owned(),
        }
    }

    #[test]
    fn the_plan_for_affected_crates_is_scoped_and_ordered() {
        let steps = plan(&crates(&["omuzaki-cli", "omuzaki-render"]));
        let got: Vec<(&str, String)> = steps.iter().map(|step| (step.name, cargo_args(step))).collect();
        assert_eq!(
            got,
            [
                ("fmt", "fmt --check -p omuzaki-cli -p omuzaki-render".to_owned()),
                (
                    "clippy",
                    "clippy -p omuzaki-cli -p omuzaki-render --features omuzaki-cli/test-fixtures --all-targets -- -D warnings"
                        .to_owned(),
                ),
                ("test", "test -p omuzaki-cli -p omuzaki-render --features omuzaki-cli/test-fixtures".to_owned()),
                ("deny", "deny --log-level error check licenses bans sources".to_owned()),
                ("lint-deps", "<lint-deps>".to_owned()),
                ("lint-links", "<lint-links>".to_owned()),
                ("lint-ui-writes", "<lint-ui-writes>".to_owned()),
                ("lint-crate-table", "<lint-crate-table>".to_owned()),
                ("lint-seed", "<lint-seed>".to_owned()),
            ]
        );
    }

    #[test]
    fn a_link_only_crate_gets_fmt_clippy_and_just_its_link_gate_test() {
        let steps = plan(&link_only(&["omuzaki-io-ops"], &["omuzaki-cli"]));
        let got: Vec<(&str, String)> = steps.iter().map(|step| (step.name, cargo_args(step))).collect();
        assert_eq!(
            got[..4],
            [
                ("fmt", "fmt --check -p omuzaki-io-ops -p omuzaki-cli".to_owned()),
                ("clippy", "clippy -p omuzaki-io-ops -p omuzaki-cli --all-targets -- -D warnings".to_owned()),
                ("test", "test -p omuzaki-io-ops".to_owned()),
                ("link-gate", "test -p omuzaki-cli --test registrations".to_owned()),
            ]
        );
        assert_eq!(got.len(), 10, "the same trailing steps as ever: {got:?}");
    }

    #[test]
    fn the_fixture_feature_is_enabled_only_where_omuzaki_cli_is_checked() {
        let steps = plan(&crates(&["omuzaki-core"]));
        assert_eq!(cargo_args(&steps[1]), "clippy -p omuzaki-core --all-targets -- -D warnings");
        assert_eq!(cargo_args(&steps[2]), "test -p omuzaki-core");
    }

    #[test]
    fn the_plan_for_the_whole_workspace_uses_workspace_selectors() {
        let steps = plan(&Scope::Workspace { trigger: None });
        assert_eq!(cargo_args(&steps[0]), "fmt --check --all");
        assert_eq!(
            cargo_args(&steps[1]),
            "clippy --workspace --features omuzaki-cli/test-fixtures --all-targets -- -D warnings"
        );
        assert_eq!(cargo_args(&steps[2]), "test --workspace --features omuzaki-cli/test-fixtures");
    }

    #[test]
    fn a_failing_step_does_not_hide_the_ones_after_it() {
        let steps = plan(&crates(&["omuzaki-core"]));
        let results = execute(&steps, |step| step.name != "clippy");
        let names: Vec<&str> = results.iter().map(|result| result.name).collect();
        assert_eq!(
            names,
            [
                "fmt",
                "clippy",
                "test",
                "deny",
                "lint-deps",
                "lint-links",
                "lint-ui-writes",
                "lint-crate-table",
                "lint-seed"
            ]
        );
        assert_eq!(results.iter().filter(|result| !result.passed).count(), 1);
    }

    #[test]
    fn the_summary_names_every_step_and_the_failures() {
        let result = |name, passed| StepResult { name, passed, elapsed: Duration::from_millis(1500) };
        let text = summary(&[result("fmt", true), result("clippy", false), result("test", false)]);
        assert!(text.contains("ok      fmt"), "{text}");
        assert!(text.contains("FAILED  clippy"), "{text}");
        assert!(text.ends_with("check: FAILED (clippy, test) in 4.5s"), "{text}");
        let all_ok = summary(&[result("fmt", true)]);
        assert!(all_ok.ends_with("check: PASSED in 1.5s"), "{all_ok}");
    }

    #[test]
    fn every_list_line_is_a_workspace_package_name() {
        let ws = crate::workspace::testing::workspace(&[("omuzaki-cli", &[]), ("omuzaki-io-ops", &[])]);
        for scope in [link_only(&["omuzaki-io-ops"], &["omuzaki-cli"]), Scope::Workspace { trigger: None }] {
            for name in scope.names(&ws) {
                assert!(ws.package(name).is_some(), "`{name}` is not a package: `cargo -p` would reject it");
            }
        }
    }

    #[test]
    fn the_banner_says_why_the_scope_is_wide() {
        let ws = crate::workspace::testing::workspace(&[("omuzaki-core", &[])]);
        let wide = Scope::Workspace { trigger: Some("Cargo.lock".into()) };
        assert_eq!(banner(&wide, &ws, "origin/main"), "check: whole workspace (Cargo.lock changed since origin/main)");
        assert_eq!(
            banner(&crates(&["omuzaki-core"]), &ws, "origin/main"),
            "check: 1 affected crate(s) since origin/main: omuzaki-core"
        );
        assert_eq!(banner(&crates(&[]), &ws, "origin/main"), "check: no crate affected since origin/main");
        let ws = crate::workspace::testing::workspace(&[("omuzaki-cli", &[]), ("omuzaki-io-ops", &[])]);
        assert_eq!(
            banner(&link_only(&["omuzaki-io-ops"], &["omuzaki-cli"]), &ws, "origin/main"),
            "check: 2 affected crate(s) since origin/main: omuzaki-cli (link gate only), omuzaki-io-ops"
        );
    }
}
