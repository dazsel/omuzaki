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

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cfg::Cfg;
pub use crates::Crate;
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

/// Whether every condition holds: `Ok(false)` as soon as one is false, otherwise `Err` with the undecided ones.
fn all_hold(conditions: &[Condition]) -> Result<bool, Vec<&Condition>> {
    let mut undecided = Vec::new();
    for condition in conditions {
        match condition.cfg.eval() {
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
    /// The registrations in its library.
    lib: Vec<Registration>,
    /// The registrations in each binary's own files.
    bins: BTreeMap<String, Vec<Registration>>,
}

impl ScannedCrate {
    /// The scan of the library's root file, where `link()` is defined; `None` without a library.
    fn lib_root_scan(&self) -> Option<&FileScan> {
        let (_, root) = self.krate.lib.as_ref()?;
        self.files.iter().find(|file| file.path == *root).map(|file| &file.scan)
    }

    /// The registrations its library makes.
    pub fn library_registrations(&self) -> &[Registration] {
        &self.lib
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
    /// Reads and scans the workspace at `root`.
    ///
    /// # Errors
    /// An unreadable manifest, directory or source file, or [`Error::UndecidableCfg`] for a registration under a
    /// `cfg` the scan can't decide.
    pub fn scan(root: &Path) -> Result<Self, Error> {
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
            .map(|(krate, files)| compiled_registrations(krate, files, &kinds))
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
    /// crate it depends on under `[dependencies]`, directly or not, plus those in the binary `bin`'s own files.
    ///
    /// # Errors
    /// [`Error::UnknownCrate`] or [`Error::UnknownBin`].
    pub fn expected(&self, package: &str, bin: Option<&str>) -> Result<Vec<Registration>, Error> {
        let app = self.get(package)?;
        let mut all: Vec<Registration> = self.closure(app)?.into_iter().flat_map(|c| c.lib.iter().cloned()).collect();
        if let Some(bin) = bin {
            if !app.krate.bins.iter().any(|(name, _)| name == bin) {
                return Err(Error::UnknownBin { package: package.to_owned(), bin: bin.to_owned() });
            }
            all.extend(app.bins.get(bin).into_iter().flatten().cloned());
        }
        all.sort();
        Ok(all)
    }

    /// `app` and everything it depends on, each crate once.
    fn closure<'a>(&'a self, app: &'a ScannedCrate) -> Result<Vec<&'a ScannedCrate>, Error> {
        let mut seen: Vec<&ScannedCrate> = Vec::new();
        let mut todo = vec![app];
        while let Some(next) = todo.pop() {
            if seen.iter().any(|c| c.krate.name == next.krate.name) {
                continue;
            }
            for dep in &next.krate.deps {
                todo.push(self.get(dep)?);
            }
            seen.push(next);
        }
        Ok(seen)
    }
}

/// The registrations `krate`'s build compiles, sorted into its library and its binaries: calls of the workspace's
/// registration macros whose `cfg`s hold.
fn compiled_registrations(
    krate: Crate,
    files: Vec<ScannedFile>,
    kinds: &BTreeSet<String>,
) -> Result<ScannedCrate, Error> {
    let conditions = file_conditions(&krate, &files);
    let mut lib = Vec::new();
    let mut bins: BTreeMap<String, Vec<Registration>> = BTreeMap::new();
    for file in &files {
        let inherited = conditions.get(&file.path).map(Vec::as_slice).unwrap_or_default();
        for call in file.scan.calls.iter().filter(|call| kinds.contains(&call.kind)) {
            let mut all: Vec<Condition> = inherited.to_vec();
            all.extend(Condition::at(&file.name, &call.cfgs));
            let compiled = all_hold(&all).map_err(|undecided| Error::UndecidableCfg {
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
            match owning_bin(&krate, &file.path) {
                Some(bin) => bins.entry(bin).or_default().push(registration),
                None => lib.push(registration),
            }
        }
    }
    Ok(ScannedCrate { krate, files, lib, bins })
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
        };
        assert_eq!(owning_bin(&krate, Path::new("src/main.rs")).as_deref(), Some("omuzaki"));
        assert_eq!(owning_bin(&krate, Path::new("src/bin/fixture.rs")).as_deref(), Some("fixture"));
        assert_eq!(owning_bin(&krate, Path::new("src/bin/tool/helper.rs")).as_deref(), Some("tool"));
        assert_eq!(owning_bin(&krate, Path::new("src/ops/mod.rs")), None);
    }
}
