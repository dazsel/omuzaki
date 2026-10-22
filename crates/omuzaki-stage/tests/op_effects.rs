//! The scene's weather and the preset list through ops (particles.md D10, D11; #757): `stage.set_weather` copies a
//! preset's values into the scene as one patch with an exact inverse, setting the same values again changes nothing,
//! a typo names the nearest preset, and the rules refuse what the scene can't hold.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError, Plan};
use omuzaki_stage::effects::presets;
use omuzaki_stage::ops::effects::list::ListEffects;
use omuzaki_stage::ops::effects::set_weather::SetWeather;
use omuzaki_stage::ops::scene::create::CreateScene;
use omuzaki_stage::scene::default::scene_for;
use omuzaki_stage_api::{Anchor, SceneDef, SceneId, WeatherArea};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

fn plan<T: Op + DeserializeOwned>(doc: &MemDoc, args: Value) -> Result<Plan, OpError> {
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let op: T = serde_json::from_value(args).expect("the arguments read");
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

/// Applies the op and checks that undo and redo are exact; returns the patch and the output.
#[allow(clippy::needless_pass_by_value, reason = "every call site passes a json! literal")]
fn edit<T: Op + DeserializeOwned>(doc: &mut MemDoc, args: Value) -> (Patch, Value) {
    let before = doc.root().clone();
    let plan = plan::<T>(doc, args.clone()).unwrap_or_else(|e| panic!("{} {args}: {e:?}", T::meta().name));
    doc.apply(&plan.patch).expect("applies");
    let after = doc.root().clone();
    doc.apply(&plan.patch.inverse()).expect("undoes");
    assert_eq!(doc.root(), &before, "{}: undo is exact", T::meta().name);
    doc.apply(&plan.patch).expect("redoes");
    assert_eq!(doc.root(), &after, "{}: redo is exact", T::meta().name);
    (plan.patch, plan.output.unwrap_or(Value::Null))
}

#[allow(clippy::needless_pass_by_value, reason = "every call site passes a json! literal")]
fn refused<T: Op + DeserializeOwned>(doc: &MemDoc, args: Value) -> OpError {
    match plan::<T>(doc, args.clone()) {
        Ok(_) => panic!("{} {args} should be refused", T::meta().name),
        Err(e) => e,
    }
}

fn scene(doc: &MemDoc) -> SceneDef {
    scene_for(doc, Some(&SceneId::new("main").unwrap())).unwrap().1
}

fn project() -> MemDoc {
    let mut doc = MemDoc::from_value(json!({
        "characters": { "kiri": { "name": "Kiri", "parts": {} } },
        "project": { "default_character": "kiri" },
    }));
    edit::<CreateScene>(&mut doc, json!({ "id": "main" }));
    doc
}

#[test]
fn set_weather_copies_the_preset_into_the_scene_as_one_undoable_patch() {
    let mut doc = project();
    let (patch, output) = edit::<SetWeather>(&mut doc, json!({ "preset": "rain", "intensity": 0.5 }));
    assert_eq!(output, json!({ "scene": "main", "weather": "rain", "intensity": 0.5 }));
    let paths: Vec<String> = patch.changes.iter().map(|c| c.path.to_string()).collect();
    assert_eq!(paths, ["scenes/main/weather"], "a new weather is one path");
    let weather = scene(&doc).weather.expect("the scene has weather");
    let rain = presets::find("rain").unwrap();
    assert_eq!(weather, rain.weather_def(0.5).unwrap(), "the preset's values, materialised");
    assert_eq!(weather.area, WeatherArea::AUTO);
}

#[test]
fn setting_the_same_weather_again_is_an_empty_patch_and_a_change_is_one_leaf() {
    let mut doc = project();
    edit::<SetWeather>(&mut doc, json!({ "preset": "snow", "intensity": 0.5 }));
    let again = plan::<SetWeather>(&doc, json!({ "preset": "snow", "intensity": 0.5 })).unwrap();
    assert!(again.patch.changes.is_empty(), "nothing changed: {:?}", again.patch);
    let (patch, _) = edit::<SetWeather>(&mut doc, json!({ "preset": "snow", "intensity": 0.25 }));
    let paths: Vec<String> = patch.changes.iter().map(|c| c.path.to_string()).collect();
    assert_eq!(paths, ["scenes/main/weather/intensity"]);
}

#[test]
fn none_removes_the_weather_and_a_box_area_is_kept() {
    let mut doc = project();
    let area = json!({ "anchor": "stage", "min": [-2.0, 0.0, -2.0], "max": [2.0, 0.0, 1.0] });
    edit::<SetWeather>(&mut doc, json!({ "preset": "petals", "area": area }));
    let weather = scene(&doc).weather.unwrap();
    assert_eq!(weather.area, WeatherArea::boxed(Anchor::Stage, [-2.0, 0.0, -2.0], [2.0, 0.0, 1.0]));
    let (patch, output) = edit::<SetWeather>(&mut doc, json!({ "preset": "none" }));
    assert_eq!(output["weather"], Value::Null);
    assert_eq!(patch.changes.len(), 1, "removing is one path");
    assert!(scene(&doc).weather.is_none());
    assert!(doc.get(&omuzaki_stage_api::paths::weather(&SceneId::new("main").unwrap())).is_none());
}

#[test]
fn a_typo_names_the_nearest_preset_and_a_burst_preset_names_its_op() {
    let doc = project();
    let error = refused::<SetWeather>(&doc, json!({ "preset": "sonw" }));
    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(error.field.as_deref(), Some("preset"));
    assert!(error.hint.as_deref().is_some_and(|h| h.contains("'snow'")), "{error:?}");
    let error = refused::<SetWeather>(&doc, json!({ "preset": "confetti" }));
    assert_eq!(error.code, ErrorCode::Invalid);
    assert!(error.message.contains("stage.effect.burst"), "{error:?}");
}

#[test]
fn the_rules_refuse_what_the_scene_cannot_hold() {
    let doc = project();
    let error = refused::<SetWeather>(&doc, json!({ "preset": "rain", "intensity": 2.0 }));
    assert_eq!(error.field.as_deref(), Some("intensity"));
    let error = refused::<SetWeather>(&doc, json!({ "preset": "none", "intensity": 0.5 }));
    assert_eq!(error.field.as_deref(), Some("intensity"));
    let backwards = json!({ "anchor": "stage", "min": [2.0, 0.0, 0.0], "max": [-2.0, 0.0, 1.0] });
    let error = refused::<SetWeather>(&doc, json!({ "preset": "rain", "area": backwards }));
    assert_eq!(error.field.as_deref(), Some("weather.area"));
    let ghost = json!({ "anchor": "member:ghost", "min": [0.0, 0.0, 0.0], "max": [1.0, 0.0, 1.0] });
    let error = refused::<SetWeather>(&doc, json!({ "preset": "rain", "area": ghost }));
    assert_eq!(error.field.as_deref(), Some("weather.area.anchor"));
    let error = refused::<SetWeather>(&doc, json!({ "scene": "nowhere", "preset": "rain" }));
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[test]
fn the_list_filters_by_kind() {
    let doc = project();
    let all = plan::<ListEffects>(&doc, json!({})).unwrap().output.unwrap();
    assert_eq!(all["presets"].as_array().unwrap().len(), presets::infos().len());
    let weather = plan::<ListEffects>(&doc, json!({ "kind": "weather" })).unwrap().output.unwrap();
    let names: Vec<&str> = weather["presets"].as_array().unwrap().iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["petals", "rain", "snow"]);
    assert_eq!(weather["presets"][0]["kind"], "weather");
}
