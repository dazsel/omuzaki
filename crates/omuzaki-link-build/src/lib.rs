//! Generates a crate's `link()` calls from its manifest (ADR 0015, ADR 0054 section 6).
//!
//! A crate whose `link()` forwards (an App, or `omuzaki-stage`) used to list the `link()` of every registering crate
//! it builds on by hand, so adding a crate to an App edited that list. Now the crate's `build.rs` calls [`emit`], which
//! reads the crate's own `Cargo.toml` and writes one `link()` call for every dependency whose manifest sets
//! `registers = true` under `[package.metadata.omuzaki]`. The crate includes the result in its `define_link!`:
//!
//! ```ignore
//! omuzaki_core::define_link! {
//!     include!(concat!(env!("OUT_DIR"), "/link_calls.rs"));
//! }
//! ```
//!
//! `registers = true` means "this crate has a `link()` that Apps must call": it makes registrations itself, or it
//! forwards to crates that do. The link gate (each App's `tests/registrations.rs`) still proves nothing was dropped,
//! and `cargo xtask lint-registers` fails a crate that registers without the flag.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fmt, fs};

use toml::{Table, Value};

/// The file [`emit`] writes under `OUT_DIR`.
pub const OUT_FILE: &str = "link_calls.rs";

/// Why the calls could not be generated.
#[derive(Debug, PartialEq, Eq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

/// What [`generate`] found.
#[derive(Debug, PartialEq, Eq)]
pub struct Generated {
    /// The Rust block to include: one `<crate>::link();` statement per flagged dependency, in package order.
    pub source: String,
    /// The manifests that were read; a change to any of them changes the result.
    pub inputs: Vec<PathBuf>,
}

/// The build-script entry point: writes `$OUT_DIR/link_calls.rs` for the crate being built and tells cargo to rerun
/// when a manifest it read changes. A failure is reported as a cargo error and ends the build script.
pub fn emit() {
    if let Err(error) = try_emit() {
        println!("cargo::error=omuzaki-link-build: {error}");
        std::process::exit(1);
    }
}

#[allow(
    clippy::disallowed_methods,
    reason = "a build script reads manifests and writes a file at build time; the fence is for ops"
)]
fn try_emit() -> Result<(), Error> {
    let var = |name: &str| env::var_os(name).map(PathBuf::from).ok_or_else(|| Error(format!("{name} isn't set")));
    let (manifest_dir, out) = (var("CARGO_MANIFEST_DIR")?, var("OUT_DIR")?);
    let generated = generate(&manifest_dir)?;
    for input in &generated.inputs {
        println!("cargo::rerun-if-changed={}", input.display());
    }
    let target = out.join(OUT_FILE);
    fs::write(&target, generated.source).map_err(|e| Error(format!("cannot write {}: {e}", target.display())))
}

/// The workspace manifest, read when a dependency says `workspace = true`.
struct Workspace {
    /// Its directory: inherited `path`s are relative to it.
    root: PathBuf,
    /// `[workspace.dependencies]`.
    dependencies: Table,
}

/// A dependency as the generator understands it.
struct Dependency {
    /// The member's spec with the workspace's entry underneath (`workspace = true`).
    spec: Table,
    /// The directory of the workspace crate it names; `None` for a crate from a registry or git, which cannot carry
    /// the flag.
    dir: Option<PathBuf>,
}

/// Reads dependency specs, remembering every manifest it opened.
struct Reader<'a> {
    manifest_dir: &'a Path,
    workspace: Option<Workspace>,
    inputs: Vec<PathBuf>,
}

impl Reader<'_> {
    fn read(&mut self, path: PathBuf) -> Result<Table, Error> {
        let table = read(&path)?;
        self.inputs.push(path);
        Ok(table)
    }

    /// Finds the workspace manifest above the crate (the nearest `Cargo.toml` with a `[workspace]` table).
    fn workspace(&mut self) -> Result<&Workspace, Error> {
        if self.workspace.is_none() {
            let found = self.manifest_dir.ancestors().skip(1).find_map(|dir| {
                let manifest = read(&dir.join("Cargo.toml")).ok()?;
                manifest.contains_key("workspace").then(|| (dir.to_path_buf(), manifest))
            });
            let Some((root, manifest)) = found else {
                return Err(Error(
                    "a dependency says `workspace = true`, but no workspace manifest is above this crate".into(),
                ));
            };
            self.inputs.push(root.join("Cargo.toml"));
            let dependencies = manifest
                .get("workspace")
                .and_then(|w| w.get("dependencies"))
                .and_then(Value::as_table)
                .cloned()
                .unwrap_or_default();
            self.workspace = Some(Workspace { root, dependencies });
        }
        self.workspace.as_ref().ok_or_else(|| Error("workspace manifest missing".into()))
    }

    /// Resolves one dependency entry. A form it does not know is an error, never skipped: a skipped dependency is a
    /// registering crate whose `link()` is silently left out.
    fn resolve(&mut self, key: &str, entry: &Value) -> Result<Dependency, Error> {
        let Some(member) = entry.as_table() else {
            // `name = "1.2"`: a registry crate.
            return Ok(Dependency { spec: Table::new(), dir: None });
        };
        let mut spec = Table::new();
        let mut base = self.manifest_dir.to_path_buf();
        if member.get("workspace").and_then(Value::as_bool) == Some(true) {
            let workspace = self.workspace()?;
            base.clone_from(&workspace.root);
            match workspace.dependencies.get(key) {
                Some(Value::Table(inherited)) => spec = inherited.clone(),
                Some(_) => {}
                None => {
                    return Err(Error(format!(
                        "`{key}` says `workspace = true`, but [workspace.dependencies] has no `{key}`"
                    )));
                }
            }
        }
        for (name, value) in member {
            if name != "workspace" {
                spec.insert(name.clone(), value.clone());
            }
        }
        if let Some(path) = spec.get("path") {
            let path = path.as_str().ok_or_else(|| Error(format!("`{key}`: `path` is not a string")))?;
            return Ok(Dependency { dir: Some(base.join(path)), spec });
        }
        let external = ["version", "git", "registry"].iter().any(|k| spec.contains_key(*k));
        if external || member.get("workspace").and_then(Value::as_bool) == Some(true) {
            return Ok(Dependency { spec, dir: None });
        }
        Err(Error(format!(
            "`{key}` is a dependency form this generator does not read (no `path`, `version`, `git` or `workspace`)"
        )))
    }
}

/// Generates the calls for the crate whose manifest is `manifest_dir/Cargo.toml`.
///
/// Workspace crates are read by `path`, directly or inherited through `workspace = true` (the workspace manifest is a
/// build input). Dependencies from a registry or git cannot carry the flag and are not read. An optional dependency
/// must have an explicit activation feature, a feature that lists `dep:<name>`; the call sits under `cfg(feature =
/// ...)` for that feature and every feature that enables it, so it is generated whenever the crate is linked through
/// the manifest's own features. (`--features <name>/<feature>` on a command line can switch a dependency on behind the
/// manifest's back; the link gate catches that build.)
///
/// # Errors
/// A manifest that can't be read or parsed, a dependency form the generator does not read, a feature that lists
/// `dep:<name>` for an optional registering dependency, or a flagged dependency declared under
/// `[target.*.dependencies]` (declare it unconditionally).
pub fn generate(manifest_dir: &Path) -> Result<Generated, Error> {
    let own_path = manifest_dir.join("Cargo.toml");
    let own = read(&own_path)?;
    let mut reader = Reader { manifest_dir, workspace: None, inputs: vec![own_path] };
    let mut source = String::from(
        "// Generated by omuzaki-link-build from this crate's Cargo.toml (ADR 0054 section 6); do not edit.\n{\n",
    );
    for (key, entry) in own.get("dependencies").and_then(Value::as_table).into_iter().flatten() {
        let dependency = reader.resolve(key, entry)?;
        let Some(dir) = &dependency.dir else { continue };
        let manifest = reader.read(dir.join("Cargo.toml"))?;
        if !registers(&manifest) {
            continue;
        }
        if dependency.spec.get("optional").and_then(Value::as_bool).unwrap_or(false) {
            forbid_hidden_activation(&own, key)?;
            // Cargo turns the implicit feature `<key>` on whenever the dependency is active, whatever switched it on
            // (a feature in the manifest, or `--features <key>/<feature>` on the command line).
            let _ = writeln!(source, "    #[cfg(feature = {key:?})]");
        }
        let _ = writeln!(source, "    {}::link();", library_name(key, &dependency.spec, &manifest));
    }
    reject_flagged_target_dependencies(&own, &mut reader)?;
    source.push_str("}\n");
    Ok(Generated { source, inputs: reader.inputs })
}

fn read(path: &Path) -> Result<Table, Error> {
    let text = fs::read_to_string(path).map_err(|e| Error(format!("cannot read {}: {e}", path.display())))?;
    text.parse().map_err(|e: toml::de::Error| Error(format!("{}: {e}", path.display())))
}

/// Whether the manifest sets `registers = true` under `[package.metadata.omuzaki]`.
fn registers(manifest: &Table) -> bool {
    manifest
        .get("package")
        .and_then(|p| p.get("metadata"))
        .and_then(|m| m.get("omuzaki"))
        .and_then(|o| o.get("registers"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// How the dependency's library is named in paths: the key it is declared under if renamed, else its `[lib] name`,
/// else its package name; dashes become underscores.
fn library_name(key: &str, spec: &Table, dependency: &Table) -> String {
    let renamed = spec.get("package").and_then(Value::as_str).is_some_and(|package| package != key);
    let declared = dependency.get("lib").and_then(|l| l.get("name")).and_then(Value::as_str);
    let name = if renamed { key } else { declared.unwrap_or(key) };
    name.replace('-', "_")
}

/// Errors when a feature lists `dep:<key>` for the optional dependency `key`.
///
/// `dep:` removes the dependency's implicit feature, and with it the one `cfg` that is true whenever the dependency is
/// active: `--features <key>/<feature>` on a command line would link the crate without any feature the generated call
/// could name, and its registrations would silently go unlinked.
fn forbid_hidden_activation(manifest: &Table, key: &str) -> Result<(), Error> {
    let hidden = format!("dep:{key}");
    let features = manifest.get("features").and_then(Value::as_table);
    for (feature, list) in features.into_iter().flatten() {
        if list.as_array().into_iter().flatten().filter_map(Value::as_str).any(|item| item == hidden) {
            return Err(Error(format!(
                "feature `{feature}` lists `{hidden}`, but `{key}` registers: `dep:` hides its implicit feature `{key}`, \
                 the one the generated link call is gated on. List `\"{key}\"` instead"
            )));
        }
    }
    Ok(())
}

/// Errors on a flagged dependency under `[target.*.dependencies]`. Every manifest it reads is a build input, so one
/// that gains the flag later reruns the build script and reaches the error.
fn reject_flagged_target_dependencies(own: &Table, reader: &mut Reader<'_>) -> Result<(), Error> {
    for (target, table) in own.get("target").and_then(Value::as_table).into_iter().flatten() {
        let dependencies = table.get("dependencies").and_then(Value::as_table);
        for (key, entry) in dependencies.into_iter().flatten() {
            let dependency = reader.resolve(key, entry)?;
            let Some(dir) = &dependency.dir else { continue };
            if registers(&reader.read(dir.join("Cargo.toml"))?) {
                return Err(Error(format!(
                    "`{key}` registers but is declared under [target.{target}.dependencies], which the generated \
                     link list does not read"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAG: &str = "[package.metadata.omuzaki]\nregisters = true\n";

    fn crate_dir(root: &Path, name: &str, manifest: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("Cargo.toml"), format!("[package]\nname = \"{name}\"\n{manifest}")).unwrap();
    }

    fn app(root: &Path, extra: &str) -> Generated {
        crate_dir(root, "app", extra);
        generate(&root.join("app")).unwrap()
    }

    #[test]
    fn a_flagged_dependency_gets_a_call_and_an_unflagged_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        crate_dir(dir.path(), "omuzaki-char", FLAG);
        crate_dir(dir.path(), "omuzaki-doc", "");
        crate_dir(dir.path(), "omuzaki-anim", FLAG);
        let generated = app(
            dir.path(),
            "[dependencies]\nomuzaki-char = { path = \"../omuzaki-char\" }\nomuzaki-doc = { path = \"../omuzaki-doc\" }\nomuzaki-anim = { path = \"../omuzaki-anim\" }\n",
        );
        assert!(
            generated.source.contains("    omuzaki_anim::link();\n    omuzaki_char::link();\n"),
            "{}",
            generated.source
        );
        assert!(!generated.source.contains("omuzaki_doc"), "{}", generated.source);
        assert_eq!(generated.inputs.len(), 4);
    }

    #[test]
    fn dev_dependencies_are_not_linked() {
        let dir = tempfile::tempdir().unwrap();
        crate_dir(dir.path(), "omuzaki-char", FLAG);
        let generated = app(dir.path(), "[dev-dependencies]\nomuzaki-char = { path = \"../omuzaki-char\" }\n");
        assert!(!generated.source.contains("link()"), "{}", generated.source);
    }

    #[test]
    fn an_optional_registering_dependency_is_called_under_its_implicit_feature() {
        let dir = tempfile::tempdir().unwrap();
        crate_dir(dir.path(), "omuzaki-render", FLAG);
        let generated = app(
            dir.path(),
            "[dependencies]\nomuzaki-render = { path = \"../omuzaki-render\", optional = true }\n[features]\nwgpu = [\"omuzaki-render\"]\ngpu = [\"omuzaki-render/extra\"]\n",
        );
        assert!(
            generated.source.contains("    #[cfg(feature = \"omuzaki-render\")]\n    omuzaki_render::link();"),
            "{}",
            generated.source
        );
    }

    #[test]
    fn dep_syntax_on_an_optional_registering_dependency_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        crate_dir(dir.path(), "omuzaki-render", FLAG);
        crate_dir(
            dir.path(),
            "app",
            "[dependencies]\nomuzaki-render = { path = \"../omuzaki-render\", optional = true }\n[features]\nwgpu = [\"dep:omuzaki-render\"]\n",
        );
        let error = generate(&dir.path().join("app")).unwrap_err().to_string();
        assert!(error.contains("`dep:omuzaki-render`") && error.contains("implicit feature"), "{error}");
    }

    #[test]
    fn dep_syntax_on_an_optional_dependency_that_does_not_register_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        crate_dir(dir.path(), "omuzaki-doc", "");
        let generated = app(
            dir.path(),
            "[dependencies]\nomuzaki-doc = { path = \"../omuzaki-doc\", optional = true }\n[features]\ndocs = [\"dep:omuzaki-doc\"]\n",
        );
        assert!(!generated.source.contains("link()"), "{}", generated.source);
    }

    #[test]
    fn a_workspace_inherited_dependency_is_read_through_the_workspace_manifest() {
        let dir = tempfile::tempdir().unwrap();
        crate_dir(dir.path(), "omuzaki-char", FLAG);
        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\n[workspace.dependencies]\nomuzaki-char = { path = \"omuzaki-char\" }\nserde = \"1\"\n",
        )
        .unwrap();
        let generated = app(dir.path(), "[dependencies]\nomuzaki-char.workspace = true\nserde.workspace = true\n");
        assert!(generated.source.contains("    omuzaki_char::link();"), "{}", generated.source);
        assert!(generated.inputs.contains(&dir.path().join("Cargo.toml")), "the workspace manifest is a build input");
    }

    #[test]
    fn an_inherited_optional_dependency_uses_the_members_optional_flag() {
        let dir = tempfile::tempdir().unwrap();
        crate_dir(dir.path(), "omuzaki-render", FLAG);
        fs::write(
            dir.path().join("Cargo.toml"),
            "[workspace]\n[workspace.dependencies]\nomuzaki-render = { path = \"omuzaki-render\" }\n",
        )
        .unwrap();
        let generated = app(
            dir.path(),
            "[dependencies]\nomuzaki-render = { workspace = true, optional = true }\n[features]\nwgpu = [\"omuzaki-render\"]\n",
        );
        assert!(
            generated.source.contains("#[cfg(feature = \"omuzaki-render\")]\n    omuzaki_render::link();"),
            "{}",
            generated.source
        );
    }

    #[test]
    fn an_inherited_dependency_the_workspace_does_not_define_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "[workspace]\n").unwrap();
        crate_dir(dir.path(), "app", "[dependencies]\nomuzaki-char.workspace = true\n");
        let error = generate(&dir.path().join("app")).unwrap_err();
        assert!(error.to_string().contains("has no `omuzaki-char`"), "{error}");
    }

    #[test]
    fn a_dependency_form_the_generator_does_not_read_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        crate_dir(dir.path(), "app", "[dependencies]\nodd = { optional = true }\n");
        let error = generate(&dir.path().join("app")).unwrap_err();
        assert!(error.to_string().contains("does not read"), "{error}");
    }

    #[test]
    fn registry_dependencies_are_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let generated = app(dir.path(), "[dependencies]\nserde = \"1\"\ntoml = { version = \"1\" }\n");
        assert!(!generated.source.contains("link()"), "{}", generated.source);
    }

    #[test]
    fn a_renamed_dependency_is_called_by_its_key() {
        let dir = tempfile::tempdir().unwrap();
        crate_dir(dir.path(), "omuzaki-char", FLAG);
        let generated =
            app(dir.path(), "[dependencies]\nchars = { path = \"../omuzaki-char\", package = \"omuzaki-char\" }\n");
        assert!(generated.source.contains("    chars::link();"), "{}", generated.source);
    }

    #[test]
    fn a_flagged_dependency_under_a_target_table_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        crate_dir(dir.path(), "omuzaki-char", FLAG);
        crate_dir(
            dir.path(),
            "app",
            "[target.'cfg(unix)'.dependencies]\nomuzaki-char = { path = \"../omuzaki-char\" }\n",
        );
        let error = generate(&dir.path().join("app")).unwrap_err();
        assert!(error.to_string().contains("[target.cfg(unix).dependencies]"), "{error}");
    }

    #[test]
    fn an_unflagged_target_dependency_is_still_a_build_input() {
        let dir = tempfile::tempdir().unwrap();
        crate_dir(dir.path(), "omuzaki-doc", "");
        let generated =
            app(dir.path(), "[target.'cfg(unix)'.dependencies]\nomuzaki-doc = { path = \"../omuzaki-doc\" }\n");
        assert!(
            generated.inputs.iter().any(|input| input.ends_with("omuzaki-doc/Cargo.toml")),
            "{:?}",
            generated.inputs
        );
    }

    #[test]
    fn no_flagged_dependency_gives_an_empty_block() {
        let dir = tempfile::tempdir().unwrap();
        let generated = app(dir.path(), "");
        assert!(generated.source.ends_with("{\n}\n"), "{}", generated.source);
    }
}
