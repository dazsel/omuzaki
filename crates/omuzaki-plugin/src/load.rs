//! Loading a component (design §1.1, §4.1, §4.2): the size cap, the component check, compiling, and the check of its
//! imports and exports against what this host serves and what the manifest says.

use omuzaki_plugin_api::PluginError;
use omuzaki_plugin_api::limits::COMPONENT_MAX_BYTES;
use omuzaki_plugin_api::manifest::Manifest;
use wasmtime::component::Component;

use crate::engine::PluginEngine;

/// The WIT package's name, without a version.
pub const PACKAGE: &str = "omuzaki:plugin";

/// The minors of `omuzaki:plugin` this host serves, newest first: its own and, from 0.2 on, the one before (§1.1).
pub const SERVED: [&str; 1] = ["0.1"];

/// The interfaces of the WIT package a component may import.
const PLUGIN_IMPORTS: [&str; 5] = ["types", "ops", "host", "data", "http"];

/// The interface every plugin exports.
const LIFECYCLE: &str = "lifecycle";

/// The WASI 0.2 interfaces the host stubs (§4.1); a component importing any other is refused.
pub const WASI_STUBS: [&str; 15] = [
    "wasi:cli/environment",
    "wasi:cli/exit",
    "wasi:cli/stdin",
    "wasi:cli/stdout",
    "wasi:cli/stderr",
    "wasi:io/error",
    "wasi:io/poll",
    "wasi:io/streams",
    "wasi:clocks/monotonic-clock",
    "wasi:clocks/wall-clock",
    "wasi:random/random",
    "wasi:random/insecure",
    "wasi:random/insecure-seed",
    "wasi:filesystem/types",
    "wasi:filesystem/preopens",
];

/// The WASI version family the stubs implement.
const WASI_MAJOR_MINOR: &str = "0.2";

/// The preamble of a component binary: magic, then the component-model layer.
const COMPONENT_PREAMBLE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x0d, 0x00, 0x01, 0x00];

/// The preamble of a core module.
const MODULE_PREAMBLE: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

/// A component's interface name split as `<package>/<interface>` and its version.
fn split(name: &str) -> (&str, Option<&str>) {
    match name.split_once('@') {
        Some((interface, version)) => (interface, Some(version)),
        None => (name, None),
    }
}

/// `major.minor` of a semver text (`0.1.0` gives `0.1`).
fn major_minor(version: &str) -> String {
    version.split('.').take(2).collect::<Vec<_>>().join(".")
}

/// Compiles `bytes` after the cheap checks: the size cap and that it is a component, not a core module.
///
/// # Errors
/// [`PluginError::Package`] past [`COMPONENT_MAX_BYTES`]; [`PluginError::Instantiate`] for a core module, a truncated
/// or invalid component.
pub fn compile(engine: &PluginEngine, bytes: &[u8]) -> Result<Component, PluginError> {
    if bytes.len() as u64 > COMPONENT_MAX_BYTES {
        return Err(PluginError::Package(format!(
            "the component is {} bytes; the cap is {COMPONENT_MAX_BYTES}",
            bytes.len()
        )));
    }
    match bytes.get(..8) {
        Some(head) if head == MODULE_PREAMBLE => {
            return Err(PluginError::Instantiate("a core module is not a component; plugins are components".into()));
        }
        Some(head) if head == COMPONENT_PREAMBLE => {}
        _ => return Err(PluginError::Instantiate("not a WebAssembly component".into())),
    }
    Component::new(engine.wasmtime(), bytes).map_err(|e| PluginError::Instantiate(format!("{e:#}")))
}

/// What the component was built against, from its imports and exports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Built {
    /// The full version of `omuzaki:plugin` it names (`0.1.0`).
    pub version: String,
}

/// Checks the component's imports and exports: every import is one of the package's interfaces or a stubbed WASI
/// 0.2 interface, it exports `lifecycle`, it names one version of the package inside this host's window, and the
/// manifest's `api` requirement agrees with that version.
///
/// # Errors
/// [`PluginError::Imports`] listing every interface the host refuses, without its version; [`PluginError::Incompatible`] outside the window;
/// [`PluginError::Manifest`] when `api` disagrees with the binary or `lifecycle` is missing.
pub fn check(engine: &PluginEngine, component: &Component, manifest: &Manifest) -> Result<Built, PluginError> {
    let ty = component.component_type();
    let mut unexpected = Vec::new();
    let mut versions = Vec::new();
    for (name, _) in ty.imports(engine.wasmtime()) {
        let (interface, version) = split(name);
        let ours = interface
            .strip_prefix(PACKAGE)
            .and_then(|rest| rest.strip_prefix('/'))
            .is_some_and(|i| PLUGIN_IMPORTS.contains(&i));
        let wasi = WASI_STUBS.contains(&interface) && version.is_some_and(|v| major_minor(v) == WASI_MAJOR_MINOR);
        match (ours, version) {
            (true, Some(version)) => versions.push(version.to_owned()),
            _ if wasi => {}
            _ => unexpected.push(interface.to_owned()),
        }
    }
    if !unexpected.is_empty() {
        return Err(PluginError::imports(unexpected));
    }
    let lifecycle_prefix = format!("{PACKAGE}/{LIFECYCLE}@");
    let exported: Vec<String> = ty
        .exports(engine.wasmtime())
        .filter_map(|(name, _)| name.strip_prefix(lifecycle_prefix.as_str()).map(str::to_owned))
        .collect();
    let Some(version) = exported.first().cloned() else {
        return Err(PluginError::Manifest(format!("the component does not export {PACKAGE}/{LIFECYCLE}")));
    };
    versions.push(version.clone());
    if let Some(other) = versions.iter().find(|v| major_minor(v) != major_minor(&version)) {
        return Err(PluginError::Manifest(format!("the component mixes {PACKAGE} {version} and {other}")));
    }
    let built_for = major_minor(&version);
    if !SERVED.contains(&built_for.as_str()) {
        return Err(PluginError::incompatible(built_for, SERVED.iter().map(|s| (*s).to_owned()).collect()));
    }
    let parsed = semver::Version::parse(&version)
        .map_err(|_| PluginError::Manifest(format!("the component names {PACKAGE}@{version}, not a semver version")))?;
    let api = semver::VersionReq::parse(&manifest.api)
        .map_err(|_| PluginError::Manifest(format!("`api = \"{}\"` is not a semver requirement", manifest.api)))?;
    if !api.matches(&parsed) {
        return Err(PluginError::Manifest(format!(
            "`api = \"{}\"` disagrees with the component, which was built for {PACKAGE}@{version}",
            manifest.api
        )));
    }
    Ok(Built { version })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_split_into_interface_and_version() {
        assert_eq!(split("wasi:io/poll@0.2.3"), ("wasi:io/poll", Some("0.2.3")));
        assert_eq!(split("thing"), ("thing", None));
        assert_eq!(major_minor("0.2.12"), "0.2");
    }
}
