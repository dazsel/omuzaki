//! `cargo xtask lint-deps`: enforces the crate layering table in `crates/README.md` (spec v3 §39 rule 10).
//!
//! Why: an implementation crate that reaches into another one couples two parallel work streams, and the conflicts
//! show up weeks later. Failing here keeps every crate buildable and testable against contracts and mocks alone.

use std::fmt;
use std::process::ExitCode;

use crate::workspace::{DepKind, Dependency, Package, Workspace};

mod test_crates;

/// The role a crate plays in the layering, derived from its name and, for libraries, its manifest (see
/// `crates/README.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrateKind {
    /// `omuzaki-core` and every `*-api` crate: contracts that change rarely.
    Api,
    /// `*-mock`: in-memory stand-ins for contracts, for tests.
    Mock,
    /// A format library: shared code that several implementations build on (`omuzaki-gltf`), declared with
    /// `layer = "lib"` under `[package.metadata.omuzaki]`. It sits between the contracts and the implementations.
    Lib,
    /// Everything else under `crates/`: the real behaviour.
    Implementation,
    /// `omuzaki-app`, `omuzaki-cli`, `omuzaki-ui`, `omuzaki-test`, `omuzaki-stage`, `xtask`: they wire implementations together. The
    /// area test crates (`omuzaki-test-<area>`) are apps too, held to their area (`test_crates.rs`).
    /// `omuzaki-link-probe` is a test-only App that `cargo xtask wasm-link-proof` builds for wasm (ADR 0015).
    /// `omuzaki-model-worker` is the out-of-process model runner's binary (#2126).
    App,
}

/// The `layer` values a manifest may declare.
const LIB_LAYER: &str = "lib";

impl CrateKind {
    /// Classifies a package by name alone: what every crate is unless its manifest says it is a library.
    pub fn of(name: &str) -> Self {
        match name {
            "omuzaki-core" => Self::Api,
            "omuzaki-app"
            | "omuzaki-cli"
            | "omuzaki-ui"
            | "omuzaki-test"
            | "omuzaki-stage"
            | "omuzaki-cohost"
            | "omuzaki-short"
            | "omuzaki-sequence"
            | "omuzaki-link-probe"
            | "omuzaki-model-worker"
            | "xtask" => Self::App,
            _ if name.ends_with("-api") => Self::Api,
            _ if name.ends_with("-mock") => Self::Mock,
            _ if test_crates::area_of(name).is_some() => Self::App,
            _ => Self::Implementation,
        }
    }

    /// Classifies a workspace member: by name, unless the manifest declares `layer = "lib"`.
    ///
    /// # Errors
    /// The reason, for a manifest that declares a layer this lint doesn't know, or declares `lib` on a crate whose
    /// name already makes it a contract, a mock or an app (a mock must not be able to exempt itself from the table).
    pub fn of_package(package: &Package) -> Result<Self, String> {
        let by_name = Self::of(&package.name);
        match package.layer.as_deref() {
            None => Ok(by_name),
            Some(LIB_LAYER) if by_name == Self::Implementation => Ok(Self::Lib),
            Some(LIB_LAYER) => Err(format!(
                "declares layer \"lib\" but its name makes it an {by_name}; only implementation crates may"
            )),
            Some(other) => {
                Err(format!("declares unknown layer {other:?} under [package.metadata.omuzaki]; known: \"lib\""))
            }
        }
    }

    /// What a crate of this kind may depend on, in the words of the table, for messages.
    fn allowed(self) -> &'static str {
        match self {
            Self::Api => "API crates",
            Self::Mock => "API crates (mocks only under [dev-dependencies])",
            Self::Lib => "API crates and other lib crates (mocks only under [dev-dependencies])",
            Self::Implementation => "API crates and lib crates (mocks only under [dev-dependencies])",
            Self::App => "anything",
        }
    }
}

impl fmt::Display for CrateKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Api => "API crate",
            Self::Mock => "mock crate",
            Self::Lib => "lib crate",
            Self::Implementation => "implementation crate",
            Self::App => "app crate",
        })
    }
}

/// Whether the table lets a crate of kind `from` declare `to` in manifest section `section`.
///
/// Dev-dependencies are the one widening: tests may use mocks (`omuzaki-ops-api` tests against `omuzaki-doc-mock`).
///
/// Libraries are depended on by implementations and apps and by other libraries, never by contracts or mocks.
/// Contracts change rarely and must not pull real code in; a mock is the independent oracle the real code is tested
/// against (`omuzaki-io-gltf` tests read the mock's GLB with the real reader and the reverse), and it stops being
/// independent the moment it shares code with what it checks.
fn permitted(from: CrateKind, to: CrateKind, section: DepKind) -> bool {
    match (from, to) {
        (CrateKind::App, _) | (_, CrateKind::Api) | (CrateKind::Implementation | CrateKind::Lib, CrateKind::Lib) => {
            true
        }
        (_, CrateKind::Mock) => section == DepKind::Dev,
        (CrateKind::Api | CrateKind::Mock, CrateKind::Lib) | (_, CrateKind::Implementation | CrateKind::App) => false,
    }
}

/// One broken rule: `from` declares a dependency on `to` that the table forbids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The crate whose manifest is wrong.
    pub from: String,
    /// The workspace crate it must not depend on.
    pub to: String,
    /// What was declared and what the table allows instead.
    pub rule: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} → {}: {}", self.from, self.to, self.rule)
    }
}

/// Every forbidden dependency between workspace crates, in crate-name order, then every native-only crate in a wasm
/// crate's tree ([`wasm_violations`]). Licences of external crates are never checked here (that is `cargo deny`'s job).
pub fn violations(workspace: &Workspace) -> Vec<Violation> {
    let mut found: Vec<Violation> =
        workspace.packages.iter().flat_map(|package| package_violations(workspace, package)).collect();
    found.extend(wasm_violations(workspace));
    found
}

/// The crate that hosts plugins: native only (a JIT host has no place in a browser, ADR 0058), named here as well as by
/// its `wasm = false` so the rule holds even if its manifest were edited.
const PLUGIN_HOST: &str = "omuzaki-plugin";

/// The prefix of the wasmtime family of external crates.
const WASMTIME_PREFIX: &str = "wasmtime";

/// Why `name` can never be in a wasm build, or `None` if it may: a workspace crate that opts out with `wasm = false`,
/// any `wasmtime*` crate, or the plugin host.
fn native_only(workspace: &Workspace, name: &str) -> Option<&'static str> {
    if name == PLUGIN_HOST {
        Some("the plugin host is native only")
    } else if name.starts_with(WASMTIME_PREFIX) {
        Some("wasmtime is native only")
    } else if workspace.package(name).is_some_and(|package| !package.wasm) {
        Some("it opts out with `wasm = false`")
    } else {
        None
    }
}

/// Every native-only crate in the normal-dependency tree of a crate that builds for wasm (the rule of
/// `tools/ci/crates.py`), with the path that reaches it. The WebAssembly stage only leaves `wasm = false` crates out of
/// its list; without this, a wasm crate that depends on one fails there with a far less clear error. The walk follows
/// workspace crates only (external crates are leaves) and stops at each native-only crate it reports.
pub fn wasm_violations(workspace: &Workspace) -> Vec<Violation> {
    workspace.packages.iter().filter(|package| package.wasm).flat_map(|root| wasm_tree(workspace, root)).collect()
}

/// The native-only crates under `root`, each once, by the shortest path (breadth first, dependencies in manifest order).
fn wasm_tree(workspace: &Workspace, root: &Package) -> Vec<Violation> {
    let mut found = Vec::new();
    let mut seen = std::collections::BTreeSet::from([root.name.as_str()]);
    let mut queue = std::collections::VecDeque::from([vec![root.name.as_str()]]);
    while let Some(path) = queue.pop_front() {
        let Some(package) = path.last().and_then(|name| workspace.package(name)) else { continue };
        for dependency in package.dependencies.iter().filter(|dependency| dependency.kind == DepKind::Normal) {
            let name = dependency.name.as_str();
            if !seen.insert(name) {
                continue;
            }
            let mut next = path.clone();
            next.push(name);
            if let Some(why) = native_only(workspace, name) {
                found.push(Violation {
                    from: root.name.clone(),
                    to: name.to_owned(),
                    rule: format!(
                        "builds for wasm but depends on a native-only crate ({why}), through {}; opt out with \
                         `wasm = false` under [package.metadata.omuzaki] or drop the dependency",
                        next.join(" → ")
                    ),
                });
            } else {
                queue.push_back(next);
            }
        }
    }
    found
}

/// What is wrong with `package`: a manifest layer that doesn't hold up, or a dependency the table forbids.
fn package_violations(workspace: &Workspace, package: &Package) -> Vec<Violation> {
    let from = match CrateKind::of_package(package) {
        Ok(kind) => kind,
        Err(rule) => {
            return vec![Violation { from: package.name.clone(), to: "[package.metadata.omuzaki]".to_owned(), rule }];
        }
    };
    package.dependencies.iter().filter_map(|dependency| violation(workspace, package, from, dependency)).collect()
}

fn violation(workspace: &Workspace, package: &Package, from: CrateKind, dependency: &Dependency) -> Option<Violation> {
    // A crate's own tests may enable its testing kit through a dev-dependency on itself: that couples nothing.
    if dependency.name == package.name && dependency.kind == DepKind::Dev {
        return None;
    }
    let target = workspace.package(&dependency.name)?;
    // A dependency whose own layer is invalid is reported on that crate; here it counts as its name says.
    let to = CrateKind::of_package(target).unwrap_or_else(|_| CrateKind::of(&target.name));
    if permitted(from, to, dependency.kind) {
        // The table lets an app depend on anything; the area test crates are the apps held to less.
        return test_crates::violation(package, dependency, target, to);
    }
    Some(Violation {
        from: package.name.clone(),
        to: dependency.name.clone(),
        rule: format!(
            "{from} may not depend on {to} under {section}; allowed: {allowed}",
            section = dependency.kind.section(),
            allowed = from.allowed(),
        ),
    })
}

/// Prints the verdict for `workspace` and reports whether the layering holds.
pub fn report(workspace: &Workspace) -> bool {
    let found = violations(workspace);
    for violation in &found {
        println!("{violation}");
    }
    if found.is_empty() {
        println!("lint-deps: ok ({} crates)", workspace.packages.len());
    } else {
        println!("lint-deps: {} violation(s); the table is in crates/README.md", found.len());
    }
    found.is_empty()
}

/// The `lint-deps` command.
pub fn run(workspace: &Workspace) -> ExitCode {
    if report(workspace) { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::testing::{with_layer, without_wasm, workspace};
    use DepKind::{Build, Dev, Normal};

    fn found(crates: &[(&str, &[(&str, DepKind)])]) -> Vec<String> {
        violations(&workspace(crates)).iter().map(ToString::to_string).collect()
    }

    fn assert_clean(crates: &[(&str, &[(&str, DepKind)])]) {
        assert_eq!(found(crates), Vec::<String>::new());
    }

    #[test]
    fn classifies_by_name() {
        assert_eq!(CrateKind::of("omuzaki-core"), CrateKind::Api);
        assert_eq!(CrateKind::of("omuzaki-doc-api"), CrateKind::Api);
        assert_eq!(CrateKind::of("omuzaki-doc-mock"), CrateKind::Mock);
        assert_eq!(CrateKind::of("omuzaki-render"), CrateKind::Implementation);
        for app in [
            "omuzaki-app",
            "omuzaki-cli",
            "omuzaki-ui",
            "omuzaki-test",
            "omuzaki-stage",
            "omuzaki-link-probe",
            "omuzaki-model-worker",
            "xtask",
        ] {
            assert_eq!(CrateKind::of(app), CrateKind::App);
        }
    }

    #[test]
    fn the_intended_layering_passes() {
        let ws: &[(&str, &[(&str, DepKind)])] = &[
            ("omuzaki-core", &[("serde", Normal)]),
            ("omuzaki-doc-api", &[("omuzaki-core", Normal)]),
            ("omuzaki-doc-mock", &[("omuzaki-doc-api", Normal)]),
            ("omuzaki-ops-api", &[("omuzaki-doc-api", Normal), ("omuzaki-doc-mock", Dev)]),
            ("omuzaki-render", &[("omuzaki-doc-api", Normal), ("omuzaki-core", Normal), ("omuzaki-doc-mock", Dev)]),
            ("omuzaki-cli", &[("omuzaki-render", Normal), ("omuzaki-doc-mock", Normal)]),
        ];
        assert_clean(ws);
    }

    /// `ws` with `omuzaki-gltf` declared a lib crate.
    fn with_gltf(crates: &[(&str, &[(&str, DepKind)])]) -> Workspace {
        with_layer(workspace(crates), "omuzaki-gltf", "lib")
    }

    fn lib_found(crates: &[(&str, &[(&str, DepKind)])]) -> Vec<String> {
        violations(&with_gltf(crates)).iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_manifest_layer_makes_a_lib_crate() {
        let lib = Package { layer: Some("lib".into()), ..workspace(&[("omuzaki-gltf", &[])]).packages.remove(0) };
        assert_eq!(CrateKind::of_package(&lib), Ok(CrateKind::Lib));
        assert_eq!(CrateKind::of("omuzaki-gltf"), CrateKind::Implementation, "by name alone it is an implementation");
        assert_eq!(CrateKind::Lib.to_string(), "lib crate");
    }

    #[test]
    fn implementations_apps_and_other_libs_may_depend_on_a_lib() {
        let io_api = ("omuzaki-io-api", &[("omuzaki-core", Normal)][..]);
        let core = ("omuzaki-core", &[][..]);
        let gltf =
            ("omuzaki-gltf", &[("omuzaki-io-api", Normal), ("omuzaki-core", Normal), ("omuzaki-io-mock", Dev)][..]);
        let io_mock = ("omuzaki-io-mock", &[("omuzaki-io-api", Normal)][..]);
        assert_eq!(
            lib_found(&[
                core,
                io_api,
                io_mock,
                gltf,
                ("omuzaki-io-gltf", &[("omuzaki-gltf", Normal), ("omuzaki-io-api", Normal), ("omuzaki-io-mock", Dev)]),
                ("omuzaki-io-vrm", &[("omuzaki-gltf", Normal), ("omuzaki-gltf", Build)]),
                ("omuzaki-gltf-extra", &[("omuzaki-gltf", Normal)]),
                ("omuzaki-cli", &[("omuzaki-gltf", Normal)]),
            ]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn a_lib_may_depend_only_on_contracts_and_other_libs() {
        let got = lib_found(&[
            ("omuzaki-gltf", &[("omuzaki-cli", Dev), ("omuzaki-doc-mock", Normal), ("omuzaki-render", Normal)]),
            ("omuzaki-render", &[]),
            ("omuzaki-cli", &[]),
            ("omuzaki-doc-mock", &[]),
        ]);
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[0].starts_with("omuzaki-gltf → omuzaki-cli: lib crate may not depend on app crate"));
        assert!(got[1].starts_with("omuzaki-gltf → omuzaki-doc-mock: lib crate may not depend on mock crate"));
        assert!(got[1].contains("[dependencies]"), "a mock is a dev-dependency or nothing");
        assert!(got[2].starts_with("omuzaki-gltf → omuzaki-render: lib crate may not depend on implementation crate"));
        assert!(got[2].contains("API crates and other lib crates"), "{}", got[2]);
    }

    #[test]
    fn contracts_and_mocks_never_depend_on_a_lib_not_even_in_dev() {
        for (from, section) in [
            ("omuzaki-doc-api", Normal),
            ("omuzaki-doc-api", Dev),
            ("omuzaki-doc-mock", Normal),
            ("omuzaki-doc-mock", Dev),
        ] {
            let got = lib_found(&[(from, &[("omuzaki-gltf", section)]), ("omuzaki-gltf", &[])]);
            assert_eq!(got.len(), 1, "{from} {section:?}: {got:?}");
            assert!(got[0].contains("may not depend on lib crate"), "{}", got[0]);
        }
    }

    #[test]
    fn a_dev_dependency_on_a_lib_is_fine_for_implementations_and_libs() {
        assert_eq!(
            lib_found(&[("omuzaki-render", &[("omuzaki-gltf", Dev)]), ("omuzaki-gltf", &[])]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn without_the_manifest_layer_a_depended_on_lib_is_just_an_implementation() {
        let got = found(&[("omuzaki-io-gltf", &[("omuzaki-gltf", Normal)]), ("omuzaki-gltf", &[])]);
        assert_eq!(got.len(), 1, "the layer is what makes the dependency legal");
        assert!(
            got[0].starts_with("omuzaki-io-gltf → omuzaki-gltf: implementation crate may not depend on implementation")
        );
    }

    #[test]
    fn an_unknown_layer_and_a_lib_that_is_a_contract_or_mock_are_violations() {
        let bad = |name: &str, layer: &str| {
            let ws = with_layer(workspace(&[(name, &[])]), name, layer);
            violations(&ws).iter().map(ToString::to_string).collect::<Vec<_>>()
        };
        let unknown = bad("omuzaki-gltf", "shared");
        assert_eq!(unknown.len(), 1);
        assert!(unknown[0].contains("unknown layer \"shared\""), "{}", unknown[0]);
        for name in ["omuzaki-doc-api", "omuzaki-doc-mock", "omuzaki-cli", "omuzaki-core"] {
            let got = bad(name, "lib");
            assert_eq!(got.len(), 1, "{name}: a contract, mock or app can't exempt itself from the table");
            assert!(got[0].contains("only implementation crates may"), "{}", got[0]);
        }
    }

    #[test]
    fn implementation_to_implementation_is_caught() {
        let got = found(&[("omuzaki-audio", &[("omuzaki-render", Normal)]), ("omuzaki-render", &[])]);
        assert_eq!(got.len(), 1);
        assert!(got[0].starts_with(
            "omuzaki-audio → omuzaki-render: implementation crate may not depend on implementation crate"
        ));
    }

    #[test]
    fn implementation_dev_dependency_on_implementation_is_still_caught() {
        assert_eq!(found(&[("omuzaki-audio", &[("omuzaki-render", Dev)]), ("omuzaki-render", &[])]).len(), 1);
    }

    #[test]
    fn a_dev_dependency_on_itself_is_fine_and_only_under_dev() {
        assert_clean(&[("omuzaki-stream", &[("omuzaki-stream", Dev)])]);
        assert_eq!(found(&[("omuzaki-stream", &[("omuzaki-stream", Normal)])]).len(), 1);
    }

    #[test]
    fn the_stage_links_every_implementation_but_implementations_still_may_not_couple() {
        // The character stage is wiring, like the CLI and the UI (crates/README.md): it may depend on all of them.
        assert_clean(&[
            (
                "omuzaki-stage",
                &[
                    ("omuzaki-char", Normal),
                    ("omuzaki-face", Normal),
                    ("omuzaki-rig", Normal),
                    ("omuzaki-anim", Normal),
                    ("omuzaki-render", Normal),
                ],
            ),
            ("omuzaki-char", &[]),
            ("omuzaki-face", &[]),
            ("omuzaki-rig", &[]),
            ("omuzaki-anim", &[]),
            ("omuzaki-render", &[]),
        ]);
        // ...but giving the stage that reach must not loosen the rule between implementations.
        let got = found(&[("omuzaki-char", &[("omuzaki-face", Normal)]), ("omuzaki-face", &[])]);
        assert_eq!(got.len(), 1);
        assert!(
            got[0].starts_with(
                "omuzaki-char → omuzaki-face: implementation crate may not depend on implementation crate"
            )
        );
    }

    #[test]
    fn mocks_are_dev_only_for_every_non_app_crate() {
        let mock = ("omuzaki-doc-mock", &[][..]);
        for (from, kind) in [
            ("omuzaki-render", Normal),
            ("omuzaki-render", Build),
            ("omuzaki-ops-api", Normal),
            ("omuzaki-x-mock", Normal),
        ] {
            assert_eq!(found(&[(from, &[("omuzaki-doc-mock", kind)]), mock]).len(), 1, "{from} {kind:?}");
        }
        assert_clean(&[("omuzaki-render", &[("omuzaki-doc-mock", Dev)]), mock]);
    }

    #[test]
    fn contracts_and_mocks_must_not_reach_up() {
        let got = found(&[
            ("omuzaki-doc-api", &[("omuzaki-render", Normal)]),
            ("omuzaki-doc-mock", &[("omuzaki-render", Normal)]),
            ("omuzaki-render", &[]),
        ]);
        assert_eq!(got.len(), 2);
        assert!(
            got[0].starts_with("omuzaki-doc-api → omuzaki-render: API crate may not depend on implementation crate")
        );
        assert!(got[1].starts_with("omuzaki-doc-mock → omuzaki-render: mock crate"));
    }

    #[test]
    fn nothing_but_apps_depends_on_apps() {
        let got =
            found(&[("omuzaki-render", &[("omuzaki-test", Dev)]), ("omuzaki-test", &[("omuzaki-render", Normal)])]);
        assert_eq!(got.len(), 1);
        assert!(got[0].starts_with("omuzaki-render → omuzaki-test"));
    }

    #[test]
    fn external_crates_are_not_this_lints_business() {
        assert_clean(&[("omuzaki-doc-api", &[("serde", Normal), ("omuzaki-render-wannabe", Normal)])]);
    }

    #[test]
    fn the_message_names_the_section() {
        let got = found(&[("omuzaki-audio", &[("omuzaki-render", Build)]), ("omuzaki-render", &[])]);
        assert!(got[0].contains("[build-dependencies]"), "{}", got[0]);
    }

    fn wasm_found(ws: &Workspace) -> Vec<String> {
        wasm_violations(ws).iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_wasm_crate_may_not_reach_a_native_only_crate_directly_or_through_another() {
        let ws = without_wasm(
            workspace(&[
                ("omuzaki-web", &[("omuzaki-view", Normal)]),
                ("omuzaki-view", &[("omuzaki-audio", Normal)]),
                ("omuzaki-audio", &[]),
            ]),
            "omuzaki-audio",
        );
        assert_eq!(
            wasm_found(&ws),
            [
                "omuzaki-view → omuzaki-audio: builds for wasm but depends on a native-only crate (it opts out with \
                 `wasm = false`), through omuzaki-view → omuzaki-audio; opt out with `wasm = false` under \
                 [package.metadata.omuzaki] or drop the dependency",
                "omuzaki-web → omuzaki-audio: builds for wasm but depends on a native-only crate (it opts out with \
                 `wasm = false`), through omuzaki-web → omuzaki-view → omuzaki-audio; opt out with `wasm = false` \
                 under [package.metadata.omuzaki] or drop the dependency",
            ]
        );
    }

    #[test]
    fn wasmtime_and_the_plugin_host_are_native_only_whatever_their_manifest() {
        let ws = workspace(&[
            ("omuzaki-web", &[("wasmtime", Normal), ("omuzaki-plugin", Normal)]),
            ("omuzaki-plugin", &[("wasmtime", Normal)]),
        ]);
        let got = wasm_found(&ws);
        assert_eq!(got.len(), 3, "{got:?}");
        assert!(got[0].starts_with("omuzaki-plugin → wasmtime: builds for wasm"), "{}", got[0]);
        assert!(got[1].starts_with("omuzaki-web → wasmtime: builds for wasm"), "{}", got[1]);
        assert!(got[1].contains("(wasmtime is native only)"), "{}", got[1]);
        assert!(got[2].contains("(the plugin host is native only)"), "{}", got[2]);
        // Opted out, the host may depend on wasmtime and nothing is reported for it.
        let host_ok = without_wasm(workspace(&[("omuzaki-plugin", &[("wasmtime-wasi", Normal)])]), "omuzaki-plugin");
        assert_eq!(wasm_found(&host_ok), Vec::<String>::new());
    }

    #[test]
    fn dev_and_build_dependencies_and_native_crates_are_not_walked() {
        let ws = without_wasm(
            without_wasm(
                workspace(&[
                    ("omuzaki-web", &[("omuzaki-audio", Dev), ("wasmtime", Build)]),
                    ("omuzaki-cli", &[("omuzaki-audio", Normal)]),
                    ("omuzaki-audio", &[]),
                ]),
                "omuzaki-audio",
            ),
            "omuzaki-cli",
        );
        assert_eq!(wasm_found(&ws), Vec::<String>::new());
        assert_eq!(found(&[("xtask", &[("wasmtime", Normal)])]), Vec::<String>::new(), "xtask is host-only");
    }
}
