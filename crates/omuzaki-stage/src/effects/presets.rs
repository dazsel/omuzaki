//! The effect presets (particles.md D9): typed TOML files under `library/effects/`, embedded one line per file as the
//! light rigs are, parsed and checked once, and found by name with did-you-mean.

use std::sync::OnceLock;

use omuzaki_core::text::edit_distance;
use omuzaki_stage_api::{EffectError, EffectKind, EffectPreset, PresetInfo};

/// The presets shipped with the crate: (file name, text).
static FILES: [(&str, &str); 6] = [
    ("confetti", include_str!("../../library/effects/confetti.toml")),
    ("emote-rain", include_str!("../../library/effects/emote-rain.toml")),
    ("hit-stars", include_str!("../../library/effects/hit-stars.toml")),
    ("petals", include_str!("../../library/effects/petals.toml")),
    ("rain", include_str!("../../library/effects/rain.toml")),
    ("snow", include_str!("../../library/effects/snow.toml")),
];

/// The presets and their rows, parsed once.
struct Library {
    presets: Vec<EffectPreset>,
    infos: Vec<PresetInfo>,
}

/// Parses one file: TOML, then the did-you-mean key check, the types and the preset's rules; its `name` must be its
/// file's.
///
/// # Errors
/// [`EffectError::Invalid`] naming the file and the rule.
pub fn parse(file: &str, text: &str) -> Result<EffectPreset, EffectError> {
    let in_file = |e: &dyn std::fmt::Display| EffectError::Invalid(format!("library/effects/{file}.toml: {e}"));
    let value: serde_json::Value = toml::from_str(text).map_err(|e| in_file(&e))?;
    let preset = EffectPreset::from_value(&value).map_err(|e| in_file(&e))?;
    if preset.name.as_str() != file {
        return Err(in_file(&format!("name `{}` is not the file's name", preset.name)));
    }
    Ok(preset)
}

/// Every embedded file, parsed, in name order.
///
/// # Errors
/// The first file that doesn't parse or breaks a rule.
pub fn load_all() -> Result<Vec<EffectPreset>, EffectError> {
    FILES.iter().map(|(file, text)| parse(file, text)).collect()
}

fn library() -> &'static Library {
    static LIBRARY: OnceLock<Library> = OnceLock::new();
    LIBRARY.get_or_init(|| {
        // A bad file is a development bug: the test that loads every file fails, and the app runs without it.
        let presets: Vec<EffectPreset> = FILES
            .iter()
            .filter_map(|(file, text)| {
                parse(file, text).map_err(|error| tracing::error!(%error, "an effect preset doesn't load")).ok()
            })
            .collect();
        let infos = presets.iter().map(EffectPreset::info).collect();
        Library { presets, infos }
    })
}

/// The presets, in name order.
pub fn presets() -> &'static [EffectPreset] {
    &library().presets
}

/// Their `stage.effect.list` rows, in name order.
pub fn infos() -> &'static [PresetInfo] {
    &library().infos
}

/// The names within reach of a typo of `name`, nearest first (at most three).
pub fn nearest(name: &str, names: impl Iterator<Item = String>) -> Vec<String> {
    let reach = name.chars().count() / 3 + 2;
    let mut close: Vec<(usize, String)> =
        names.map(|n| (edit_distance(name, &n), n)).filter(|(d, _)| *d <= reach).collect();
    close.sort();
    close.into_iter().take(3).map(|(_, n)| n).collect()
}

/// The preset called `name`.
///
/// # Errors
/// [`EffectError::UnknownPreset`] with the nearest names.
pub fn find(name: &str) -> Result<&'static EffectPreset, EffectError> {
    presets().iter().find(|p| p.name.as_str() == name).ok_or_else(|| {
        EffectError::unknown_preset(name, nearest(name, presets().iter().map(|p| p.name.as_str().to_owned())))
    })
}

/// The preset called `name`, which must be of `kind`.
///
/// # Errors
/// As [`find`], and [`EffectError::Invalid`] for a preset of the other kind, saying which op takes it.
pub fn find_kind(name: &str, kind: EffectKind) -> Result<&'static EffectPreset, EffectError> {
    let preset = find(name)?;
    if preset.kind == kind {
        return Ok(preset);
    }
    let (is, use_op) = match preset.kind {
        EffectKind::Burst => ("a burst", "stage.effect.burst"),
        EffectKind::Weather => ("weather", "stage.set_weather or stage.effect.weather"),
    };
    Err(EffectError::Invalid(format!("{name} is {is}; use {use_op}")))
}
