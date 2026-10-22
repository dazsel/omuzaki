//! The WIT file parses, is the package the crate names, and documents every function.

#![allow(clippy::unwrap_used)] // the helper outside #[test] fns

use omuzaki_plugin_api::{WIT, WIT_PACKAGE};
use wit_parser::Resolve;

fn resolve() -> (Resolve, wit_parser::PackageId) {
    let mut resolve = Resolve::default();
    let id = resolve.push_source("omuzaki-plugin.wit", WIT).unwrap();
    (resolve, id)
}

#[test]
fn the_package_line_equals_wit_package() {
    let (resolve, id) = resolve();
    assert_eq!(resolve.packages[id].name.to_string(), WIT_PACKAGE);
}

#[test]
fn the_base_world_has_the_interfaces_of_the_design() {
    let (resolve, id) = resolve();
    let interfaces: Vec<&str> = resolve.packages[id].interfaces.keys().map(String::as_str).collect();
    assert_eq!(interfaces, ["types", "ops", "host", "data", "http", "lifecycle"]);
    let world = &resolve.worlds[resolve.packages[id].worlds["plugin"]];
    let name = |key: &wit_parser::WorldKey| match key {
        wit_parser::WorldKey::Name(n) => n.clone(),
        wit_parser::WorldKey::Interface(i) => resolve.interfaces[*i].name.clone().unwrap_or_default(),
    };
    // `types` is pulled in by the `use` lines, not named by the world.
    let imports: Vec<String> = world.imports.keys().map(name).filter(|n| n != "types").collect();
    let exports: Vec<String> = world.exports.keys().map(name).collect();
    assert_eq!(imports, ["ops", "host", "data", "http"]);
    assert_eq!(exports, ["lifecycle"]);
}

#[test]
fn every_interface_function_has_a_doc_comment() {
    let (resolve, id) = resolve();
    let mut checked = 0;
    for interface in resolve.packages[id].interfaces.values() {
        for (name, func) in &resolve.interfaces[*interface].functions {
            let docs = func.docs.contents.as_deref().unwrap_or("").trim();
            assert!(!docs.is_empty(), "`{name}` has no doc comment");
            checked += 1;
        }
    }
    assert_eq!(checked, 4 + 2 + 4 + 1 + 3, "a function was added or removed: update this count with the design");
}
