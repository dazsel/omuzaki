//! `cargo xtask lint-net`: a capture crate may not reach a network crate (SE-04, spec v3 §31 and §39 rule 10).
//!
//! Why: "camera frames never leave the device; audio never leaves unless a person routes it". A crate that holds raw
//! camera frames or microphone samples declares `privacy = "capture"` under `[package.metadata.omuzaki]`, and then may
//! not open a connection, through its dependencies or in its source. A crate that depends directly on a network crate
//! declares `net = "<why it talks to the network>"`, so every network user is named for review.
//!
//! ```toml
//! [package.metadata.omuzaki]
//! privacy = "capture"                       # holds raw camera frames or microphone samples
//! net = "why this crate opens connections"  # a crate that talks to a network; the reason is required
//! ```
//!
//! The rules, one violation each:
//! 1. a workspace crate that depends directly (normal or build) on a listed network crate declares `net`;
//! 2. a `capture` crate reaches no network crate through any chain of normal or build dependencies, read from the full
//!    graph `cargo metadata` resolves for every platform (dev-dependencies excluded);
//! 3. the source of a `capture` crate names none of `std::net`, `tokio::net`, `TcpStream`, `TcpListener`, `UdpSocket`
//!    (the lexer drops comments and strings, so they never count);
//! 4. `net` is not empty, and `privacy` is `capture` or absent.
//!
//! Limits, stated so review can judge the lint against them: the network crates are the names in `NETWORK_CRATES` and
//! the `sentry` family, plus every workspace crate that declares `net`, so a network stack under another name is not
//! seen. The source scan is lexical: it cannot follow a socket type through an alias or a generic.
//!
//! `cargo xtask lint-net --list` prints every crate that declares `net`, with its reason, for review.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use clap::Args;
use serde::Deserialize;
use xtask::registry::Error as ReadError;
use xtask::registry::lex::{Tok, Token, tokens};

use crate::error::{Error, Result};
use crate::process::{capture, cargo};
use crate::workspace::Workspace;

/// External crates that open network connections; a workspace crate that depends on one directly declares `net`.
const NETWORK_CRATES: &[&str] = &[
    "reqwest",
    "hyper",
    "hyper-util",
    "ureq",
    "isahc",
    "attohttpc",
    "curl",
    "tungstenite",
    "tokio-tungstenite",
    "rustls",
    "native-tls",
    "openssl",
    "socket2",
    "rmcp",
];

/// Every crate whose name starts with this is a network crate too: the Sentry SDK family.
const NETWORK_PREFIX: &str = "sentry";

/// The key that marks a crate holding raw camera frames or microphone samples.
const PRIVACY_KEY: &str = "privacy";
/// The one value `privacy` may take.
const CAPTURE: &str = "capture";

/// The key that says why a crate talks to the network.
const NET_KEY: &str = "net";

/// The table both keys live in, for messages.
const MANIFEST_TABLE: &str = "[package.metadata.omuzaki]";

/// Socket types a capture crate's source may not name, whichever module they came from.
const SOCKET_TYPES: &[&str] = &["TcpStream", "TcpListener", "UdpSocket"];

/// Modules whose `net` submodule holds sockets: `std::net` and `tokio::net`.
const SOCKET_MODULES: &[&str] = &["std", "tokio"];

/// Options of `cargo xtask lint-net`.
#[derive(Debug, Args)]
pub struct LintNetArgs {
    /// Print every crate that declares `net`, with its reason, in name order, and stop.
    #[arg(long)]
    list: bool,
}

/// One broken rule: `from` is the crate, `to` what it depends on or where the offence is, `rule` what is wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The workspace crate whose manifest or source is wrong.
    pub from: String,
    /// The network crate reached, the dependency declared, or `file:line` for a source mention.
    pub to: String,
    /// What is wrong and what to do.
    pub rule: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -> {}: {}", self.from, self.to, self.rule)
    }
}

/// A workspace member, with the two keys this lint reads.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Crate {
    /// Package name.
    name: String,
    /// The crate's directory, absolute.
    dir: PathBuf,
    /// The `privacy` value, if the manifest declares the key.
    privacy: Option<String>,
    /// The `net` reason, if the manifest declares the key.
    net: Option<String>,
}

/// The dependency graph by crate name: each crate's normal and build dependencies. Two versions of one name merge into
/// one node, which only adds paths, so the lint errs towards reporting.
type Edges = BTreeMap<String, BTreeSet<String>>;

/// The parts of `cargo metadata --format-version 1` (with dependencies) this lint reads.
#[derive(Deserialize)]
struct Metadata {
    workspace_members: Vec<String>,
    packages: Vec<RawPackage>,
    /// Required: cargo prints it with dependencies, so a missing graph is a metadata error and the lint fails closed.
    resolve: RawResolve,
}

#[derive(Deserialize)]
struct RawPackage {
    id: String,
    name: String,
    manifest_path: PathBuf,
    /// `[package.metadata]`, free-form; `null` when the manifest has none.
    metadata: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawResolve {
    nodes: Vec<RawNode>,
}

#[derive(Deserialize)]
struct RawNode {
    id: String,
    deps: Vec<RawNodeDep>,
}

#[derive(Deserialize)]
struct RawNodeDep {
    pkg: String,
    dep_kinds: Vec<RawDepKind>,
}

#[derive(Deserialize)]
struct RawDepKind {
    kind: Option<String>,
}

/// The `[package.metadata.omuzaki]` key `key` as the manifest declares it: `None` when absent. A key present with a
/// value that is not a string, or a `[package.metadata.omuzaki]` that is not a table, reads as `""`, which the lint
/// rejects, so a manifest it can't read fails closed.
fn omuzaki_key(metadata: Option<&serde_json::Value>, key: &str) -> Option<String> {
    let omuzaki = metadata?.get("omuzaki")?;
    let Some(table) = omuzaki.as_object() else { return Some(String::new()) };
    table.get(key).map(|value| value.as_str().unwrap_or_default().to_owned())
}

/// Whether `name` is an external network crate by its name alone.
fn is_external_network(name: &str) -> bool {
    NETWORK_CRATES.contains(&name) || name.starts_with(NETWORK_PREFIX)
}

/// The workspace members, sorted by name so every report is stable.
fn crates(metadata: &Metadata) -> Vec<Crate> {
    let members: BTreeSet<&str> = metadata.workspace_members.iter().map(String::as_str).collect();
    let mut crates: Vec<Crate> = metadata
        .packages
        .iter()
        .filter(|package| members.contains(package.id.as_str()))
        .map(|package| Crate {
            name: package.name.clone(),
            dir: package.manifest_path.parent().unwrap_or(&package.manifest_path).to_path_buf(),
            privacy: omuzaki_key(package.metadata.as_ref(), PRIVACY_KEY),
            net: omuzaki_key(package.metadata.as_ref(), NET_KEY),
        })
        .collect();
    crates.sort_by(|a, b| a.name.cmp(&b.name));
    crates
}

/// The package name of the id `id`, which the resolve graph must name.
fn name_of(metadata: &Metadata, id: &str) -> Result<String> {
    metadata.packages.iter().find(|package| package.id == id).map(|package| package.name.clone()).ok_or_else(|| {
        Error::Failed {
            command: "cargo metadata".to_owned(),
            stderr: format!("the resolve graph names package {id}, which its package list does not"),
        }
    })
}

/// The dependency graph: an edge for every dependency that is normal or build in some target. A dependency with no
/// kind at all counts as normal (fail closed), and only dependencies whose every kind is `dev` are left out.
fn edges(metadata: &Metadata) -> Result<Edges> {
    let mut edges = Edges::new();
    for node in &metadata.resolve.nodes {
        let from = name_of(metadata, &node.id)?;
        let entry = edges.entry(from).or_default();
        for dep in &node.deps {
            let normal =
                dep.dep_kinds.is_empty() || dep.dep_kinds.iter().any(|kind| kind.kind.as_deref() != Some("dev"));
            if normal {
                entry.insert(name_of(metadata, &dep.pkg)?);
            }
        }
    }
    Ok(edges)
}

/// Every network crate `start` reaches, each with the shortest path to it, `start` first. The search stops at a network
/// crate, so a path names the first one met, and dependencies are visited in name order: the output is deterministic.
fn network_paths(edges: &Edges, start: &str, is_network: impl Fn(&str) -> bool) -> Vec<Vec<String>> {
    let mut parent: BTreeMap<&str, &str> = BTreeMap::new();
    let mut seen: BTreeSet<&str> = BTreeSet::from([start]);
    let mut queue: VecDeque<&str> = VecDeque::from([start]);
    let mut found = Vec::new();
    while let Some(current) = queue.pop_front() {
        for next in edges.get(current).into_iter().flatten().map(String::as_str) {
            if !seen.insert(next) {
                continue;
            }
            parent.insert(next, current);
            if is_network(next) {
                let mut path = vec![next.to_owned()];
                let mut step = next;
                while let Some(&before) = parent.get(step) {
                    path.push(before.to_owned());
                    step = before;
                }
                path.reverse();
                found.push(path);
            } else {
                queue.push_back(next);
            }
        }
    }
    found
}

/// Rule 4: a `privacy` value other than `capture`, or an empty `net` reason.
fn key_violations(name: &str, privacy: Option<&str>, net: Option<&str>) -> Vec<Violation> {
    let at = |rule: String| Violation { from: name.to_owned(), to: MANIFEST_TABLE.to_owned(), rule };
    let mut found = Vec::new();
    if let Some(value) = privacy.filter(|value| *value != CAPTURE) {
        found.push(at(format!("privacy = {value:?} is unknown; the only value is \"{CAPTURE}\"")));
    }
    if net.is_some_and(|reason| reason.trim().is_empty()) {
        found.push(at(format!("{NET_KEY} is empty; say why the crate opens connections")));
    }
    found
}

/// Rule 2: the violation for one network path from a capture crate. `to` is the network crate reached.
fn reach_violation(name: &str, path: &[String]) -> Violation {
    Violation {
        from: name.to_owned(),
        to: path.last().cloned().unwrap_or_default(),
        rule: format!("privacy = \"{CAPTURE}\" but it reaches a network crate, via {}", path.join(" -> ")),
    }
}

/// Whether the identifier at token `i` names a socket: a socket type, or `std::net` / `tokio::net`.
fn socket_mention(toks: &[Token], i: usize) -> Option<String> {
    let Tok::Ident(name) = &toks.get(i)?.tok else { return None };
    if SOCKET_TYPES.contains(&name.as_str()) {
        return Some(name.clone());
    }
    let colons = toks.get(i + 1).is_some_and(|t| t.is_punct(':')) && toks.get(i + 2).is_some_and(|t| t.is_punct(':'));
    let net_module = toks.get(i + 3).is_some_and(|t| t.is_ident("net"));
    (colons && net_module && SOCKET_MODULES.contains(&name.as_str())).then(|| format!("{name}::net"))
}

/// Every `.rs` file under `dir`, sorted, so the report is stable.
fn rust_files(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let entries = fs::read_dir(&next).map_err(|source| ReadError::Read { path: next.clone(), source })?;
        for entry in entries {
            let entry = entry.map_err(|source| ReadError::Read { path: next.clone(), source })?;
            let kind = entry.file_type().map_err(|source| ReadError::Read { path: entry.path(), source })?;
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Rule 3: every socket name in the source of capture crate `name`, in file and line order.
fn source_violations(root: &Path, name: &str, dir: &Path) -> Result<Vec<Violation>> {
    let mut found = Vec::new();
    for file in rust_files(dir)? {
        let source = fs::read_to_string(&file).map_err(|source| ReadError::Read { path: file.clone(), source })?;
        let toks = tokens(&source);
        let shown = file.strip_prefix(root).unwrap_or(&file).to_string_lossy().replace('\\', "/");
        for (i, token) in toks.iter().enumerate() {
            if let Some(socket) = socket_mention(&toks, i) {
                found.push(Violation {
                    from: name.to_owned(),
                    to: format!("{shown}:{}", token.line),
                    rule: format!(
                        "names `{socket}`; a capture crate may not open a socket (privacy = \"{CAPTURE}\", SE-04)"
                    ),
                });
            }
        }
    }
    Ok(found)
}

/// Every violation among `crates`, in name order, each crate's rules in the order of the module docs.
fn violations(root: &Path, crates: &[Crate], edges: &Edges) -> Result<Vec<Violation>> {
    let declared: BTreeSet<&str> = crates.iter().filter(|c| c.net.is_some()).map(|c| c.name.as_str()).collect();
    let is_network = |name: &str| is_external_network(name) || declared.contains(name);
    let mut found = Vec::new();
    for krate in crates {
        found.extend(key_violations(&krate.name, krate.privacy.as_deref(), krate.net.as_deref()));
        let direct = edges.get(&krate.name).into_iter().flatten();
        if krate.net.is_none() {
            found.extend(direct.clone().filter(|dep| is_external_network(dep)).map(|dep| Violation {
                from: krate.name.clone(),
                to: dep.clone(),
                rule: format!("depends directly on a network crate, so it must declare `{NET_KEY} = \"<why>\"` under {MANIFEST_TABLE}"),
            }));
        }
        if krate.privacy.as_deref() == Some(CAPTURE) {
            found.extend(
                network_paths(edges, &krate.name, is_network).iter().map(|path| reach_violation(&krate.name, path)),
            );
            found.extend(source_violations(root, &krate.name, &krate.dir)?);
        }
    }
    Ok(found)
}

/// Loads the full dependency graph of the workspace at `root`.
fn load(root: &Path) -> Result<Metadata> {
    let mut command = Command::new(cargo());
    command.args(["metadata", "--format-version", "1", "--manifest-path"]).arg(root.join("Cargo.toml"));
    Ok(serde_json::from_slice(&capture(&mut command)?)?)
}

/// Prints the verdict for `workspace` and reports whether it holds.
pub fn report(workspace: &Workspace) -> Result<bool> {
    let metadata = load(&workspace.root)?;
    let crates = crates(&metadata);
    let found = violations(&workspace.root, &crates, &edges(&metadata)?)?;
    for violation in &found {
        println!("{violation}");
    }
    if found.is_empty() {
        let captures = crates.iter().filter(|c| c.privacy.as_deref() == Some(CAPTURE)).count();
        let nets = crates.iter().filter(|c| c.net.is_some()).count();
        println!("lint-net: ok ({captures} capture crate(s), {nets} crate(s) declare {NET_KEY})");
    } else {
        println!("lint-net: {} violation(s); the keys are in the module docs of xtask/src/lint_net.rs", found.len());
    }
    Ok(found.is_empty())
}

/// The `lint-net` command, or with `--list`, the crates that declare `net` and why.
pub fn run(workspace: &Workspace, args: &LintNetArgs) -> Result<ExitCode> {
    if args.list {
        for krate in crates(&load(&workspace.root)?) {
            if let Some(reason) = &krate.net {
                println!("{}: {reason}", krate.name);
            }
        }
        return Ok(ExitCode::SUCCESS);
    }
    Ok(if report(workspace)? { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edges_of(pairs: &[(&str, &[&str])]) -> Edges {
        pairs.iter().map(|(from, to)| ((*from).to_owned(), to.iter().map(|t| (*t).to_owned()).collect())).collect()
    }

    #[test]
    fn a_path_to_a_network_crate_is_the_shortest_and_names_every_hop() {
        let edges = edges_of(&[("cap", &["mid", "net-a"]), ("mid", &["reqwest"]), ("reqwest", &["hyper"])]);
        let found = network_paths(&edges, "cap", |name| name == "reqwest" || name == "net-a");
        assert_eq!(found, vec![vec!["cap", "net-a"], vec!["cap", "mid", "reqwest"]]);
    }

    #[test]
    fn a_cycle_terminates_and_finds_nothing_when_no_network_crate_is_reached() {
        let edges = edges_of(&[("cap", &["a"]), ("a", &["cap", "b"]), ("b", &["a"])]);
        assert_eq!(network_paths(&edges, "cap", |name| name == "reqwest"), Vec::<Vec<String>>::new());
    }

    #[test]
    fn socket_names_are_found_in_paths_and_by_type_and_nothing_else() {
        let names = |source: &str| -> Vec<String> {
            let toks = tokens(source);
            (0..toks.len()).filter_map(|i| socket_mention(&toks, i)).collect()
        };
        assert_eq!(names("use std::net::TcpStream;"), ["std::net", "TcpStream"]);
        assert_eq!(names("let s = tokio::net::UdpSocket::bind(a);"), ["tokio::net", "UdpSocket"]);
        assert_eq!(names("// std::net::TcpStream\nlet s = \"TcpListener\";"), Vec::<String>::new());
        assert_eq!(names("let netting = std_net_hook();"), Vec::<String>::new());
    }

    #[test]
    fn an_absent_key_is_none_and_a_non_string_one_reads_empty() {
        let metadata = serde_json::json!({ "omuzaki": { "net": 3, "privacy": "capture" } });
        assert_eq!(omuzaki_key(Some(&metadata), "net").as_deref(), Some(""));
        assert_eq!(omuzaki_key(Some(&metadata), "privacy").as_deref(), Some("capture"));
        assert_eq!(omuzaki_key(Some(&metadata), "missing"), None);
        assert_eq!(omuzaki_key(None, "net"), None);
        let not_a_table = serde_json::json!({ "omuzaki": "yes" });
        assert_eq!(omuzaki_key(Some(&not_a_table), "privacy").as_deref(), Some(""));
    }
}
