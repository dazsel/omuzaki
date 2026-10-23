//! The effect contract (particles D9 to D14): the design's §3 confetti preset and rain weather files round trip and
//! validate, a misspelt key names the nearest one, scenes read and write `weather`, the weather rules refuse, a chat
//! `EmoteSpan` decodes as a `ChatEmoteRef`, and the request types check their ranges.

#![allow(clippy::unwrap_used, clippy::float_cmp, clippy::needless_pass_by_value)]

use std::path::Path;

use omuzaki_core::id::Slug;
use omuzaki_stage_api::effects::{
    AreaBox, ColourSource, EffectEventKind, MAX_BURST_COUNT, WeatherOverride, WeatherSource,
};
use omuzaki_stage_api::*;
use serde_json::{Value, json};

fn toml_value(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/effects").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn confetti() -> EffectPreset {
    EffectPreset::from_value(&toml_value("confetti.toml")).unwrap()
}

fn rain() -> WeatherDef {
    serde_json::from_value(toml_value("weather.toml")).unwrap()
}

#[test]
fn the_confetti_preset_parses_validates_and_writes_back_to_the_same_values() {
    let preset = confetti();
    assert_eq!((preset.name.as_str(), preset.kind), ("confetti", EffectKind::Burst));
    let burst = preset.burst.as_ref().unwrap();
    assert_eq!((burst.count, burst.colours), (150, ColourSource::Palette));
    assert_eq!(burst.at, EffectAt { offset: [0.0, 0.0, 0.15], ..EffectAt::new(EffectOrigin::Subject, Some("chest")) });
    assert_eq!(preset.sprite.shape(), Some(omuzaki_render_api::SpriteShape::rect(0.6)));
    let written = toml::to_string(&preset).unwrap();
    let again = EffectPreset::from_value(&toml::from_str(&written).unwrap()).unwrap();
    assert_eq!(again, preset);
    assert_eq!(
        preset.info(),
        PresetInfo {
            name: Slug::new("confetti").unwrap(),
            title: "Confetti".into(),
            summary: preset.summary.clone(),
            kind: EffectKind::Burst,
            default_count: Some(150),
        }
    );
    assert!(preset.weather_def(1.0).is_none(), "a burst is no weather");
}

#[test]
fn the_rain_weather_file_parses_validates_and_writes_back_to_the_same_values() {
    let weather = rain();
    assert_eq!(weather.preset.as_ref().map(Slug::as_str), Some("rain"));
    assert_eq!((weather.intensity, weather.density, weather.height), (0.6, 160.0, 2.6));
    assert_eq!(weather.area, WeatherArea::AUTO);
    assert_eq!(weather.check(), Ok(()));
    let back: WeatherDef = toml::from_str(&toml::to_string(&weather).unwrap()).unwrap();
    assert_eq!(back, weather);
    assert_eq!(serde_json::to_value(&weather).unwrap()["area"], json!("auto"));
}

#[test]
fn a_misspelt_key_is_refused_with_the_nearest_name() {
    for (table, wrong, right) in [
        (None, "summery", "summary"),
        (Some("burst"), "cuont", "count"),
        (Some("particles"), "lifetme", "lifetime"),
        (Some("particles"), "gravty", "gravity"),
    ] {
        let mut value = toml_value("confetti.toml");
        let at = match table {
            Some(t) => value.get_mut(t).unwrap(),
            None => &mut value,
        };
        let map = at.as_object_mut().unwrap();
        let v = map.remove(right).unwrap();
        map.insert(wrong.into(), v);
        let err = EffectPreset::from_value(&value).unwrap_err().to_string();
        assert!(err.contains(&format!("did you mean `{right}`")), "{wrong}: {err}");
    }
}

#[test]
fn a_preset_must_carry_its_kinds_block_and_a_weather_one_a_floor() {
    let mut burst_as_weather = toml_value("confetti.toml");
    burst_as_weather["kind"] = json!("weather");
    assert!(EffectPreset::from_value(&burst_as_weather).unwrap_err().to_string().contains("[weather]"));

    let mut too_many = toml_value("confetti.toml");
    too_many["burst"]["count"] = json!(MAX_BURST_COUNT + 1);
    assert!(EffectPreset::from_value(&too_many).unwrap_err().to_string().contains("burst.count"));

    let weather = rain();
    let mut preset = confetti();
    preset.kind = EffectKind::Weather;
    preset.burst = None;
    preset.sprite = weather.sprite;
    preset.particles = weather.particles.clone();
    preset.weather = Some(effects::WeatherDefaults { density: 160.0, height: 2.6, area: WeatherArea::AUTO });
    assert_eq!(preset.validate(), Ok(()));
    let materialised = preset.weather_def(0.6).unwrap();
    assert_eq!(WeatherDef { preset: weather.preset.clone(), ..materialised }, weather);
    preset.particles.floor = None;
    assert!(preset.validate().unwrap_err().to_string().contains("floor"));
}

fn scene_with_weather(weather: Value) -> Value {
    json!({ "scene": { "name": "Cafe" }, "weather": weather })
}

#[test]
fn scenes_read_and_write_weather_only_when_present() {
    let without = SceneDef::from_node(&json!({ "scene": { "name": "Cafe" } })).unwrap();
    assert_eq!(without.weather, None);
    let mut node = Value::Null;
    without.to_node(&mut node).unwrap();
    assert!(node.get("weather").is_none(), "no weather, no key");

    let with = SceneDef::from_node(&scene_with_weather(toml_value("weather.toml"))).unwrap();
    assert_eq!(with.weather, Some(rain()));
    assert!(with.check().is_ok(), "{:?}", with.check());
    let mut node = Value::Null;
    with.to_node(&mut node).unwrap();
    assert_eq!(SceneDef::from_node(&node).unwrap(), with);
    assert_eq!(node["weather"]["intensity"], json!(0.6), "written as typed");

    // Taking the weather away removes the key and keeps the rest of the node.
    let mut node = scene_with_weather(toml_value("weather.toml"));
    without.to_node(&mut node).unwrap();
    assert!(node.get("weather").is_none());
}

fn refused(edit: impl FnOnce(&mut Value)) -> SceneError {
    let mut weather = toml_value("weather.toml");
    edit(&mut weather);
    SceneDef::from_node(&scene_with_weather(weather)).unwrap().check().unwrap_err()
}

#[test]
fn the_weather_rules_refuse_with_the_path() {
    let e = refused(|w| w["intensity"] = json!(2.0));
    assert_eq!(e.path, "weather.intensity");
    assert_eq!(refused(|w| w["density"] = json!(0.0)).path, "weather.density");
    assert_eq!(refused(|w| w["height"] = json!(-1.0)).path, "weather.height");
    let e = refused(|w| w["area"] = json!({ "anchor": "stage", "min": [0.0, 0.0, 0.0], "max": [1.0, -1.0, 1.0] }));
    assert_eq!((e.path.as_str(), e.rule.as_str()), ("weather.area", "min.y is above max.y"));
    let e = refused(|w| w["area"] = json!({ "anchor": "member:kiri", "min": [0.0, 0.0, 0.0], "max": [1.0, 1.0, 1.0] }));
    assert_eq!(e.path, "weather.area.anchor");
    assert_eq!(refused(|w| w["particles"]["coverage"] = json!(3.0)).path, "weather.particles");
    assert_eq!(refused(|w| w["sprite"] = json!({ "atlas": "chat-emotes" })).path, "weather.sprite");
    let e = refused(|w| w["particles"]["orientation"] = json!({ "kind": "billboard" }));
    assert_eq!((e.path.as_str(), e.rule.contains("streak")), ("weather.sprite", true));
}

#[test]
fn a_box_area_reads_and_its_anchor_must_be_cast() {
    let area: WeatherArea =
        serde_json::from_value(json!({ "min": [-2.0, 0.0, -1.0], "max": [2.0, 3.0, 1.0] })).unwrap();
    assert_eq!(
        area,
        WeatherArea::Box(AreaBox { anchor: Anchor::Subject, min: [-2.0, 0.0, -1.0], max: [2.0, 3.0, 1.0] })
    );
    assert!(serde_json::from_value::<WeatherArea>(json!("manual")).is_err());
}

#[test]
fn a_chat_emote_span_decodes_as_an_emote_ref() {
    let span =
        json!({ "start": 4, "end": 11, "id": "25", "name": "Kappa", "image": "https://static-cdn.example/25.png" });
    let emote: ChatEmoteRef = serde_json::from_value(span).unwrap();
    assert_eq!(emote.name, "Kappa");
    let spans = json!({ "preset": "emote-rain", "emotes": [{ "start": 0, "end": 3, "id": "1", "name": "Hey" }] });
    let request: BurstRequest = serde_json::from_value(spans).unwrap();
    assert_eq!(request.emotes.len(), 1);
    assert_eq!(request.validate(), Ok(()));
}

#[test]
fn origins_read_and_write_as_strings_and_points_fit_their_origin() {
    for text in ["subject", "stage", "camera", "world", "member:kiri", "prop:mug"] {
        let origin: EffectOrigin = text.parse().unwrap();
        assert_eq!(origin.to_string(), text);
    }
    for bad in ["member:stage", "Subject", "member:", "hand"] {
        assert!(bad.parse::<EffectOrigin>().is_err(), "{bad}");
    }
    assert!(EffectAt::new(EffectOrigin::Camera, Some("top")).validate().is_ok());
    assert!(EffectAt::new(EffectOrigin::Camera, Some("chest")).validate().is_err());
    assert!(EffectAt::new(EffectOrigin::Stage, Some("top")).validate().is_err());
    assert!(EffectAt::new(EffectOrigin::Subject, Some("rightHand")).validate().is_ok());
    let at: EffectAt = serde_json::from_value(json!({ "from": "member:kiri", "point": "rightHand" })).unwrap();
    assert_eq!(at.from, EffectOrigin::Member(MemberId::new("kiri").unwrap()));
}

#[test]
fn burst_requests_refuse_out_of_range_values_instead_of_clamping() {
    let base = || BurstRequest::new(Slug::new("confetti").unwrap());
    assert_eq!(base().validate(), Ok(()));
    for (request, field) in [
        (BurstRequest { count: Some(0), ..base() }, "count"),
        (BurstRequest { count: Some(1_001), ..base() }, "count"),
        (BurstRequest { colours: Some(vec![]), ..base() }, "colours"),
        (BurstRequest { scale: Some(5.0), ..base() }, "scale"),
        (
            BurstRequest { emotes: vec![ChatEmoteRef { id: "1".into(), name: "a".into(), image: None }; 9], ..base() },
            "emotes",
        ),
        (
            BurstRequest {
                emotes: vec![ChatEmoteRef { id: "1".into(), name: "a".into(), image: Some("http://x/a.png".into()) }],
                ..base()
            },
            "https",
        ),
    ] {
        let err = request.validate().unwrap_err();
        assert!(matches!(err, EffectError::Invalid(ref m) if m.contains(field)), "{field}: {err}");
    }
    assert!(serde_json::from_value::<BurstRequest>(json!({ "preset": "confetti", "cuont": 3 })).is_err());
}

#[test]
fn live_weather_reads_a_preset_none_or_scene() {
    let rain: LiveWeather =
        serde_json::from_value(json!({ "preset": "rain", "intensity": 0.8, "for_s": 60.0 })).unwrap();
    assert_eq!(rain, LiveWeather::preset(Slug::new("rain").unwrap(), 0.8, Some(60.0)));
    let snow: LiveWeather = serde_json::from_value(json!({ "preset": "snow" })).unwrap();
    assert!(matches!(snow, LiveWeather::Preset(WeatherOverride { intensity, for_s: None, .. }) if intensity == 1.0));
    assert_eq!(serde_json::from_value::<LiveWeather>(json!({ "preset": "none" })).unwrap(), LiveWeather::None);
    assert_eq!(serde_json::from_value::<LiveWeather>(json!({ "preset": "scene" })).unwrap(), LiveWeather::Scene);
    assert!(serde_json::from_value::<LiveWeather>(json!({ "preset": "none", "intensity": 1.0 })).is_err());
    let half = LiveWeather::preset(Slug::new("rain").unwrap(), 0.5, Some(60.0));
    assert_eq!(serde_json::to_value(&half).unwrap(), json!({ "preset": "rain", "intensity": 0.5, "for_s": 60.0 }));
    assert!(LiveWeather::preset(Slug::new("rain").unwrap(), 1.5, None).validate().is_err());
}

#[test]
fn events_and_status_round_trip() {
    let event =
        EffectEvent { at: omuzaki_core::time::Seconds(12.5), kind: EffectEventKind::Weather(LiveWeather::Scene) };
    let back: EffectEvent = serde_json::from_value(serde_json::to_value(&event).unwrap()).unwrap();
    assert_eq!(back, event);
    let status =
        EffectStatus { slots: 10, cap: 5_000, weather: WeatherSource::Scene, density: 1.0, ..EffectStatus::default() };
    let back: EffectStatus = serde_json::from_value(serde_json::to_value(&status).unwrap()).unwrap();
    assert_eq!(back, status);
}
