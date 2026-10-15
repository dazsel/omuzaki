//! `cargo xtask lint-licences`: one licence file per dependency, tool and asset (`docs/licensing/deps/<name>.toml`),
//! and the register page generated from them (ADR 0053 §3).
//!
//! A pull request that adds a dependency adds a file instead of editing a shared table. The lint fails when something
//! in use has no file: a direct dependency of a workspace crate (`Cargo.lock`), a tool a workflow installs, a committed
//! font or `NOTICES.md`. It also fails on an orphaned file, one that claims a crate, tool or path that is gone.

pub(crate) mod register;
mod usage;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::ExitCode;

use clap::Args;

use crate::workspace::Workspace;

/// The licence files, relative to the workspace root.
const DEPS_DIR: &str = "docs/licensing/deps";
/// All licensing material: the files, the register and the policy.
const LICENSING_DIR: &str = "docs/licensing";
/// The workflows whose `tools:` inputs the lint reads.
const WORKFLOWS_DIR: &str = ".github/workflows";
/// The register page: the prose and one marker per section table.
const REGISTER: &str = "docs/licensing/register.md";

/// Options of `cargo xtask lint-licences`.
#[derive(Debug, Args)]
pub struct LintLicencesArgs {
    /// Print the generated register to stdout (problems go to stderr).
    #[arg(long)]
    print: bool,
}

/// Everything wrong, one message per line, and the generated register.
fn lint(workspace: &Workspace) -> (Vec<String>, String, usize) {
    let root = workspace.root.as_path();
    let mut found = Vec::new();
    let (entries, broken) = register::load(&root.join(DEPS_DIR));
    found.extend(broken.into_iter().map(|(name, message)| format!("{DEPS_DIR}/{name}.toml: cannot read: {message}")));
    let page = fs::read_to_string(root.join(REGISTER)).unwrap_or_else(|error| {
        found.push(format!("{REGISTER}: cannot read: {error}"));
        String::new()
    });
    let sections: Vec<&str> = page.lines().filter_map(register::marker).collect();
    let members = members(workspace, &mut found);
    let lock = fs::read_to_string(root.join("Cargo.lock"))
        .map_err(|e| e.to_string())
        .and_then(|t| usage::Lock::parse(&t, &members));
    let lock = lock.unwrap_or_else(|message| {
        found.push(format!("Cargo.lock: cannot read: {message}"));
        usage::Lock::default()
    });
    let tools = usage::workflow_tools(root);

    let mut claims: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, entry) in &entries {
        let at = format!("{DEPS_DIR}/{name}.toml");
        found.extend(entry.problems().into_iter().map(|problem| format!("{at}: {problem}")));
        if !sections.contains(&entry.section.as_str()) {
            found.push(format!("{at}: section `{}` has no `<!-- licence-table: {0} -->` in {REGISTER}", entry.section));
        }
        for krate in &entry.crates {
            claims.entry(krate).or_default().push(name);
            if !lock.direct.contains_key(krate) {
                let why =
                    if lock.packages.contains(krate) { "only a transitive dependency" } else { "not in Cargo.lock" };
                found
                    .push(format!("{at}: orphaned: crate `{krate}` is no workspace crate's direct dependency ({why})"));
            }
        }
        for tool in entry.tools.iter().filter(|tool| !tools.contains(*tool)) {
            found.push(format!("{at}: orphaned: no workflow installs tool `{tool}`"));
        }
        for path in entry.paths.iter().filter(|path| !root.join(path).exists()) {
            found.push(format!("{at}: orphaned: path `{path}` does not exist"));
        }
    }
    for (krate, users) in &lock.direct {
        match claims.get(krate.as_str()).map(Vec::as_slice) {
            None | Some([]) => {
                let users: Vec<&str> = users.iter().map(String::as_str).collect();
                found.push(format!("crate `{krate}` (used by {}) has no file in {DEPS_DIR}/", users.join(", ")));
            }
            Some([_]) => {}
            Some(files) => {
                found.push(format!("crate `{krate}` is claimed by more than one file: {}", files.join(", ")));
            }
        }
    }
    for tool in tools.iter().filter(|tool| !entries.iter().any(|(_, entry)| entry.tools.contains(*tool))) {
        found.push(format!("tool `{tool}` (installed by a workflow) has no file in {DEPS_DIR}/"));
    }
    for asset in usage::assets(root).iter().filter(|asset| !entries.iter().any(|(_, e)| e.covers_path(asset))) {
        found.push(format!("`{asset}` has no file in {DEPS_DIR}/ (list it under `paths`)"));
    }
    let generated = register::render(&page, &entries);
    (found, generated, entries.len())
}

/// The workspace's crates as `(name, version)`, which is how the lockfile tells a member from an external package of
/// the same name. The version comes from the member's manifest (`version.workspace = true` reads the root's
/// `[workspace.package]`); one that cannot be read is a problem, and that member matches nothing.
fn members(workspace: &Workspace, found: &mut Vec<String>) -> usage::Members {
    let read = |path: std::path::PathBuf| fs::read_to_string(path).ok()?.parse::<toml::Table>().ok();
    let inherited = read(workspace.root.join("Cargo.toml"))
        .and_then(|root| root.get("workspace")?.get("package")?.get("version")?.as_str().map(str::to_owned));
    let mut members = usage::Members::new();
    for package in &workspace.packages {
        let manifest = workspace.root.join(&package.dir).join("Cargo.toml");
        let version = read(manifest).and_then(|table| match table.get("package")?.get("version")? {
            toml::Value::String(version) => Some(version.clone()),
            _ => inherited.clone(),
        });
        match version {
            Some(version) => {
                members.insert((package.name.clone(), version));
            }
            None => found.push(format!(
                "{}/Cargo.toml: cannot read the version of `{}`",
                package.dir.display(),
                package.name
            )),
        }
    }
    members
}

/// Whether a change to `file` (relative to the root) can change the lint's result: the licence files, register and
/// policy, the workflows whose `tools:` it reads, and every asset it scans. `affected` calls this, so the lint and the
/// affected-scope trigger share one list.
pub fn is_input(file: &Path) -> bool {
    file.starts_with(LICENSING_DIR) || file.starts_with(WORKFLOWS_DIR) || usage::is_scanned_asset(file)
}

/// The file extensions the lint treats as assets under `crates/`.
#[cfg(test)]
pub fn scanned_extensions() -> &'static [&'static str] {
    usage::ASSET_EXTENSIONS
}

/// Prints every problem and a summary; returns whether there were none.
pub fn report(workspace: &Workspace) -> bool {
    let (found, _, files) = lint(workspace);
    summarise(&found, files, |line| println!("{line}"))
}

fn summarise(found: &[String], files: usize, mut out: impl FnMut(&str)) -> bool {
    for problem in found {
        out(problem);
    }
    if found.is_empty() {
        out(&format!("lint-licences: ok ({files} files)"));
    } else {
        out(&format!("lint-licences: {} problem(s); the rules are in docs/licensing/policy.md", found.len()));
    }
    found.is_empty()
}

/// The `lint-licences` command.
pub fn run(workspace: &Workspace, args: &LintLicencesArgs) -> ExitCode {
    let passed = if args.print {
        let (found, generated, files) = lint(workspace);
        print!("{generated}");
        summarise(&found, files, |line| eprintln!("{line}"))
    } else {
        report(workspace)
    };
    if passed { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repository_has_a_file_for_everything_it_uses() {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("../Cargo.toml");
        let workspace = Workspace::load(Some(&manifest)).expect("workspace");
        let (found, generated, files) = lint(&workspace);
        assert!(found.is_empty(), "{}", found.join("\n"));
        assert!(files > 0 && !generated.contains("licence-table:"), "{files} files");
    }

    #[test]
    fn every_scanned_input_schedules_the_lint() {
        let mut files: Vec<String> = usage::ASSET_EXTENSIONS.iter().map(|ext| format!("crates/a/x.{ext}")).collect();
        files.extend(["crates/a/NOTICES.md", ".github/assets/any.txt", ".github/workflows/ci.yml"].map(str::to_owned));
        files.extend([format!("{DEPS_DIR}/serde.toml"), REGISTER.to_owned()]);
        for file in &files {
            assert!(is_input(Path::new(file)), "{file}");
        }
        assert!(!is_input(Path::new("crates/a/src/lib.rs")));
    }
}
