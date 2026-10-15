//! `cargo xtask lint-registers`: a crate that makes a registration says so in its manifest (ADR 0054 section 6).
//!
//! Why: an App's `link()` calls are generated from the dependencies whose manifest sets `registers = true` under
//! `[package.metadata.omuzaki]` (`omuzaki-link-build`). A crate that calls `register_*!` but lacks the flag would be
//! left out of every generated list, and its registrations would depend on whoever remembered to name it by hand. The
//! link gate would catch the loss in an App's tests; this lint names the cause, without building anything.

use std::process::ExitCode;

use xtask::registry::Sources;

use crate::error::Result;
use crate::workspace::Workspace;

/// One line per crate whose library registers something but whose manifest does not set `registers = true`, sorted.
pub fn violations(sources: &Sources) -> Vec<String> {
    let mut found: Vec<String> = sources
        .crates()
        .iter()
        .filter(|krate| !krate.krate.registers)
        .filter_map(|krate| {
            let registrations = krate.library_registrations();
            let first = registrations.iter().min()?;
            Some(format!(
                "{}: makes {} registration(s) (the first is register_{}!({}) at {}:{}) but sets no `registers = true` \
                 under [package.metadata.omuzaki]",
                krate.krate.name,
                registrations.len(),
                first.kind,
                first.name,
                first.file,
                first.line
            ))
        })
        .collect();
    found.sort();
    found
}

/// Prints every violation and a summary; returns whether there were none.
pub fn report(workspace: &Workspace) -> Result<bool> {
    let sources = Sources::scan(&workspace.root)?;
    let found = violations(&sources);
    for violation in &found {
        println!("{violation}");
    }
    if found.is_empty() {
        println!("lint-registers: ok ({} crates)", sources.crates().len());
    } else {
        println!(
            "lint-registers: {} violation(s); Apps generate their link() calls from `registers = true` \
             (docs/decisions/0054-features-are-added-not-edited-in.md)",
            found.len()
        );
    }
    Ok(found.is_empty())
}

/// The `lint-registers` command.
pub fn run(workspace: &Workspace) -> Result<ExitCode> {
    Ok(if report(workspace)? { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}
