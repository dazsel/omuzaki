//! `cargo xtask lint-links`: the wiring that keeps self-registrations in every binary on every target (ADR 0015).
//!
//! A crate is *linkable* when it has a library that registers something, defines `pub fn link()`, or depends on a
//! linkable crate. The rules:
//! 1. A linkable crate defines `link()` at the root of its library (`src/lib.rs`), with `omuzaki_core::define_link!`.
//! 2. A crate that depends on a linkable crate under `[dependencies]` calls `<that_crate>::link()` somewhere in its
//!    sources: from its own `link()` if other crates link it, at startup if it is an App binary. A crate that lists
//!    `omuzaki-link-build` under `[build-dependencies]` has the calls generated for every dependency that sets
//!    `registers = true`, so for those dependencies it needs no call in its sources (ADR 0054 section 6).
//!
//! So an App that calls its dependencies' `link()` reaches every registering crate through a chain of real calls.
//! On native targets naming the crate is enough to keep its registrations (rustc references every `#[used]` static
//! of a linked crate); on wasm the call is what keeps the crate's single object file (one codegen unit there, see
//! `.cargo/config.toml`).

use std::collections::{BTreeMap, BTreeSet};

use super::{ScannedCrate, Sources};

/// Every rule violation, one line each, sorted; empty when the wiring is complete.
pub fn lint_links(sources: &Sources) -> Vec<String> {
    let mut memo = BTreeMap::new();
    let linkable: BTreeSet<&str> = sources
        .crates()
        .iter()
        .filter(|krate| is_linkable(sources, krate, &mut memo))
        .map(|krate| krate.krate.name.as_str())
        .collect();
    let mut out = Vec::new();
    for krate in sources.crates() {
        let name = krate.krate.name.as_str();
        let linkable_deps: Vec<&ScannedCrate> = krate
            .krate
            .deps
            .iter()
            .filter(|dep| linkable.contains(dep.as_str()))
            .filter_map(|dep| sources.get(dep).ok())
            .collect();
        if linkable.contains(name) && !defines_link(krate) {
            let registrations = krate.library_registrations().len();
            let why = if registrations > 0 {
                format!("its library makes {registrations} registration(s)")
            } else {
                let names: Vec<&str> = linkable_deps.iter().map(|dep| dep.krate.name.as_str()).collect();
                format!(
                    "it links crates that register ({}), and the crates that link it reach them through its `link()`",
                    names.join(", ")
                )
            };
            let root = krate.krate.lib.as_ref().map_or_else(String::new, |(_, root)| root.display().to_string());
            out.push(format!("{name}: defines no `link()` (`omuzaki_core::define_link!`) in {root}, but {why}"));
        }
        if krate.krate.lists_link_build && !krate.krate.generates_links {
            out.push(format!(
                "{name}: lists `omuzaki-link-build`, but its generated link list is not wired: `build.rs` `main` must call \
                 `omuzaki_link_build::emit()`, and `define_link!` must contain an unconditional (no attribute, not nested) \
                 `include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"))`"
            ));
        }
        let calls = krate.link_calls();
        for dep in linkable_deps {
            // A linkable crate always has a library.
            let Some((ident, _)) = &dep.krate.lib else { continue };
            // A crate with a generated list calls every dependency that sets `registers = true`, and no source says so.
            let generated = krate.krate.generates_links && dep.krate.registers;
            if !generated && !calls.contains(&ident.as_str()) {
                out.push(format!(
                    "{name} → {}: never calls `{ident}::link()`, so its registrations may not be linked",
                    dep.krate.name
                ));
            }
        }
    }
    out.sort();
    out
}

fn defines_link(krate: &ScannedCrate) -> bool {
    krate.lib_root_scan().is_some_and(|scan| scan.defines_link)
}

/// Whether `krate` is linkable, memoised in `memo`. Cargo forbids dependency cycles, so the recursion ends.
fn is_linkable<'a>(sources: &'a Sources, krate: &'a ScannedCrate, memo: &mut BTreeMap<&'a str, bool>) -> bool {
    if let Some(&known) = memo.get(krate.krate.name.as_str()) {
        return known;
    }
    let mut linkable = false;
    if krate.krate.lib.is_some() {
        linkable = defines_link(krate) || !krate.library_registrations().is_empty();
        for dep in krate.krate.deps.iter().filter_map(|dep| sources.get(dep).ok()) {
            linkable |= is_linkable(sources, dep, memo);
        }
    }
    memo.insert(krate.krate.name.as_str(), linkable);
    linkable
}
