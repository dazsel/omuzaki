//! The link gate (ADR 0015): which self-registrations each App binary must contain, read from the source, and the
//! checks that it does.
//!
//! Crates register ops, part kinds, templates, formats and the rest with `register_<kind>!` macros, so no central list
//! exists (spec v3 §39 rule 2). The price is that nothing in the code names a registration, and a linker keeps only
//! what something references: a crate the binary never mentions is not linked at all, and on wasm an object file
//! nothing references is dropped with its registrations. No error says so; the op is simply not there.
//!
//! - [`Sources::expected`] reads every registration in the crates a binary links, from their `src/` trees.
//! - [`verify`] compares that with what the binary recorded at runtime (`omuzaki_core::link::registered()`), by name.
//!   Each App runs it in `tests/registrations.rs`.
//! - [`lint_links`] checks the `link()` wiring that keeps registrations in binaries on every target.
//!
//! What counts as a registration: a call, at item level in a library source file (the file's top level or an inline
//! `mod` block), of a macro named `register_<kind>` that some crate in the workspace defines with `macro_rules!`, and
//! that the build compiles. The `cfg`s on the call, on its inline modules and on the `mod` declarations that lead to
//! its file are evaluated (`cfg::Cfg`): `#[cfg(test)]` code is left out, and a `cfg` the scan can't decide (a cargo
//! feature) is an error, never a guess. Binary targets (`src/main.rs`, `src/bin/`) count for their own binary only;
//! `tests/`, `benches/` and `examples/` never ship.

mod cfg;
mod crates;
mod gate;
pub mod lex; // `pub` because `lint-ui-writes` is in the binary crate, which sees this library only through its API
mod links;
mod scan;
mod wiring;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cfg::Cfg;
pub use cfg::Target;
pub use crates::{Crate, Dependency};
pub use gate::verify;
pub use links::lint_links;
use scan::{CfgAt, FileScan, ModDecl};

/// One self-registration: found in the source, or recorded by a running binary.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Registration {
    /// The package that registers it (`omuzaki-char`).
    pub package: String,
    /// The registry: the macro's name without `register_` (`op`, `part_kind`, ...).
    pub kind: String,
    /// The macro's first argument as written (`CharacterCreate`, `"button"`). Compared without whitespace, because
    /// the compiler's `stringify!` spaces some tokens differently from the source.
    pub name: String,
    /// The file of the macro call, relative to the workspace root, with `/` separators.
    pub file: String,
    /// The line of the macro call.
    pub line: u32,
}

impl Registration {
    /// Reads the line form, `package kind name file:line` separated by tabs (`omuzaki registrations`, and the
    /// `Display` of `omuzaki_core::link::Registered`). `None` for anything else.
    pub fn parse(line: &str) -> Option<Self> {
        let mut fields = line.split('\t');
        let (package, kind, name, place) = (fields.next()?, fields.next()?, fields.next()?, fields.next()?);
        let (file, line) = place.rsplit_once(':')?;
        if fields.next().is_some() {
            return None;
        }
        Some(Self {
            package: package.to_owned(),
            kind: kind.to_owned(),
            name: name.to_owned(),
            file: file.replace('\\', "/"),
            line: line.parse().ok()?,
        })
    }
}

impl fmt::Display for Registration {
    /// The line form [`Registration::parse`] reads.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\t{}\t{}\t{}:{}", self.package, self.kind, self.name, self.file, self.line)
    }
}

/// Why the sources could not be read.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A file or directory could not be read.
    #[error("cannot read {}: {source}", path.display())]
    Read {
        /// What was being read.
        path: PathBuf,
        /// The OS error.
        source: io::Error,
    },
    /// A manifest is not valid TOML or lacks what the gate needs.
    #[error("{}: {message}", path.display())]
    Manifest {
        /// The manifest.
        path: PathBuf,
        /// What is wrong with it.
        message: String,
    },
    /// No workspace member has this package name.
    #[error("no workspace crate is called `{0}`")]
    UnknownCrate(String),
    /// The package has no binary of this name.
    #[error("`{package}` has no binary called `{bin}`")]
    UnknownBin {
        /// The package.
        package: String,
        /// The binary asked for.
        bin: String,
    },
    /// A feature the package being built has no way to turn on: not one of its features or optional dependencies, and
    /// not `dep/feature` for a workspace crate it declares (the gate reads only those, so it refuses even a valid
    /// `serde/std` rather than guess).
    #[error(
        "`{package}` has no feature `{feature}` the link gate can follow (`feature`, or `dep/feature` for a \
         workspace crate)"
    )]
    UnknownFeature {
        /// The package being built.
        package: String,
        /// The feature as given.
        feature: String,
    },
    /// A registration sits under a `cfg` the scan can't decide, so whether the binary should contain it is unknown.
    #[error(
        "{registration} is compiled only under {conditions}, which the link gate can't decide from the source (it \
         doesn't see cargo's feature resolution). Make the registration unconditional, or teach \
         `xtask::registry::cfg` this option (ADR 0015)"
    )]
    UndecidableCfg {
        /// The call: `file:line register_<kind>!(<name>)`.
        registration: String,
        /// The undecided predicates and where each one is written.
        conditions: String,
    },
}

/// A `cfg` predicate and where it is written.
#[derive(Debug, Clone)]
struct Condition {
    cfg: Cfg,
    file: String,
    line: u32,
}

impl Condition {
    fn at(file: &str, cfgs: &[CfgAt]) -> impl Iterator<Item = Self> {
        cfgs.iter().map(move |at| Self { cfg: at.cfg.clone(), file: file.to_owned(), line: at.line })
    }
}

/// Whether every condition holds: `Ok(false)` as soon as one is false, otherwise `Err` with the undecided ones. "Holds"
/// means holds for `target`, or with `None` can hold on some target that ships (`Cfg::can_be`).
fn all_hold<'a>(conditions: &'a [Condition], target: Option<&Target>) -> Result<bool, Vec<&'a Condition>> {
    let mut undecided = Vec::new();
    for condition in conditions {
        match target.map_or_else(|| condition.cfg.can_be(true), |target| condition.cfg.eval(target)) {
            Some(false) => return Ok(false),
            Some(true) => {}
            None => undecided.push(condition),
        }
    }
    if undecided.is_empty() { Ok(true) } else { Err(undecided) }
}

/// One scanned source file.
#[derive(Debug, Clone)]
struct ScannedFile {
    /// Relative to its crate's directory.
    path: PathBuf,
    /// Relative to the workspace root, with `/` separators: how messages and `Registration::file` name it.
    name: String,
    scan: FileScan,
}

/// One crate's scanned sources and the registrations its build compiles.
#[derive(Debug, Clone)]
pub struct ScannedCrate {
    /// The crate.
    pub krate: Crate,
    files: Vec<ScannedFile>,
    /// The registrations in its library, for the target the scan was made for.
    lib: Vec<Registration>,
    /// The registrations in its library on every target that could ship, not just this build's (`Cfg::can_be`).
    lib_any_target: Vec<Registration>,
    /// The registrations in each binary's own files, for the target the scan was made for.
    bins: BTreeMap<String, Vec<Registration>>,
}

impl ScannedCrate {
    /// The scan of the library's root file, where `link()` is defined; `None` without a library.
    fn lib_root_scan(&self) -> Option<&FileScan> {
        let (_, root) = self.krate.lib.as_ref()?;
        self.files.iter().find(|file| file.path == *root).map(|file| &file.scan)
    }

    /// The registrations its library makes on any target that could ship (wasm included, test code excluded): the set
    /// the wiring lints judge a crate by, since they must not depend on the host that runs them.
    pub fn library_registrations(&self) -> &[Registration] {
        &self.lib_any_target
    }

    /// The crates whose `link()` this crate's sources call, in its library and its binaries.
    fn link_calls(&self) -> Vec<&str> {
        self.files.iter().flat_map(|file| file.scan.link_calls.iter().map(String::as_str)).collect()
    }
}

/// Every workspace crate's `src/` tree, scanned once.
#[derive(Debug, Clone)]
pub struct Sources {
    crates: Vec<ScannedCrate>,
}

impl Sources {
    /// Reads and scans the workspace at `root` for the host's own build ([`Target::host`]).
    ///
    /// # Errors
    /// An unreadable manifest, directory or source file, or [`Error::UndecidableCfg`] for a registration under a
    /// `cfg` the scan can't decide.
    pub fn scan(root: &Path) -> Result<Self, Error> {
        Self::scan_for(root, &Target::host())
    }

    /// [`scan`](Self::scan) for a build of `target`: the `cfg`s on registrations are evaluated with its options, not the
    /// host's, so a module built for another target is judged by what that target compiles.
    ///
    /// # Errors
    /// As [`scan`](Self::scan).
    pub fn scan_for(root: &Path, target: &Target) -> Result<Self, Error> {
        let mut read = Vec::new();
        let mut kinds = BTreeSet::new();
        for krate in crates::load(root)? {
            let dir = root.join(&krate.dir);
            let mut files = Vec::new();
            for file in rust_files(&dir.join("src"))? {
                let source = fs::read_to_string(&file).map_err(|source| Error::Read { path: file.clone(), source })?;
                let scan = scan::scan(&source);
                kinds.extend(scan.defines.iter().cloned());
                let path = file.strip_prefix(&dir).unwrap_or(&file).to_path_buf();
                files.push(ScannedFile { name: slashes(file.strip_prefix(root).unwrap_or(&file)), path, scan });
            }
            read.push((krate, files));
        }
        let crates = read
            .into_iter()
            .map(|(krate, files)| compiled_registrations(krate, files, &kinds, target))
            .collect::<Result<_, _>>()?;
        Ok(Self { crates })
    }

    /// The scanned crates, sorted by name.
    pub fn crates(&self) -> &[ScannedCrate] {
        &self.crates
    }

    /// The crate called `package`.
    ///
    /// # Errors
    /// [`Error::UnknownCrate`].
    pub fn get(&self, package: &str) -> Result<&ScannedCrate, Error> {
        self.crates.iter().find(|c| c.krate.name == package).ok_or_else(|| Error::UnknownCrate(package.to_owned()))
    }

    /// Every registration in every crate's library, sorted.
    pub fn registrations(&self) -> Vec<Registration> {
        let mut all: Vec<Registration> = self.crates.iter().flat_map(|c| c.lib.iter().cloned()).collect();
        all.sort();
        all
    }

    /// What a binary that links `package` must contain: the registrations in the libraries of `package` and of every
    /// crate it links under `[dependencies]`, directly or not, plus those in the binary `bin`'s own files.
    ///
    /// The build's features are read from [`FEATURES_VAR`], written as `cargo --features` takes them: a gate test run
    /// as `cargo test -p omuzaki-cli --features omuzaki-stage/wgpu` is run with
    /// `OMUZAKI_LINK_FEATURES=omuzaki-stage/wgpu` too. Unset, it is the default build. A mismatch fails closed either
    /// way, unless every registration of a crate only the build switched on is missing; so CI sets both from one value.
    ///
    /// # Errors
    /// [`Error::UnknownCrate`], [`Error::UnknownBin`] or [`Error::UnknownFeature`].
    pub fn expected(&self, package: &str, bin: Option<&str>) -> Result<Vec<Registration>, Error> {
        let features = std::env::var(FEATURES_VAR).unwrap_or_default();
        let features: Vec<&str> = features.split([',', ' ']).filter(|f| !f.is_empty()).collect();
        self.expected_with_features(package, bin, &features)
    }

    /// [`expected`](Self::expected) for a build of `package` with these features on, as `cargo --features` names
    /// them: `feature` of `package`, `package/feature`, or `dep/feature` for a dependency `package` declares under the
    /// key `dep` (`omuzaki-stage/wgpu`). Activation follows the whole dependency graph the way Cargo resolves it:
    /// `dep/feature`, `dep?/feature`, `dep:`, features turning on features, and what each dependency declaration asks.
    /// An App's gate test may pass `cfg!(feature = "...")` for each optional registering dependency.
    ///
    /// # Errors
    /// [`Error::UnknownCrate`], [`Error::UnknownBin`] or [`Error::UnknownFeature`].
    pub fn expected_with_features(
        &self,
        package: &str,
        bin: Option<&str>,
        features: &[&str],
    ) -> Result<Vec<Registration>, Error> {
        let app = self.get(package)?;
        let mut all: Vec<Registration> =
            self.closure(app, features)?.into_iter().flat_map(|c| c.lib.iter().cloned()).collect();
        if let Some(bin) = bin {
            if !app.krate.bins.iter().any(|(name, _)| name == bin) {
                return Err(Error::UnknownBin { package: package.to_owned(), bin: bin.to_owned() });
            }
            all.extend(app.bins.get(bin).into_iter().flatten().cloned());
        }
        all.sort();
        Ok(all)
    }

    /// The crates a build of `package` with these features links, `package` included, by name: what the expected set
    /// is read from.
    ///
    /// # Errors
    /// [`Error::UnknownCrate`] or [`Error::UnknownFeature`].
    pub fn linked_crates(&self, package: &str, features: &[&str]) -> Result<Vec<String>, Error> {
        Ok(self.closure(self.get(package)?, features)?.into_iter().map(|c| c.krate.name.clone()).collect())
    }

    /// `app` and every crate a build of it with these command-line `features` links, each crate once.
    fn closure<'a>(&'a self, app: &'a ScannedCrate, features: &[&str]) -> Result<Vec<&'a ScannedCrate>, Error> {
        let root = &app.krate;
        let unknown = |feature: &str| Error::UnknownFeature { package: root.name.clone(), feature: feature.to_owned() };
        let mut graph = FeatureGraph::default();
        graph.active.insert(root.name.clone());
        graph.enable(&root.name, "default");
        for feature in features {
            match feature.split_once('/') {
                // `--features dep/feature` names a dependency of the package being built, or the package itself; the
                // feature after the slash must exist in that crate, as Cargo requires.
                Some((package, name)) if package == root.name => {
                    if !root.has_feature(name) {
                        return Err(unknown(feature));
                    }
                    graph.enable(&root.name, name);
                }
                Some((key, name)) => {
                    let dep = root.dependencies.iter().find(|d| d.key == key.trim_end_matches('?'));
                    let Some(dep) = dep else { return Err(unknown(feature)) };
                    if !self.get(&dep.package)?.krate.has_feature(name) {
                        return Err(unknown(feature));
                    }
                    graph.apply(root, feature);
                }
                None if root.has_feature(feature) => graph.enable(&root.name, feature),
                None => return Err(unknown(feature)),
            }
        }
        loop {
            let before = graph.size();
            for package in graph.active.clone() {
                let krate = &self.get(&package)?.krate;
                graph.step(krate);
            }
            if graph.size() == before {
                break;
            }
        }
        graph.active.iter().map(|package| self.get(package)).collect()
    }
}

/// The environment variable [`Sources::expected`] reads the build's `--features` from.
pub const FEATURES_VAR: &str = "OMUZAKI_LINK_FEATURES";

impl Crate {
    /// Whether `--features` may name `name` for this crate: one of its `[features]` (`default` only if declared), or
    /// an optional dependency's implicit feature.
    fn has_feature(&self, name: &str) -> bool {
        self.features.iter().any(|(feature, _)| feature == name) || self.implicit(name)
    }

    /// Whether `name` is the implicit feature of an optional dependency: one declared under that key that no feature
    /// lists as `dep:<key>`, and no explicit feature shadows.
    fn implicit(&self, name: &str) -> bool {
        self.dependencies.iter().any(|d| d.optional && d.key == name)
            && !self.dep_syntax.iter().any(|key| key == name)
            && !self.features.iter().any(|(feature, _)| feature == name)
    }
}

/// Feature resolution over the workspace's path dependencies, as Cargo does it for one package's build: which crates
/// are linked and which of their features are on. It grows monotonically to a fixpoint.
///
/// - A non-optional dependency of a linked crate is linked, with the features its declaration lists, and its
///   `default` feature unless the declaration says `default-features = false`.
/// - An optional dependency is linked once its crate switches it on: through its implicit feature, `dep:<key>`, or
///   `<key>/<feature>`, which also turns `feature` on there. `<key>?/<feature>` turns `feature` on only once something
///   else has switched the dependency on.
/// - A feature that lists another feature turns that one on.
///
/// Not `cargo metadata`: the gate also runs inside test binaries where no cargo can be asked (see `crates`). The test
/// `xtask/tests/feature_graph.rs` checks this resolution against `cargo metadata`'s on fixtures and on the workspace.
#[derive(Default)]
struct FeatureGraph {
    /// The linked crates.
    active: BTreeSet<String>,
    /// The features on in each linked crate, implicit ones included.
    enabled: BTreeMap<String, BTreeSet<String>>,
    /// The optional dependencies each crate has switched on, by key.
    switched_on: BTreeMap<String, BTreeSet<String>>,
}

impl FeatureGraph {
    fn size(&self) -> usize {
        let count = |map: &BTreeMap<String, BTreeSet<String>>| map.values().map(BTreeSet::len).sum::<usize>();
        self.active.len() + count(&self.enabled) + count(&self.switched_on)
    }

    fn enable(&mut self, package: &str, feature: &str) {
        self.enabled.entry(package.to_owned()).or_default().insert(feature.to_owned());
    }

    fn is_on(&self, krate: &Crate, dep: &Dependency) -> bool {
        !dep.optional || self.switched_on.get(&krate.name).is_some_and(|keys| keys.contains(&dep.key))
    }

    /// Switches on `krate`'s optional dependency `key`, with its implicit feature if it has one.
    fn switch_on(&mut self, krate: &Crate, key: &str) {
        self.switched_on.entry(krate.name.clone()).or_default().insert(key.to_owned());
        if krate.implicit(key) {
            self.enable(&krate.name, key);
        }
    }

    /// One pass over a linked crate: links its switched-on dependencies with what their declarations ask, and
    /// applies the lists of its features that are on.
    fn step(&mut self, krate: &Crate) {
        for dep in &krate.dependencies {
            if !self.is_on(krate, dep) {
                continue;
            }
            self.active.insert(dep.package.clone());
            if dep.default_features {
                self.enable(&dep.package, "default");
            }
            for feature in &dep.features {
                self.enable(&dep.package, feature);
            }
        }
        for feature in self.enabled.get(&krate.name).cloned().unwrap_or_default() {
            match krate.features.iter().find(|(name, _)| *name == feature) {
                Some((_, list)) => list.iter().for_each(|item| self.apply(krate, item)),
                None if krate.implicit(&feature) => self.switch_on(krate, &feature),
                None => {} // `default` when the crate has none, or a feature Cargo would reject
            }
        }
    }

    /// Applies one entry of a feature list of `krate` (or of a command line building it).
    fn apply(&mut self, krate: &Crate, item: &str) {
        if let Some(key) = item.strip_prefix("dep:") {
            self.switch_on(krate, key);
        } else if let Some((key, feature)) = item.split_once('/') {
            let weak = key.ends_with('?');
            let key = key.trim_end_matches('?');
            // A dependency that isn't a workspace member (`serde/std`) links no registrations.
            let Some(dep) = krate.dependencies.iter().find(|d| d.key == key) else { return };
            if weak && !self.is_on(krate, dep) {
                return; // a later pass applies it again, once something else has switched the dependency on
            }
            if dep.optional {
                self.switch_on(krate, key);
            }
            self.enable(&dep.package, feature);
        } else {
            self.enable(&krate.name, item);
        }
    }
}

/// The registrations `krate`'s build for `target` compiles, sorted into its library and its binaries: calls of the
/// workspace's registration macros whose `cfg`s hold. The library's are also listed for every target that could ship
/// (`lib_any_target`), which the lints use so that a registration behind `cfg(target_arch = "wasm32")` still counts on
/// a Linux host.
fn compiled_registrations(
    krate: Crate,
    files: Vec<ScannedFile>,
    kinds: &BTreeSet<String>,
    target: &Target,
) -> Result<ScannedCrate, Error> {
    let conditions = file_conditions(&krate, &files);
    let (lib, bins) = registrations_where(&krate, &files, &conditions, kinds, Some(target))?;
    let (lib_any_target, _) = registrations_where(&krate, &files, &conditions, kinds, None)?;
    Ok(ScannedCrate { krate, files, lib, lib_any_target, bins })
}

/// A crate's library registrations and each binary's.
type Compiled = (Vec<Registration>, BTreeMap<String, Vec<Registration>>);

/// The registration calls whose `cfg`s hold (for `target`, or on some target that ships when it is `None`), split into
/// the library's and each binary's.
fn registrations_where(
    krate: &Crate,
    files: &[ScannedFile],
    conditions: &BTreeMap<PathBuf, Vec<Condition>>,
    kinds: &BTreeSet<String>,
    target: Option<&Target>,
) -> Result<Compiled, Error> {
    let mut lib = Vec::new();
    let mut bins: BTreeMap<String, Vec<Registration>> = BTreeMap::new();
    for file in files {
        let inherited = conditions.get(&file.path).map(Vec::as_slice).unwrap_or_default();
        for call in file.scan.calls.iter().filter(|call| kinds.contains(&call.kind)) {
            let mut all: Vec<Condition> = inherited.to_vec();
            all.extend(Condition::at(&file.name, &call.cfgs));
            let compiled = all_hold(&all, target).map_err(|undecided| Error::UndecidableCfg {
                registration: format!("{}:{} register_{}!({})", file.name, call.line, call.kind, call.name),
                conditions: undecided
                    .iter()
                    .map(|c| format!("`cfg({})` ({}:{})", c.cfg, c.file, c.line))
                    .collect::<Vec<_>>()
                    .join(" and "),
            })?;
            if !compiled {
                continue;
            }
            let registration = Registration {
                package: krate.name.clone(),
                kind: call.kind.clone(),
                name: call.name.clone(),
                file: file.name.clone(),
                line: call.line,
            };
            match owning_bin(krate, &file.path) {
                Some(bin) => bins.entry(bin).or_default().push(registration),
                None => lib.push(registration),
            }
        }
    }
    Ok((lib, bins))
}

/// The `cfg`s each file is compiled under: those on the chain of `mod` declarations from a crate root (library or
/// binary) down to it, plus the file's own `#![cfg]`s. A file no declaration reaches (one `include!`d from generated
/// code, say) keeps only its own.
fn file_conditions(krate: &Crate, files: &[ScannedFile]) -> BTreeMap<PathBuf, Vec<Condition>> {
    let by_path: BTreeMap<&Path, &ScannedFile> = files.iter().map(|f| (f.path.as_path(), f)).collect();
    let own = |file: &ScannedFile| Condition::at(&file.name, &file.scan.inner_cfgs).collect::<Vec<_>>();
    let roots = krate.lib.iter().map(|(_, root)| root).chain(krate.bins.iter().map(|(_, root)| root));
    let mut queue: VecDeque<(PathBuf, Vec<Condition>, bool)> =
        roots.filter_map(|root| by_path.get(root.as_path()).map(|file| (root.clone(), own(file), true))).collect();
    let mut out: BTreeMap<PathBuf, Vec<Condition>> = BTreeMap::new();
    while let Some((path, conditions, is_root)) = queue.pop_front() {
        if out.contains_key(&path) {
            continue;
        }
        if let Some(file) = by_path.get(path.as_path()) {
            for decl in &file.scan.mods {
                let Some(child) = child_files(&path, is_root, decl).into_iter().find_map(|c| by_path.get(c.as_path()))
                else {
                    continue;
                };
                let mut inherited = conditions.clone();
                inherited.extend(Condition::at(&file.name, &decl.cfgs));
                inherited.extend(own(child));
                queue.push_back((child.path.clone(), inherited, false));
            }
        }
        out.insert(path, conditions);
    }
    for file in files {
        out.entry(file.path.clone()).or_insert_with(|| own(file));
    }
    out
}

/// Where `mod name;` in `file` can live, as rustc looks for it: `name.rs` or `name/mod.rs` in the module's directory,
/// or the `#[path]`. A crate root or `mod.rs` owns its directory; any other `foo.rs` owns `foo/`.
fn child_files(file: &Path, is_root: bool, decl: &ModDecl) -> Vec<PathBuf> {
    let parent = file.parent().unwrap_or_else(|| Path::new(""));
    let owns_dir = is_root || file.file_name().is_some_and(|name| name == "mod.rs");
    let mut dir = if owns_dir { parent.to_path_buf() } else { parent.join(file.file_stem().unwrap_or_default()) };
    if let Some(path) = &decl.path {
        // Outside inline modules a `#[path]` is relative to the declaring file's directory.
        let base = if decl.inline.is_empty() { parent.to_path_buf() } else { dir.join(decl.inline.join("/")) };
        return vec![crates::normalise(&base.join(path))];
    }
    for module in &decl.inline {
        dir.push(module);
    }
    vec![dir.join(format!("{}.rs", decl.name)), dir.join(&decl.name).join("mod.rs")]
}

/// Shorthand for [`Sources::scan`] then [`Sources::expected`].
///
/// # Errors
/// As those two.
pub fn expected(root: &Path, package: &str, bin: Option<&str>) -> Result<Vec<Registration>, Error> {
    Sources::scan(root)?.expected(package, bin)
}

/// [`expected`] for a build of `target`: [`Sources::scan_for`] then [`Sources::expected`].
///
/// # Errors
/// As those two.
pub fn expected_for(
    root: &Path,
    target: &Target,
    package: &str,
    bin: Option<&str>,
) -> Result<Vec<Registration>, Error> {
    Sources::scan_for(root, target)?.expected(package, bin)
}

/// [`Sources::expected_with_features`] for the workspace at `root`.
///
/// # Errors
/// As [`Sources::scan`] and [`Sources::expected_with_features`].
pub fn expected_with_features(
    root: &Path,
    package: &str,
    bin: Option<&str>,
    features: &[&str],
) -> Result<Vec<Registration>, Error> {
    Sources::scan(root)?.expected_with_features(package, bin, features)
}

/// The binary that owns `file` (relative to its crate), if a binary does: a binary's root file, and everything
/// under `src/bin/`.
fn owning_bin(krate: &Crate, file: &Path) -> Option<String> {
    if let Some((name, _)) = krate.bins.iter().find(|(_, root)| root == file) {
        return Some(name.clone());
    }
    let in_bin_dir = file.strip_prefix("src/bin").ok()?;
    let first = in_bin_dir.components().next()?.as_os_str().to_string_lossy().into_owned();
    Some(first.strip_suffix(".rs").map_or(first.clone(), str::to_owned))
}

/// Every `.rs` file under `dir`, sorted; none if `dir` doesn't exist.
fn rust_files(dir: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    let mut todo = vec![dir.to_path_buf()];
    while let Some(next) = todo.pop() {
        let entries = fs::read_dir(&next).map_err(|source| Error::Read { path: next.clone(), source })?;
        for path in entries.filter_map(Result::ok).map(|e| e.path()) {
            if path.is_dir() {
                todo.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// A relative path with `/` separators on every platform, as `file!()` writes it when cargo builds on Linux.
fn slashes(path: &Path) -> String {
    path.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_line_form_round_trips_and_rejects_other_text() {
        let r = Registration {
            package: "omuzaki-char".into(),
            kind: "op".into(),
            name: "crate::ops::Rename".into(),
            file: "crates/omuzaki-char/src/ops.rs".into(),
            line: 12,
        };
        assert_eq!(Registration::parse(&r.to_string()), Some(r));
        let windows = Registration::parse("a\top\tX\tcrates\\a\\src\\lib.rs:3").map(|r| r.file);
        assert_eq!(windows.as_deref(), Some("crates/a/src/lib.rs"));
        for bad in ["", "a\top\tX", "a\top\tX\tfile", "a\top\tX\tfile:x", "a\top\tX\tf:1\textra"] {
            assert_eq!(Registration::parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn a_binary_owns_its_root_and_everything_under_src_bin() {
        let krate = Crate {
            name: "omuzaki-cli".into(),
            dir: "crates/omuzaki-cli".into(),
            lib: Some(("omuzaki_cli".into(), "src/lib.rs".into())),
            deps: Vec::new(),
            bins: vec![("omuzaki".into(), "src/main.rs".into()), ("fixture".into(), "src/bin/fixture.rs".into())],
            registers: false,
            generates_links: false,
            lists_link_build: false,
            optional_deps: Vec::new(),
            dependencies: Vec::new(),
            features: Vec::new(),
            dep_syntax: Vec::new(),
        };
        assert_eq!(owning_bin(&krate, Path::new("src/main.rs")).as_deref(), Some("omuzaki"));
        assert_eq!(owning_bin(&krate, Path::new("src/bin/fixture.rs")).as_deref(), Some("fixture"));
        assert_eq!(owning_bin(&krate, Path::new("src/bin/tool/helper.rs")).as_deref(), Some("tool"));
        assert_eq!(owning_bin(&krate, Path::new("src/ops/mod.rs")), None);
    }
}
