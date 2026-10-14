//! `cargo xtask lint-seed`: a published seed version keeps its content, and no item names private material (library D15,
//! D16, ADR 0019).
//!
//! Why: an `id@version` is a pin. Once it is published it must mean the same recipe, or every saved document that applies
//! it changes silently. And the seed is licence-clean by construction, so no item may name the private test material
//! (`docs/licensing/policy.md`). The lint reads each manifest with the library contract (`omuzaki-library-api`), so the
//! canonical form it compares is the one the library hashes. It runs no build, only git and parsing.
//!
//! Checked: every TOML under `crates/*/library/`. A file under `seed/` is a seed item and gets the seed rules. Any other
//! file with a `version` key is adapter data: it gets the version rules over its whole content, and private names are
//! refused anywhere in it. History is matched by `id`, not by path, so an item that moves to another file keeps its
//! history. `cargo xtask check` runs this when a file under `crates/*/library/` changed.

mod bump;
mod private;
mod rules;

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::Args;
use omuzaki_library_api::ItemManifest;

use crate::error::Result;
use crate::process::capture;
use crate::workspace::Workspace;
use bump::Release;

pub use private::PrivateList;

/// Options of `cargo xtask lint-seed`.
#[derive(Debug, Args)]
pub struct LintSeedArgs {
    /// The branch the version rules compare against (its merge-base with HEAD is used).
    #[arg(long, value_name = "REF", default_value = "origin/main")]
    base: String,
}

/// One manifest to check: its path relative to the workspace root (with `/` separators) and its text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The workspace-relative path.
    pub path: String,
    /// The file's text, as checked.
    pub text: String,
}

/// Each item on the merge base, keyed by id: its release, or why it cannot be read there.
pub type Releases = BTreeMap<String, std::result::Result<Release, String>>;

/// One problem, naming its file and, when it has one, its item id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    file: String,
    id: String,
    message: String,
}

impl Violation {
    fn new(file: &str, id: &str, message: impl Into<String>) -> Self {
        Self { file: file.to_owned(), id: id.to_owned(), message: message.into() }
    }
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.id.is_empty() {
            write!(f, "{}: {}", self.file, self.message)
        } else {
            write!(f, "{}: {}: {}", self.file, self.id, self.message)
        }
    }
}

/// Whether a path is a seed item, which must sit under a `seed/` folder of its crate's library.
fn is_seed(path: &str) -> bool {
    path.contains("/library/seed/")
}

/// The published id an item is known by, so its history and its duplicates are found whatever file it sits in. A seed
/// item publishes its `id`. Adapter data has a local `id`, or a `name` (the light rigs), under its section folder, and
/// publishes as `omuzaki/<section>/<local>` (D14): two sections' `curvy` stay apart, and a seed item with that published id
/// collides with the adapter item it would shadow.
fn identity(path: &str, table: &toml::Table) -> Option<String> {
    let local = table.get("id").or_else(|| table.get("name")).and_then(toml::Value::as_str)?;
    if is_seed(path) {
        return Some(local.to_owned());
    }
    let section = Path::new(path).parent()?.file_name()?.to_str()?;
    Some(format!("omuzaki/{section}/{local}"))
}

/// The releases in `files` (path, text), keyed by [`identity`]. A file that is not TOML, or has no identity, is not an
/// item and has no history. The first file with an identity keeps it; a later one with the same identity is reported
/// where it is checked.
pub fn releases<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Releases {
    let mut releases = Releases::new();
    for (path, text) in files {
        let Ok(table) = text.parse::<toml::Table>() else { continue };
        if !is_seed(path) && !table.contains_key("version") {
            continue;
        }
        let Some(id) = identity(path, &table) else { continue };
        let release = if is_seed(path) { read_seed(text) } else { Release::of_adapter(&table) };
        releases.entry(id).or_insert(release);
    }
    releases
}

/// A seed item's release from its manifest text.
fn read_seed(text: &str) -> std::result::Result<Release, String> {
    let manifest = ItemManifest::parse(text).map_err(|error| format!("does not parse as an item manifest: {error}"))?;
    Release::of_manifest(manifest)
}

/// Every problem in `sources`, judged against `releases` (the merge base). A new item has no history to compare.
pub fn lint(sources: &[Source], releases: &Releases, list: &PrivateList) -> Vec<Violation> {
    let mut found = Vec::new();
    let mut owners: BTreeMap<String, String> = BTreeMap::new();
    for source in sources {
        let seed = is_seed(&source.path);
        let table: toml::Table = match source.text.parse() {
            Ok(table) => table,
            Err(error) => {
                found.push(Violation::new(&source.path, "", format!("not valid TOML: {error}")));
                continue;
            }
        };
        if !seed && !table.contains_key("version") {
            // Not an item unless it was one on the merge base: dropping the `version` of a published item must not
            // skip its checks, or its changed content would pass as an unversioned file.
            let published = identity(&source.path, &table).filter(|id| releases.contains_key(id));
            if let Some(id) = published {
                found.push(Violation::new(
                    &source.path,
                    &id,
                    "the `version` was removed, but this item was published with one",
                ));
            }
            continue;
        }
        let (id, messages) =
            if seed { check_seed(source, releases, list) } else { check_adapter(source, &table, releases, list) };
        if !id.is_empty() {
            match owners.get(&id) {
                Some(first) => {
                    found.push(Violation::new(&source.path, &id, format!("the id is also defined in {first}")));
                }
                None => {
                    owners.insert(id.clone(), source.path.clone());
                }
            }
        }
        found.extend(messages.into_iter().map(|message| Violation::new(&source.path, &id, message)));
    }
    found
}

/// A seed item's id and problems: the manifest rules, then the version rules.
fn check_seed(source: &Source, releases: &Releases, list: &PrivateList) -> (String, Vec<String>) {
    let manifest = match ItemManifest::parse(&source.text) {
        Ok(manifest) => manifest,
        Err(error) => return (String::new(), vec![format!("does not parse as an item manifest: {error}")]),
    };
    let id = manifest.id.clone();
    let mut messages = rules::check(&source.path, true, &manifest, list);
    messages.extend(version_problems(true, &id, Release::of_manifest(manifest), releases));
    (id, messages)
}

/// Adapter data's id and problems: the version rules over its whole content, and private names in its decoded values and
/// keys (so an escape such as `b` cannot hide a name; comments are not data).
fn check_adapter(
    source: &Source,
    table: &toml::Table,
    releases: &Releases,
    list: &PrivateList,
) -> (String, Vec<String>) {
    let Some(id) = identity(&source.path, table) else {
        return (
            String::new(),
            vec!["adapter data with a version needs an `id` or `name` to find its history".to_owned()],
        );
    };
    let mut texts = Vec::new();
    collect_strings(&toml::Value::Table(table.clone()), &mut texts);
    let mut messages: Vec<String> = texts
        .iter()
        .filter_map(|text| list.named_in(text))
        .map(|name| format!("names private test material (`{name}`) in adapter data"))
        .collect();
    messages.extend(version_problems(false, &id, Release::of_adapter(table), releases));
    (id, messages)
}

/// Every string in `value` and every key under it, decoded.
fn collect_strings(value: &toml::Value, out: &mut Vec<String>) {
    match value {
        toml::Value::String(text) => out.push(text.clone()),
        toml::Value::Array(items) => items.iter().for_each(|item| collect_strings(item, out)),
        toml::Value::Table(table) => {
            for (key, item) in table {
                out.push(key.clone());
                collect_strings(item, out);
            }
        }
        _ => {}
    }
}

/// The version rules for `now` against the same id's release on the merge base.
fn version_problems(
    seed: bool,
    id: &str,
    now: std::result::Result<Release, String>,
    releases: &Releases,
) -> Vec<String> {
    let now = match now {
        Ok(now) => now,
        Err(message) => return vec![message],
    };
    match releases.get(id) {
        None => bump::check(seed, &now, None),
        Some(Ok(before)) => bump::check(seed, &now, Some(before)),
        Some(Err(message)) => vec![format!("on the merge base, {message}")],
    }
}

/// Every file under `crates/*/library/`, sorted, and the problems reading them (a file that cannot be read is a finding,
/// not a crash, so the rest still gets checked).
fn library_sources(root: &Path) -> (Vec<Source>, Vec<Violation>) {
    let mut sources = Vec::new();
    let mut problems = Vec::new();
    let crates = root.join("crates");
    let Ok(entries) = fs::read_dir(&crates) else {
        return (sources, vec![Violation::new("crates", "", "cannot list the crates folder")]);
    };
    let mut members: Vec<PathBuf> = entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect();
    members.sort();
    for library in members.into_iter().map(|member| member.join("library")).filter(|library| library.is_dir()) {
        collect(root, &library, &mut sources, &mut problems);
    }
    (sources, problems)
}

fn collect(root: &Path, dir: &Path, sources: &mut Vec<Source>, problems: &mut Vec<Violation>) {
    let mut paths: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(entries) => entries.filter_map(|entry| entry.ok().map(|entry| entry.path())).collect(),
        Err(error) => {
            problems.push(Violation::new(&relative(root, dir), "", format!("cannot list the folder: {error}")));
            return;
        }
    };
    paths.sort();
    for path in paths {
        if path.is_dir() {
            collect(root, &path, sources, problems);
        } else if path.extension().is_some_and(|extension| extension == "toml") {
            let relative = relative(root, &path);
            match fs::read_to_string(&path) {
                Ok(text) => sources.push(Source { path: relative, text }),
                Err(error) => problems.push(Violation::new(&relative, "", format!("cannot read: {error}"))),
            }
        }
    }
}

/// The path relative to the workspace root, with `/` separators on every platform.
fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .map(|component| component.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Runs git in `root`; a non-zero exit is an error carrying git's stderr.
fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    capture(Command::new("git").arg("-C").arg(root).args(args))
}

/// The commit `base` and HEAD diverged from.
fn merge_base(root: &Path, base: &str) -> Result<String> {
    let bytes = git(root, &["merge-base", "HEAD", base])?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_owned())
}

/// Every library file on the merge base, as releases keyed by id.
fn previous_releases(root: &Path, merge_base: &str) -> Result<Releases> {
    let listing = git(root, &["ls-tree", "-r", "--name-only", merge_base, "--", "crates"])?;
    let mut files = Vec::new();
    for path in String::from_utf8_lossy(&listing).lines().filter(|path| {
        path.contains("/library/") && Path::new(path).extension().is_some_and(|extension| extension == "toml")
    }) {
        let bytes = git(root, &["show", &format!("{merge_base}:{path}")])?;
        files.push((path.to_owned(), String::from_utf8_lossy(&bytes).into_owned()));
    }
    Ok(releases(files.iter().map(|(path, text)| (path.as_str(), text.as_str()))))
}

/// Whether a file under `crates/*/library/` differs from the merge base, committed or not.
fn library_changed(root: &Path, base: &str) -> Result<bool> {
    let merge_base = merge_base(root, base)?;
    let tracked = git(root, &["diff", "--name-only", &merge_base])?;
    let untracked = git(root, &["ls-files", "--others", "--exclude-standard"])?;
    let listed = [tracked, untracked].concat();
    Ok(String::from_utf8_lossy(&listed).lines().any(|line| line.starts_with("crates/") && line.contains("/library/")))
}

/// Lints every library file, prints each problem and a verdict, and reports whether the seed is clean.
fn report(workspace: &Workspace, base: &str) -> Result<bool> {
    let (sources, mut problems) = library_sources(&workspace.root);
    let merge_base = merge_base(&workspace.root, base)?;
    let previous = previous_releases(&workspace.root, &merge_base)?;
    problems.extend(lint(&sources, &previous, &PrivateList::policy()));
    for problem in &problems {
        println!("{problem}");
    }
    if problems.is_empty() {
        println!("lint-seed: ok ({} files)", sources.len());
    } else {
        println!("lint-seed: {} violation(s)", problems.len());
    }
    Ok(problems.is_empty())
}

/// The `lint-seed` command.
pub fn run(workspace: &Workspace, args: &LintSeedArgs) -> Result<ExitCode> {
    Ok(if report(workspace, &args.base)? { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

/// The `check` step: lints only when a library file changed, so an unrelated change costs nothing. Returns whether the
/// step passed.
pub fn check(workspace: &Workspace, base: &str) -> bool {
    let outcome = library_changed(&workspace.root, base).and_then(|changed| {
        if changed {
            report(workspace, base)
        } else {
            println!("lint-seed: no file under crates/*/library/ changed; skipped");
            Ok(true)
        }
    });
    outcome.unwrap_or_else(|error| {
        eprintln!("lint-seed: {error}");
        false
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &[&str] = &["zorblax"];

    fn list() -> PrivateList {
        PrivateList::new(LIST, &[])
    }

    const TWINTAILS: &str = "schema = 1\nid = \"omuzaki/hair/twintails\"\nversion = \"1.0.0\"\nsection = \"hair\"\nkind = \"part\"\n\
        target = \"character\"\nslot = \"hairstyle\"\ntitle = \"Twintails\"\nsummary = \"Two tails.\"\ntags = [\"hair\"]\n\
        origin = \"generator defaults\"\nparams.len = { type = \"number\", default = 2.0 }\n[rights]\nauthor = \"Omuzaki\"\n\
        licence = \"CC0-1.0\"\ncommercial = \"ok\"\nowned = true\n";

    const SEED_PATH: &str = "crates/omuzaki-char/library/seed/hair/twintails.toml";

    const CURVY: &str = "version = \"1.0.0\"\nid = \"curvy\"\nheight = 0.0\ncurves = 1.0\n";

    const CURVY_PATH: &str = "crates/omuzaki-char/library/body-shapes/curvy.toml";

    fn source(path: &str, text: &str) -> Source {
        Source { path: path.to_owned(), text: text.to_owned() }
    }

    fn messages(sources: &[Source], previous: &[(&str, &str)]) -> Vec<String> {
        lint(sources, &releases(previous.iter().copied()), &list()).iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_reformatted_file_passes_and_a_changed_value_without_a_bump_fails_naming_file_and_id() {
        let reformatted =
            format!("# a comment\n\n{}", TWINTAILS.replace("title = \"Twintails\"", "title   =   \"Twintails\""));
        assert_eq!(messages(&[source(SEED_PATH, &reformatted)], &[(SEED_PATH, TWINTAILS)]), Vec::<String>::new());
        let retitled = TWINTAILS.replace("summary = \"Two tails.\"", "summary = \"Two long tails.\"");
        assert_eq!(
            messages(&[source(SEED_PATH, &retitled)], &[(SEED_PATH, TWINTAILS)]),
            vec![format!(
                "{SEED_PATH}: omuzaki/hair/twintails: the content changed but the version stayed 1.0.0: bump the version"
            )]
        );
    }

    #[test]
    fn a_removed_param_with_only_a_minor_bump_fails_and_a_major_bump_passes() {
        let removed = TWINTAILS.replace("params.len = { type = \"number\", default = 2.0 }\n", "");
        let minor = removed.replace("1.0.0", "1.1.0");
        let found = messages(&[source(SEED_PATH, &minor)], &[(SEED_PATH, TWINTAILS)]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("omuzaki/hair/twintails: param `len` removed needs a major bump"), "{found:?}");
        let major = removed.replace("1.0.0", "2.0.0");
        assert_eq!(messages(&[source(SEED_PATH, &major)], &[(SEED_PATH, TWINTAILS)]), Vec::<String>::new());
    }

    #[test]
    fn a_moved_item_keeps_its_history_so_an_edit_without_a_bump_still_fails() {
        let moved = "crates/omuzaki-anim/library/seed/hair/twintails.toml";
        let retitled = TWINTAILS.replace("summary = \"Two tails.\"", "summary = \"Two long tails.\"");
        assert_eq!(
            messages(&[source(moved, &retitled)], &[(SEED_PATH, TWINTAILS)]),
            vec![format!(
                "{moved}: omuzaki/hair/twintails: the content changed but the version stayed 1.0.0: bump the version"
            )]
        );
    }

    #[test]
    fn a_new_file_has_no_previous_version_to_compare() {
        assert_eq!(messages(&[source(SEED_PATH, TWINTAILS)], &[]), Vec::<String>::new());
    }

    #[test]
    fn a_duplicate_id_is_reported_on_the_later_file() {
        let elsewhere = "crates/omuzaki-anim/library/seed/hair/twintails.toml";
        let found = messages(&[source(SEED_PATH, TWINTAILS), source(elsewhere, TWINTAILS)], &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0]
                .starts_with(&format!("{elsewhere}: omuzaki/hair/twintails: the id is also defined in {SEED_PATH}"))
        );
    }

    #[test]
    fn an_adapter_file_without_a_version_is_not_an_item_and_is_skipped() {
        let adapter = "# body-shape preset\nid = \"curvy\"\nheight = 0.0\n";
        assert_eq!(messages(&[source(CURVY_PATH, adapter)], &[]), Vec::<String>::new());
    }

    #[test]
    fn adapter_data_with_a_version_is_bumped_by_its_whole_content() {
        let reordered = "curves = 1.0\n\nversion = \"1.0.0\"\nheight = 0.0\nid = \"curvy\"\n";
        assert_eq!(messages(&[source(CURVY_PATH, reordered)], &[(CURVY_PATH, CURVY)]), Vec::<String>::new());
        let changed = CURVY.replace("height = 0.0", "height = 0.5");
        assert_eq!(
            messages(&[source(CURVY_PATH, &changed)], &[(CURVY_PATH, CURVY)]),
            vec![format!(
                "{CURVY_PATH}: omuzaki/body-shapes/curvy: the content changed but the version stayed 1.0.0: bump the version"
            )]
        );
    }

    #[test]
    fn a_private_name_in_adapter_data_fails_even_when_escaped() {
        // The previous text carries the same label, so only the name is refused, not an unbumped change as well.
        let escaped = format!("{CURVY}label = \"zor\\u0062lax\"\n");
        assert_eq!(
            messages(&[source(CURVY_PATH, &escaped)], &[(CURVY_PATH, &escaped)]),
            vec![format!(
                "{CURVY_PATH}: omuzaki/body-shapes/curvy: names private test material (`zorblax`) in adapter data"
            )]
        );
    }

    #[test]
    fn dropping_the_version_of_a_published_adapter_item_is_refused_whatever_else_changes() {
        let unversioned = CURVY.replace("version = \"1.0.0\"\n", "");
        let changed = unversioned.replace("height = 0.0", "height = 0.5");
        assert_eq!(
            messages(&[source(CURVY_PATH, &unversioned)], &[(CURVY_PATH, CURVY)]),
            vec![format!(
                "{CURVY_PATH}: omuzaki/body-shapes/curvy: the `version` was removed, but this item was published with one"
            )]
        );
        assert_eq!(messages(&[source(CURVY_PATH, &changed)], &[(CURVY_PATH, CURVY)]).len(), 1);
    }

    #[test]
    fn a_comment_is_not_data_so_a_name_in_one_is_not_refused() {
        let commented = format!("# zorblax preset\n{CURVY}");
        assert_eq!(messages(&[source(CURVY_PATH, &commented)], &[(CURVY_PATH, CURVY)]), Vec::<String>::new());
    }

    #[test]
    fn a_versioned_light_rig_keyed_by_name_is_bumped_like_any_adapter_item() {
        let rig_path = "crates/omuzaki-stage/library/light-rigs/dev-studio.toml";
        let rig = "version = \"1.0.0\"\nname = \"dev-studio\"\nintensity = 1.0\n";
        assert_eq!(messages(&[source(rig_path, rig)], &[(rig_path, rig)]), Vec::<String>::new());
        let changed = rig.replace("intensity = 1.0", "intensity = 0.5");
        assert_eq!(
            messages(&[source(rig_path, &changed)], &[(rig_path, rig)]),
            vec![format!(
                "{rig_path}: omuzaki/light-rigs/dev-studio: the content changed but the version stayed 1.0.0: bump the version"
            )]
        );
    }

    #[test]
    fn the_same_local_id_in_two_sections_is_not_a_duplicate() {
        let other = "crates/omuzaki-char/library/skin-tones/curvy.toml";
        let found = messages(&[source(CURVY_PATH, CURVY), source(other, CURVY)], &[]);
        assert_eq!(found, Vec::<String>::new());
    }

    #[test]
    fn a_seed_item_with_a_published_id_collides_with_the_adapter_item_it_shadows() {
        let seed = "crates/omuzaki-char/library/seed/body-shapes/curvy.toml";
        let seed_item = TWINTAILS
            .replace("omuzaki/hair/twintails", "omuzaki/body-shapes/curvy")
            .replace("section = \"hair\"", "section = \"body-shapes\"");
        let found = messages(&[source(CURVY_PATH, CURVY), source(seed, &seed_item)], &[]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with(&format!("{seed}: omuzaki/body-shapes/curvy: the id is also defined in {CURVY_PATH}"))
        );
    }

    #[test]
    fn a_non_finite_adapter_number_is_refused_not_nulled() {
        let nan = CURVY.replace("height = 0.0", "height = nan");
        let found = messages(&[source(CURVY_PATH, &nan)], &[(CURVY_PATH, CURVY)]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("not finite"), "{found:?}");
    }

    #[test]
    fn a_private_name_in_a_seed_item_fails_with_its_file_and_id() {
        let named = TWINTAILS.replace("title = \"Twintails\"", "title = \"Zorblax tails\"");
        let found = messages(&[source(SEED_PATH, &named)], &[]);
        assert_eq!(
            found,
            vec![format!("{SEED_PATH}: omuzaki/hair/twintails: names private test material (`zorblax`) in its title")]
        );
    }

    #[test]
    fn a_path_is_made_relative_with_forward_slashes() {
        let root = Path::new("/work/omuzaki");
        assert_eq!(
            relative(root, Path::new("/work/omuzaki/crates/omuzaki-char/library/seed")),
            "crates/omuzaki-char/library/seed"
        );
    }
}
