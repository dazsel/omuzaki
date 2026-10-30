//! The kit's design library (#2612, #2615): the named shape dials and the age model in `library/dials/`, embedded by
//! `build.rs` and registered once with the character contract's design layer
//! ([`omuzaki_char_api::design`]). Adding a dial is adding a `[[dial]]` table to a file.

use omuzaki_char_api::design::{AgeModel, DesignLibrary, DialDef};
use serde::Deserialize;

include!(concat!(env!("OUT_DIR"), "/dials.rs"));

/// One file of `library/dials/`: dials, and at most one age model across the folder.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DialFile {
    #[serde(default)]
    dial: Vec<DialDef>,
    #[serde(default)]
    age: Option<AgeModel>,
}

/// Parses every file of `library/dials/`.
///
/// # Errors
/// The file that doesn't parse, an invalid dial, or a second age model.
pub fn load() -> Result<DesignLibrary, String> {
    let mut lib = DesignLibrary::default();
    for (stem, text) in FILES {
        let file: DialFile = toml::from_str(text).map_err(|e| format!("library/dials/{stem}.toml: {e}"))?;
        for dial in &file.dial {
            dial.check().map_err(|e| format!("library/dials/{stem}.toml: {e}"))?;
        }
        lib.dials.extend(file.dial);
        if let Some(age) = file.age {
            if !lib.age.is_empty() {
                return Err(format!("library/dials/{stem}.toml: a second age model"));
            }
            lib.age = age;
        }
    }
    Ok(lib)
}

omuzaki_char_api::register_design_library!(load);

/// The embedded files, as `(file stem, text)`, for the tests.
pub fn files() -> &'static [(&'static str, &'static str)] {
    FILES
}
