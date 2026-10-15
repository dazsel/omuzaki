//! The look presets this crate ships (LK-06): one TOML file per preset in `library/looks/`, registered once for
//! the crate with [`register_look_presets!`](omuzaki_char_api::register_look_presets).
//!
//! **Adding a preset is adding a file.** `build.rs` lists the folder and embeds every `<id>.toml` (no list in code names
//! them), [`parse`] reads one, and the registry (`omuzaki_char_api::look_presets`) serves them to the `look.*` ops, the
//! stage and the exports. The format and what a look may change are `omuzaki_char_api::look`'s; a preset is data plus,
//! for a new kind of shading, a program in `omuzaki-render`.
//!
//! The files are parsed once per process: presets are read on every look edit and every build that applies a look, and
//! they never change while it runs.

use std::sync::OnceLock;

use omuzaki_char_api::LookPreset;

include!(concat!(env!("OUT_DIR"), "/looks.rs"));

/// Parses one preset file: a [`LookPreset`] in TOML whose `id` is the file's stem, with valid parameters.
///
/// # Errors
/// A message naming the file and what is wrong with it.
pub fn parse(stem: &str, text: &str) -> Result<LookPreset, String> {
    let preset: LookPreset = toml::from_str(text).map_err(|e| format!("library/looks/{stem}.toml: {e}"))?;
    if preset.id.as_str() != stem {
        return Err(format!("library/looks/{stem}.toml says it is `{}`: a preset's id is its file name", preset.id));
    }
    preset.params.validate().map_err(|e| format!("library/looks/{stem}.toml: {e}"))?;
    Ok(preset)
}

/// Every preset file of the crate, parsed once: the crate's preset source.
fn load() -> Result<Vec<LookPreset>, String> {
    static PRESETS: OnceLock<Result<Vec<LookPreset>, String>> = OnceLock::new();
    PRESETS.get_or_init(|| FILES.iter().map(|(stem, text)| parse(stem, text)).collect()).clone()
}

omuzaki_char_api::register_look_presets!(load);

#[cfg(test)]
mod tests {
    use omuzaki_char_api::{DEFAULT_PRESET, find_look_preset, look_presets};

    use super::*;

    #[test]
    fn every_file_parses_and_the_default_is_shipped() {
        let presets = load().unwrap();
        assert_eq!(presets.len(), FILES.len());
        assert!(look_presets().unwrap().iter().any(|p| p.id.as_str() == DEFAULT_PRESET));
        assert_eq!(find_look_preset(DEFAULT_PRESET).unwrap().name, "Soft SD");
    }

    #[test]
    fn a_file_must_be_named_for_its_id_and_hold_valid_numbers() {
        let text = "id = \"soft\"\nname = \"S\"\nsummary = \"s\"\norigin = \"o\"\n";
        assert!(parse("soft", text).is_ok());
        assert!(parse("other", text).unwrap_err().contains("is its file name"));
        let bad = format!("{text}[params.cloth]\nrim_scale = -1.0\n");
        assert!(parse("soft", &bad).unwrap_err().contains("cloth.rim_scale"));
        let typo = format!("{text}[params.cloth]\nrim_scal = 1.0\n");
        assert!(parse("soft", &typo).unwrap_err().contains("rim_scal"));
    }

    #[test]
    fn soft_sd_is_the_identity_on_materials() {
        let soft = find_look_preset(DEFAULT_PRESET).unwrap().params;
        let classes = omuzaki_char_api::MaterialClass::ALL;
        for class in classes {
            let look = soft.class(class);
            assert_eq!(
                (look.softness_scale, look.rim_scale, look.outline_width_scale),
                (Some(1.0), Some(1.0), Some(1.0))
            );
            assert_eq!(look.program, None);
            assert_eq!(look.outline_width, None);
        }
        assert_eq!(soft.scene.bloom, Some(true));
    }
}
