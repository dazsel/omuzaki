//! The shipped effect presets (particles.md D9): every file parses, keeps its rules and is found by name, and a typo
//! is refused with the nearest names.

use omuzaki_stage::effects::presets;
use omuzaki_stage_api::{EffectError, EffectKind, WeatherDef};

#[test]
fn every_shipped_preset_parses_and_keeps_its_rules() {
    let all = presets::load_all().expect("every preset file loads");
    let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["confetti", "emote-rain", "hit-stars", "petals", "rain", "snow"]);
    assert_eq!(presets::presets().len(), all.len(), "the library holds every file");
    let weather = all.iter().filter(|p| p.kind == EffectKind::Weather).count();
    assert_eq!(weather, 3, "rain, snow and petals are weather");
    for preset in &all {
        if let Some(def) = preset.weather_def(1.0) {
            def.check().unwrap_or_else(|(path, rule)| panic!("{}: weather.{path} {rule}", preset.name));
        }
    }
}

#[test]
fn the_list_rows_carry_each_burst_count() {
    let rows = presets::infos();
    let confetti = rows.iter().find(|r| r.name.as_str() == "confetti").expect("confetti listed");
    assert_eq!(confetti.kind, EffectKind::Burst);
    assert!(confetti.default_count.is_some_and(|c| c > 0));
    let rain = rows.iter().find(|r| r.name.as_str() == "rain").expect("rain listed");
    assert_eq!(rain.default_count, None);
}

#[test]
fn a_typo_names_the_nearest_presets() {
    let Err(EffectError::UnknownPreset { name, nearest, .. }) = presets::find("confeti") else {
        panic!("a typo is an unknown preset")
    };
    assert_eq!(name, "confeti");
    assert_eq!(nearest.first().map(String::as_str), Some("confetti"));
    let Err(EffectError::UnknownPreset { nearest, .. }) = presets::find("xyzzyplugh") else { panic!("unknown") };
    assert!(nearest.is_empty(), "nothing is near a word that isn't close: {nearest:?}");
}

#[test]
fn a_preset_of_the_other_kind_says_which_op_takes_it() {
    let Err(EffectError::Invalid(why)) = presets::find_kind("confetti", EffectKind::Weather) else {
        panic!("confetti is not weather")
    };
    assert!(why.contains("stage.effect.burst"), "{why}");
    assert!(presets::find_kind("snow", EffectKind::Weather).is_ok());
}

#[test]
fn a_misspelt_key_or_a_wrong_name_is_refused_with_the_file() {
    let text = include_str!("../library/effects/snow.toml");
    let misspelt = text.replacen("density", "densty", 1);
    let Err(EffectError::Invalid(why)) = presets::parse("snow", &misspelt) else { panic!("misspelt key refused") };
    assert!(why.contains("library/effects/snow.toml") && why.contains("density"), "{why}");
    let Err(EffectError::Invalid(why)) = presets::parse("sleet", text) else { panic!("name must be the file's") };
    assert!(why.contains("file's name"), "{why}");
}

#[test]
fn weather_materialises_its_values_at_an_intensity() {
    let rain = presets::find("rain").expect("rain");
    let def: WeatherDef = rain.weather_def(0.5).expect("rain is weather");
    assert_eq!(def.intensity, 0.5);
    assert_eq!(def.preset.as_ref().map(omuzaki_core::id::Slug::as_str), Some("rain"));
    assert_eq!(def.particles, rain.particles);
    assert!(presets::find("confetti").expect("confetti").weather_def(1.0).is_none());
}
