//! The design library: the dials, the anatomy's structures and the age model, shipped as data by a crate and registered once
//! ([`register_design_library!`](crate::register_design_library!)), as the look presets are. `omuzaki-char` ships the
//! kit's (`library/dials/*.toml`, `library/age.toml`).

use std::sync::OnceLock;

use super::age::AgeModel;
use super::anatomy::StructureDef;
use super::dial::DialDef;
use crate::character::nearest_name;

/// What a crate ships: dials and, at most once across crates, the age model.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DesignLibrary {
    /// Every dial, sorted by id.
    pub dials: Vec<DialDef>,
    /// Every anatomy structure (#2614), sorted by id.
    pub structures: Vec<StructureDef>,
    /// How age moves the dials' owners: the skin, the hair. Empty if no crate ships one.
    pub age: AgeModel,
}

/// A crate's design library: one registration per crate.
pub struct DesignSource {
    /// Parses the crate's files. An error names the file that doesn't load.
    pub load: fn() -> Result<DesignLibrary, String>,
}

inventory::collect!(DesignSource);

/// Registers a crate's design library: `$load` is a `fn() -> Result<DesignLibrary, String>`.
#[macro_export]
macro_rules! register_design_library {
    ($load:path) => {
        $crate::__inventory::submit! { $crate::design::DesignSource { load: $load } }
        $crate::__record_registration!("design_library", $load);
    };
}

/// Every registered dial and the age model, merged once (registrations are fixed at link time).
///
/// # Errors
/// A source that doesn't load, two dials with one id, an invalid dial, or two age models.
pub fn design_library() -> Result<&'static DesignLibrary, String> {
    static LIBRARY: OnceLock<Result<DesignLibrary, String>> = OnceLock::new();
    LIBRARY.get_or_init(merge).as_ref().map_err(Clone::clone)
}

fn merge() -> Result<DesignLibrary, String> {
    let mut all = DesignLibrary::default();
    for source in inventory::iter::<DesignSource> {
        let lib = (source.load)()?;
        all.dials.extend(lib.dials);
        all.structures.extend(lib.structures);
        if !lib.age.is_empty() {
            if !all.age.is_empty() {
                return Err("two crates ship an age model: a build has one".to_owned());
            }
            all.age = lib.age;
        }
    }
    all.dials.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(pair) = all.dials.windows(2).find(|p| p[0].id == p[1].id) {
        return Err(format!("two dials are called `{}`", pair[0].id));
    }
    for dial in &all.dials {
        dial.check()?;
    }
    all.structures.sort_by(|a, b| a.id.cmp(&b.id));
    if let Some(pair) = all.structures.windows(2).find(|p| p[0].id == p[1].id) {
        return Err(format!("two anatomy structures are called `{}`", pair[0].id));
    }
    for structure in &all.structures {
        structure.check()?;
    }
    Ok(all)
}

/// The dial `id`.
///
/// # Errors
/// The library's errors, or an unknown id with the nearest one.
pub fn find_dial(id: &str) -> Result<DialDef, String> {
    let lib = design_library()?;
    if let Some(d) = lib.dials.iter().find(|d| d.id.as_str() == id) {
        return Ok(d.clone());
    }
    match nearest_name(id, lib.dials.iter().map(|d| d.id.as_str())) {
        Some(near) => Err(format!("there is no dial `{id}`; did you mean `{near}`?")),
        None => Err(format!(
            "there is no dial `{id}` ({} dials are registered: `shape.list_dials` lists them)",
            lib.dials.len()
        )),
    }
}

/// The anatomy structure `id`.
///
/// # Errors
/// The library's errors, or an unknown id with the nearest one.
pub fn find_structure(id: &str) -> Result<&'static StructureDef, String> {
    let lib = design_library()?;
    if let Some(s) = lib.structures.iter().find(|s| s.id.as_str() == id) {
        return Ok(s);
    }
    match nearest_name(id, lib.structures.iter().map(|s| s.id.as_str())) {
        Some(near) => Err(format!("there is no anatomy structure `{id}`; did you mean `{near}`?")),
        None => Err(format!(
            "there is no anatomy structure `{id}` ({} are registered: `anatomy.list` lists them)",
            lib.structures.len()
        )),
    }
}
