//! The fixture scene (every TOML example of design D2, D3, D5 and D6), its round trip, one test per D13 rule, and a
//! property test over generated scenes.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use omuzaki_char_api::CharId;
use omuzaki_render_api::Colour;
use omuzaki_stage_api::*;
use proptest::prelude::*;
use proptest::test_runner::RngSeed;
use serde_json::{Value, json};

fn toml_value(rel: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scene").join(rel);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn collection(names: &[&str], folder: &str) -> Value {
    Value::Object(names.iter().map(|n| ((*n).to_string(), toml_value(&format!("{folder}/{n}.toml")))).collect())
}

/// The scene node the store would load from the fixture files.
fn node() -> Value {
    json!({
        "scene": toml_value("scene.toml"),
        "lighting": toml_value("lighting.toml"),
        "cameras": collection(&["full", "face", "orbit"], "cameras"),
        "cast": collection(&["main", "kiri"], "cast"),
    })
}

fn fixture() -> SceneDef {
    SceneDef::from_node(&node()).unwrap()
}

#[test]
fn the_fixture_scene_parses_and_means_what_the_design_says() {
    let s = fixture();
    assert_eq!(s.scene.name, "Main");
    assert_eq!(s.scene.subject, Some(MemberId::new("main").unwrap()));
    assert_eq!(s.scene.camera, Some(CameraId::new("full").unwrap()));
    assert_eq!(s.lighting.lights.len(), 4);
    let key = &s.lighting.lights[&LightId::new("key").unwrap()];
    assert_eq!(key.shape, LightShape::Directional { azimuth: 33.69, elevation: 46.13 });
    assert!(key.shadow && key.anchor == Anchor::Subject);
    let candle = &s.lighting.lights[&LightId::new("candle").unwrap()];
    assert_eq!(candle.anchor, Anchor::Member(MemberId::new("kiri").unwrap()));
    assert_eq!(s.cameras[&CameraId::new("full").unwrap()].yaw, 30.0);
    assert_eq!(s.cast.len(), 2);
    assert_eq!(s.check().unwrap(), vec![], "the fixture is a valid scene with no findings");
}

#[test]
fn the_fixture_round_trips_through_the_node() {
    let s = fixture();
    let mut out = Value::Null;
    s.to_node(&mut out).unwrap();
    assert_eq!(SceneDef::from_node(&out).unwrap(), s);
    // What a person typed in the fixture is what is written back (no widened `f32` digits).
    assert_eq!(out["lighting"]["lights"]["key"]["azimuth"], json!(33.69));
    assert_eq!(out["cameras"]["face"]["shift"], json!([0.0, 0.05]));
    assert_eq!(out["scene"]["ground"], node()["scene"]["ground"]);
}

/// Edits the fixture node at `ptr` and reads it back.
fn edited(ptr: &str, value: Value) -> SceneDef {
    let mut n = node();
    *n.pointer_mut(ptr).unwrap_or_else(|| panic!("{ptr}")) = value;
    SceneDef::from_node(&n).unwrap()
}

fn refused(def: &SceneDef) -> SceneError {
    def.check().unwrap_err()
}

#[test]
fn numbers_that_are_not_finite_or_are_negative_are_refused_with_the_path() {
    let cases: [(&str, Value, &str); 9] = [
        ("/lighting/exposure", json!(-0.1), "lighting.exposure"),
        ("/lighting/outline_light", json!(-1.0), "lighting.outline_light"),
        ("/lighting/ambient/intensity", json!(-1.0), "lighting.ambient.intensity"),
        ("/lighting/lights/key/intensity", json!(-1.0), "lighting.lights.key.intensity"),
        ("/lighting/lights/candle/range", json!(0.0), "lighting.lights.candle.range"),
        ("/scene/ground/opacity", json!(-0.5), "scene.ground.opacity"),
        ("/scene/ground/opacity", json!(1.5), "scene.ground.opacity"),
        ("/scene/ground/radius", json!(-1.0), "scene.ground.radius"),
        ("/cameras/full/zoom", json!(0.0), "cameras.full.zoom"),
    ];
    for (ptr, value, path) in cases {
        assert_eq!(refused(&edited(ptr, value)).path, path);
    }
}

#[test]
fn non_finite_numbers_are_refused() {
    let mut s = fixture();
    s.lighting.exposure = f32::NAN;
    assert_eq!(refused(&s).path, "lighting.exposure");
    let mut s = fixture();
    s.cast.get_mut(&MemberId::new("kiri").unwrap()).unwrap().position = [f32::INFINITY, 0.0];
    assert_eq!(refused(&s).path, "cast.kiri.position");
    let mut s = fixture();
    s.lighting.lights.get_mut(&LightId::new("candle").unwrap()).unwrap().shape =
        LightShape::Point { position: [0.0, f32::NAN, 0.0], range: 1.0 };
    assert_eq!(refused(&s).path, "lighting.lights.candle.position");
}

#[test]
fn angles_outside_their_range_are_refused() {
    let cases: [(&str, f64, &str); 6] = [
        ("/lighting/lights/key/azimuth", 181.0, "lighting.lights.key.azimuth"),
        ("/lighting/lights/key/elevation", -91.0, "lighting.lights.key.elevation"),
        ("/cameras/full/yaw", -200.0, "cameras.full.yaw"),
        ("/cameras/full/pitch", 90.5, "cameras.full.pitch"),
        ("/cast/kiri/yaw", 270.0, "cast.kiri.yaw"),
        ("/cast/main/yaw", -181.0, "cast.main.yaw"),
    ];
    for (ptr, value, path) in cases {
        assert_eq!(refused(&edited(ptr, json!(value))).path, path);
    }
}

#[test]
fn fov_must_be_inside_zero_to_one_eighty() {
    for bad in [0.0, 180.0, -5.0, 200.0] {
        assert_eq!(refused(&edited("/cameras/full/fov", json!(bad))).path, "cameras.full.fov");
    }
    edited("/cameras/full/fov", json!(179.0)).check().unwrap();
}

#[test]
fn counts_over_the_limits_are_refused() {
    let mut s = fixture();
    // The ambient takes one of the renderer's eight light slots, so seven lights fit and the eighth does not.
    for i in 0..3 {
        s.lighting.lights.insert(
            LightId::new(format!("extra-{i}")).unwrap(),
            s.lighting.lights[&LightId::new("rim").unwrap()].clone(),
        );
    }
    assert_eq!(s.lighting.lights.len(), MAX_LIGHTS - 1);
    s.check().unwrap();
    s.lighting
        .lights
        .insert(LightId::new("one-too-many").unwrap(), s.lighting.lights[&LightId::new("rim").unwrap()].clone());
    assert_eq!(refused(&s).path, "lighting.lights");

    let mut s = fixture();
    for i in 0..MAX_CAMERAS {
        s.cameras.insert(CameraId::new(format!("c-{i}")).unwrap(), CameraDef::preset(Framing::Bust));
    }
    assert_eq!(refused(&s).path, "cameras");

    let mut s = fixture();
    for i in 0..MAX_CAST {
        s.cast.insert(MemberId::new(format!("m-{i}")).unwrap(), CastMember::new(CharId::new("aiko").unwrap()));
    }
    assert_eq!(refused(&s).path, "cast");

    let mut s = fixture();
    let cup = omuzaki_stage_api::PropDef::new(omuzaki_stage_api::PropKindId::new("cup").unwrap());
    s.props = (0..=MAX_PROPS).map(|i| (PropId::new(format!("p-{i}")).unwrap(), cup.clone())).collect();
    assert_eq!(refused(&s).path, "props");
    s.props.pop_last();
    s.check().unwrap();
}

#[test]
fn a_second_shadow_casting_directional_light_is_refused() {
    let mut s = fixture();
    s.lighting.lights.get_mut(&LightId::new("rim").unwrap()).unwrap().shadow = true;
    let err = refused(&s);
    assert_eq!(err.path, "lighting.lights.rim.shadow");
    assert!(err.rule.contains("`key` already"), "{err}");
    // A point light may carry the flag without counting: only directional lights cast today.
    let mut s = fixture();
    s.lighting.lights.get_mut(&LightId::new("candle").unwrap()).unwrap().shadow = true;
    s.check().unwrap();
}

#[test]
fn anchors_subjects_and_cameras_must_name_something_that_exists() {
    let cases: [(&str, Value, &str); 7] = [
        ("/lighting/lights/key/anchor", json!("member:ghost"), "lighting.lights.key.anchor"),
        ("/scene/subject", json!("ghost"), "scene.subject"),
        ("/scene/camera", json!("ghost"), "scene.camera"),
        ("/cameras/full/subject", json!("member:ghost"), "cameras.full.subject"),
        ("/cameras/full/subject", json!("stage"), "cameras.full.subject"),
        ("/cameras/orbit/anchor", json!("member:ghost"), "cameras.orbit.anchor"),
        ("/cameras/orbit/target", json!([0.0, null, 0.0]), ""),
    ];
    for (ptr, value, path) in cases {
        let mut n = node();
        *n.pointer_mut(ptr).unwrap() = value;
        match SceneDef::from_node(&n) {
            Ok(def) => assert_eq!(refused(&def).path, path, "{ptr}"),
            Err(_) => assert!(path.is_empty(), "{ptr} should read"),
        }
    }
}

#[test]
fn a_free_camera_needs_a_position_and_a_target() {
    let mut s = fixture();
    s.cameras.get_mut(&CameraId::new("orbit").unwrap()).unwrap().target = None;
    assert_eq!(refused(&s).path, "cameras.orbit.target");
}

#[test]
fn member_ids_stage_and_subject_are_refused() {
    for reserved in ["stage", "subject"] {
        let mut s = fixture();
        s.cast.insert(MemberId::new(reserved).unwrap(), CastMember::new(CharId::new("aiko").unwrap()));
        assert_eq!(refused(&s).path, format!("cast.{reserved}"));
    }
}

#[test]
fn a_member_whose_character_is_missing_is_a_finding_not_a_refusal() {
    let s = fixture();
    let findings = s.check_with(&|c| c.as_str() != "kiri").unwrap();
    assert_eq!(findings.len(), 1);
    assert_eq!((findings[0].kind, findings[0].path.as_str()), (FindingKind::MissingCharacter, "cast.kiri.character"));
}

#[test]
fn lighting_with_only_the_ambient_is_a_finding() {
    let mut s = fixture();
    s.lighting.lights.clear();
    let findings = s.check().unwrap();
    assert_eq!(findings.iter().map(|f| f.kind).collect::<Vec<_>>(), [FindingKind::NoLights]);
}

#[test]
fn unknown_keys_are_refused_at_every_level_with_the_nearest_name() {
    let mut n = node();
    n["scene"]["backdrop"]["colour "] = json!("#fff");
    assert!(SceneDef::from_node(&n).is_err());
    let mut n = node();
    n["lighting"]["lights"]["key"]["azimut"] = json!(1.0);
    let err = SceneDef::from_node(&n).unwrap_err().to_string();
    assert!(err.contains("azimut"), "{err}");
    let mut n = node();
    n["cast"] = json!({"main": {"character": "aiko", "possition": [0, 0]}});
    let err = SceneDef::from_node(&n).unwrap_err().to_string();
    assert!(err.contains("possition") && err.contains("position"), "{err}");
    let mut n = node();
    n["castt"] = json!({});
    assert!(SceneDef::from_node(&n).unwrap_err().to_string().contains("did you mean `cast`"));
}

#[test]
fn nested_unknown_fields_are_refused() {
    let cases = [
        ("/lighting/bloom/strenght", json!(1.0)),
        ("/scene/post/grade/saturaton", json!(1.0)),
        ("/scene/post/depth_of_field/rnge", json!(1.0)),
        ("/lighting/ambient/intensty", json!(1.0)),
        ("/scene/ground/opacty", json!(1.0)),
    ];
    for (ptr, value) in cases {
        let (parent, key) = ptr.rsplit_once('/').unwrap();
        let mut n = node();
        n.pointer_mut(parent).unwrap().as_object_mut().unwrap().insert(key.into(), value);
        assert!(SceneDef::from_node(&n).is_err(), "{ptr} must be refused");
    }
}

#[test]
fn schemas_exist_because_ops_take_these_types_as_arguments() {
    let schema = serde_json::to_value(schemars::schema_for!(SceneDef)).unwrap();
    assert!(schema["properties"]["lighting"].is_object());
    for s in [
        serde_json::to_value(schemars::schema_for!(LightDef)).unwrap(),
        serde_json::to_value(schemars::schema_for!(CameraDef)).unwrap(),
        serde_json::to_value(schemars::schema_for!(CastMember)).unwrap(),
        serde_json::to_value(schemars::schema_for!(SceneSettings)).unwrap(),
        serde_json::to_value(schemars::schema_for!(Anchor)).unwrap(),
    ] {
        assert!(s.is_object());
    }
}

// ---- property test -------------------------------------------------------------------------------------------

fn slug(prefix: &'static str) -> impl Strategy<Value = String> {
    (0u8..6).prop_map(move |i| format!("{prefix}-{i}"))
}

fn f(lo: f32, hi: f32) -> impl Strategy<Value = f32> {
    (lo..=hi).prop_map(|v| (v * 1000.0).round() / 1000.0)
}

fn colour() -> impl Strategy<Value = Colour> {
    (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(r, g, b)| Colour::rgb(r, g, b))
}

fn anchor() -> impl Strategy<Value = Anchor> {
    prop_oneof![
        Just(Anchor::Stage),
        Just(Anchor::Subject),
        slug("m").prop_map(|m| Anchor::Member(MemberId::new(m).unwrap()))
    ]
}

fn light() -> impl Strategy<Value = LightDef> {
    let shape = prop_oneof![
        (f(-180.0, 180.0), f(-90.0, 90.0))
            .prop_map(|(azimuth, elevation)| LightShape::Directional { azimuth, elevation }),
        ([f(-5.0, 5.0), f(-5.0, 5.0), f(-5.0, 5.0)], f(0.1, 20.0))
            .prop_map(|(position, range)| LightShape::Point { position, range }),
    ];
    (
        prop::option::of(prop_oneof![Just(LightRole::Key), Just(LightRole::Fill), Just(LightRole::Practical)]),
        colour(),
        f(0.0, 10.0),
        anchor(),
        shape,
    )
        .prop_map(|(role, colour, intensity, anchor, shape)| LightDef {
            role,
            colour,
            intensity,
            shadow: false,
            anchor,
            shape,
        })
}

fn camera() -> impl Strategy<Value = CameraDef> {
    (
        f(-180.0, 180.0),
        f(-90.0, 90.0),
        f(1.0, 170.0),
        f(0.1, 4.0),
        [f(-1.0, 1.0), f(-1.0, 1.0)],
        prop_oneof![Just(Framing::Face), Just(Framing::Bust), Just(Framing::Full), Just(Framing::Hands)],
    )
        .prop_map(|(yaw, pitch, fov, zoom, shift, framing)| CameraDef {
            yaw,
            pitch,
            fov,
            zoom,
            shift,
            ..CameraDef::preset(framing)
        })
}

fn member() -> impl Strategy<Value = CastMember> {
    (slug("c"), [f(-3.0, 3.0), f(-3.0, 3.0)], f(-180.0, 180.0), any::<bool>()).prop_map(
        |(c, position, yaw, visible)| CastMember { position, yaw, visible, ..CastMember::new(CharId::new(c).unwrap()) },
    )
}

fn scene() -> impl Strategy<Value = SceneDef> {
    let lights = prop::collection::btree_map(slug("l").prop_map(|s| LightId::new(s).unwrap()), light(), 0..4);
    let cameras = prop::collection::btree_map(slug("cam").prop_map(|s| CameraId::new(s).unwrap()), camera(), 0..4);
    let cast = prop::collection::btree_map(slug("m").prop_map(|s| MemberId::new(s).unwrap()), member(), 0..4);
    (lights, cameras, cast, f(0.0, 3.0), f(0.0, 1.0), prop::option::of(f(0.0, 1.0)), any::<bool>(), colour()).prop_map(
        |(lights, cameras, cast, exposure, opacity, skin_floor, bloom, bg)| {
            let mut s = SceneDef::new(SceneSettings::named("Generated"));
            s.scene.ground.opacity = opacity;
            s.scene.post.bloom = bloom;
            s.scene.backdrop = Backdrop::Colour { colour: bg };
            s.lighting.exposure = exposure;
            s.lighting.skin_floor = skin_floor;
            s.lighting.bloom = bloom.then_some(BloomDef { strength: 0.3, radius: 0.5, threshold: 0.9 });
            s.lighting.lights = lights;
            s.cameras = cameras;
            s.cast = cast;
            s
        },
    )
}

fn config() -> ProptestConfig {
    // A fixed seed unless PROPTEST_RNG_SEED asks otherwise, so a failure reproduces (as omuzaki-doc's properties do).
    let mut config = ProptestConfig { failure_persistence: None, ..ProptestConfig::default() };
    if matches!(config.rng_seed, RngSeed::Random) {
        config.rng_seed = RngSeed::Fixed(0x5EED);
    }
    config
}

proptest! {
    #![proptest_config(config())]
    #[test]
    fn generated_scenes_round_trip_through_the_node_and_json(s in scene()) {
        let mut node = Value::Null;
        s.to_node(&mut node).unwrap();
        prop_assert_eq!(SceneDef::from_node(&node).unwrap(), s.clone());
        prop_assert_eq!(serde_json::from_str::<SceneDef>(&serde_json::to_string(&s).unwrap()).unwrap(), s.clone());
        let mut again = node.clone();
        s.to_node(&mut again).unwrap();
        prop_assert_eq!(again, node, "writing is idempotent");
    }
}
