//! Phase 2 of the scene (#931, design D3, D4, D14): spot lights, fog and the environment are additive, so a phase 1
//! scene reads and writes back unchanged; their files read flat like the lights' and name a wrong field; the check
//! refuses impossible numbers by path; and a spot resolves in subject units, so the light reaching a face is the same at
//! two statures.

#![allow(clippy::unwrap_used)]

use std::path::Path;

use omuzaki_core::glam::{Affine3A, Vec3};
use omuzaki_render_api::{Colour, Light};
use omuzaki_stage_api::resolve::{AnchorFrame, LightBuf};
use omuzaki_stage_api::*;
use omuzaki_store_api::ContentHash;
use serde_json::{Value, json};

fn toml_value(rel: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/scene").join(rel);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    toml::from_str(&text).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn collection(names: &[&str], folder: &str) -> Value {
    Value::Object(names.iter().map(|n| ((*n).to_string(), toml_value(&format!("{folder}/{n}.toml")))).collect())
}

/// The phase 1 fixture scene node (every TOML example of design D2, D3, D5 and D6).
fn phase_one() -> Value {
    json!({
        "scene": toml_value("scene.toml"),
        "lighting": toml_value("lighting.toml"),
        "cameras": collection(&["full", "face", "orbit"], "cameras"),
        "cast": collection(&["main", "kiri"], "cast"),
    })
}

/// The design's phase 2 example (D3): a spot on the subject, fog and a sky, added to the phase 1 lighting.
const PHASE_TWO: &str = r##"
[fog]
colour = "#2a2238"
near = 2.0
far = 9.0

[environment]
kind = "sky"
zenith = "#6f86b8"
horizon = "#e8c9a0"
ground = "#3b3026"
intensity = 0.6
rotation = 0.0
toon_ramp = true

[lights.stage]
kind = "spot"
role = "accent"
colour = "#ffe0c0"
intensity = 6.0
position = [0.0, 2.6, 1.2]
target = [0.0, 1.0, 0.0]
cone = 25.0
penumbra = 0.3
range = 6.0
shadow = false
anchor = "subject"
"##;

fn phase_two() -> Value {
    let mut node = phase_one();
    let extra: Value = toml::from_str(PHASE_TWO).unwrap();
    let lighting = node["lighting"].as_object_mut().unwrap();
    for (key, value) in extra.as_object().unwrap() {
        if key == "lights" {
            let lights = lighting["lights"].as_object_mut().unwrap();
            lights.extend(value.as_object().unwrap().clone());
        } else {
            lighting.insert(key.clone(), value.clone());
        }
    }
    node["scene"]["backdrop"] = json!({"kind": "environment"});
    node
}

/// `node` as the writer puts it: every value as given, plus a light's `shadow = false`, which phase 1 already wrote out.
fn as_written(mut node: Value) -> Value {
    for light in node["lighting"]["lights"].as_object_mut().unwrap().values_mut() {
        light.as_object_mut().unwrap().entry("shadow").or_insert(json!(false));
    }
    node
}

#[test]
fn a_phase_one_scene_reads_and_writes_back_unchanged() {
    let def = SceneDef::from_node(&phase_one()).unwrap();
    assert_eq!((def.lighting.fog, def.lighting.environment), (None, None), "phase 2 fields default to absent");
    let mut out = Value::Null;
    def.to_node(&mut out).unwrap();
    for section in ["scene", "lighting"] {
        assert_eq!(out[section], as_written(phase_one())[section], "{section} writes back as the file has it");
    }
    assert_eq!(out["lighting"].get("fog"), None);
    assert_eq!(out["lighting"].get("environment"), None);
}

#[test]
fn the_design_example_of_phase_two_reads_and_round_trips() {
    let def = SceneDef::from_node(&phase_two()).unwrap();
    let l = &def.lighting;
    assert_eq!(l.fog, Some(FogDef { colour: Colour::hex(0x002a_2238), near: 2.0, far: 9.0 }));
    let env = l.environment.unwrap();
    assert_eq!(
        env.source,
        EnvironmentSource::Sky {
            zenith: Colour::hex(0x006f_86b8),
            horizon: Colour::hex(0x00e8_c9a0),
            ground: Colour::hex(0x003b_3026)
        }
    );
    assert_eq!((env.intensity, env.rotation, env.toon_ramp), (0.6, 0.0, true));
    assert_eq!(
        l.lights[&LightId::new("stage").unwrap()].shape,
        LightShape::Spot { position: [0.0, 2.6, 1.2], target: [0.0, 1.0, 0.0], cone: 25.0, penumbra: 0.3, range: 6.0 }
    );
    assert_eq!(def.scene.backdrop, Backdrop::Environment {});
    assert_eq!(def.check().unwrap(), vec![], "a valid scene with no findings");
    let mut out = Value::Null;
    def.to_node(&mut out).unwrap();
    assert_eq!(SceneDef::from_node(&out).unwrap(), def);
    assert_eq!(out["lighting"], as_written(phase_two())["lighting"], "written back as the file has it");
}

#[test]
fn an_environment_reads_with_defaults_and_an_hdri_names_its_asset() {
    let bare: EnvironmentDef = serde_json::from_value(json!({"kind": "sky"})).unwrap();
    assert_eq!(bare, EnvironmentDef::default());
    let hash = ContentHash::of(b"an hdri").to_string();
    let hdri: EnvironmentDef =
        serde_json::from_value(json!({"kind": "hdri", "asset": hash, "rotation": 90.0, "toon_ramp": false})).unwrap();
    assert_eq!(hdri.source, EnvironmentSource::Hdri { asset: hash.parse().unwrap() });
    assert_eq!((hdri.intensity, hdri.rotation, hdri.toon_ramp), (1.0, 90.0, false));
    assert_eq!(
        serde_json::to_value(hdri).unwrap(),
        json!({"kind": "hdri", "asset": hash, "intensity": 1.0, "rotation": 90.0, "toon_ramp": false})
    );
}

#[test]
fn a_wrong_field_is_refused_by_name() {
    let hash = ContentHash::of(b"x").to_string();
    let lights = [
        (
            json!({"kind": "spot", "position": [0, 2, 1], "target": [0, 1, 0], "cone": 20, "range": 5, "azimuth": 1}),
            "no `azimuth`",
        ),
        (json!({"kind": "spot", "position": [0, 2, 1], "cone": 20, "range": 5}), "needs `target`"),
        (json!({"kind": "spot", "position": [0, 2, 1], "target": [0, 1, 0], "range": 5}), "needs `cone`"),
        (json!({"kind": "point", "position": [0, 2, 1], "range": 5, "cone": 20}), "no `cone`"),
        (json!({"kind": "directional", "azimuth": 1, "elevation": 2, "target": [0, 0, 0]}), "no `target`"),
    ];
    for (v, expect) in lights {
        let err = serde_json::from_value::<LightDef>(v).unwrap_err().to_string();
        assert!(err.contains(expect), "{err}");
    }
    let spot: LightDef = serde_json::from_value(
        json!({"kind": "spot", "position": [0, 2, 1], "target": [0, 1, 0], "cone": 20, "range": 5}),
    )
    .unwrap();
    assert!(matches!(spot.shape, LightShape::Spot { penumbra, .. } if penumbra == 0.0), "a hard edge by default");
    let environments = [
        (json!({"kind": "sky", "asset": hash}), "no `asset`"),
        (json!({"kind": "hdri", "asset": hash, "zenith": "#ffffff"}), "no `zenith`"),
        (json!({"kind": "hdri"}), "needs `asset`"),
        (json!({"kind": "sky", "intensty": 1.0}), "intensty"),
    ];
    for (v, expect) in environments {
        let err = serde_json::from_value::<EnvironmentDef>(v).unwrap_err().to_string();
        assert!(err.contains(expect), "{err}");
    }
    assert!(serde_json::from_value::<FogDef>(json!({"colour": "#000000", "near": 1, "far": 2, "density": 1})).is_err());
}

#[test]
fn impossible_phase_two_numbers_are_refused_with_the_path() {
    let edits: [(&str, Value, &str); 9] = [
        ("/lighting/lights/stage/cone", json!(0.0), "lighting.lights.stage.cone"),
        ("/lighting/lights/stage/cone", json!(95.0), "lighting.lights.stage.cone"),
        ("/lighting/lights/stage/penumbra", json!(1.5), "lighting.lights.stage.penumbra"),
        ("/lighting/lights/stage/range", json!(0.0), "lighting.lights.stage.range"),
        ("/lighting/lights/stage/target", json!([0.0, 2.6, 1.2]), "lighting.lights.stage.target"),
        ("/lighting/fog/near", json!(-1.0), "lighting.fog.near"),
        ("/lighting/fog/far", json!(2.0), "lighting.fog.far"),
        ("/lighting/environment/intensity", json!(-0.5), "lighting.environment.intensity"),
        ("/lighting/lights/stage/intensity", json!(-1.0), "lighting.lights.stage.intensity"),
    ];
    for (ptr, value, path) in edits {
        let mut node = phase_two();
        *node.pointer_mut(ptr).unwrap_or_else(|| panic!("{ptr}")) = value;
        let def = SceneDef::from_node(&node).unwrap();
        assert_eq!(def.check().unwrap_err().path, path, "{ptr}");
    }
}

#[test]
fn an_environment_backdrop_without_an_environment_is_a_finding() {
    let mut node = phase_two();
    node["lighting"].as_object_mut().unwrap().remove("environment");
    let findings = SceneDef::from_node(&node).unwrap().check().unwrap();
    assert_eq!(
        findings.iter().map(|f| (f.kind, f.path.as_str())).collect::<Vec<_>>(),
        [(FindingKind::NoEnvironment, "scene.backdrop")]
    );
}

/// The three.js spot (render-api D10): the point falloff times `smoothstep(cos outer, cos inner, cos angle)`.
fn spot_reaching(light: &Light, p: Vec3) -> f32 {
    let Light::Spot { position, direction, intensity, range, cone_outer_deg, penumbra, .. } = *light else {
        panic!("not a spot")
    };
    let to = p - position;
    let d = to.length();
    let window = (1.0 - (d / range).powi(4)).clamp(0.0, 1.0);
    let (outer, inner) = (cone_outer_deg.to_radians().cos(), (cone_outer_deg * (1.0 - penumbra)).to_radians().cos());
    let cos = to.normalize().dot(direction.normalize());
    let t = ((cos - outer) / (inner - outer)).clamp(0.0, 1.0);
    intensity * window * window / d.powi(2).max(0.01) * t * t * (3.0 - 2.0 * t)
}

#[test]
fn a_spot_resolves_in_subject_units_and_lights_a_face_alike_at_two_statures() {
    let def = SceneDef::from_node(&phase_two()).unwrap();
    let stage = LightId::new("stage").unwrap();
    let index = 1 + def.lighting.lights.keys().position(|id| *id == stage).unwrap();
    // A soft-sd at the reference height facing +Z at the origin, and a tall adult two metres to the right turned 90°.
    let figures = [
        (REFERENCE_HEIGHT_M, Affine3A::IDENTITY),
        (1.714, Affine3A::from_translation(Vec3::new(2.0, 0.0, 0.0)) * Affine3A::from_rotation_y(90_f32.to_radians())),
    ];
    let mut reaching = Vec::new();
    for (height_m, root) in figures {
        let frames =
            move |anchor: &Anchor| (!matches!(anchor, Anchor::Stage)).then_some(AnchorFrame { root, height_m });
        let mut out = LightBuf::new();
        def.lighting.resolve_into(&frames, &mut out).unwrap();
        let light = out.as_slice()[index];
        let Light::Spot { position, direction, range, intensity, cone_outer_deg, penumbra, .. } = light else {
            panic!("the stage light is not a spot")
        };
        let place = |p: [f32; 3]| root.transform_point3(Vec3::from_array(p) * height_m);
        assert!(position.abs_diff_eq(place([0.0, 2.6, 1.2]), 1e-5), "{height_m} m: the lamp at {position}");
        let aim = place([0.0, 1.0, 0.0]) - position;
        assert!(direction.normalize().abs_diff_eq(aim.normalize(), 1e-5), "{height_m} m: pointing {direction}");
        assert!((range - 6.0 * height_m).abs() < 1e-4 && (intensity - 6.0 * height_m * height_m).abs() < 1e-3);
        assert_eq!((cone_outer_deg, penumbra), (25.0, 0.3), "angles don't scale");
        // The face: 0.9 of the height up, a little forward, and a point near the cone's edge.
        let face = [[0.0, 0.9, 0.05], [0.12, 0.95, 0.05]].map(|p| spot_reaching(&light, place(p)));
        reaching.push(face);
    }
    for (a, b) in reaching[0].iter().zip(&reaching[1]) {
        assert!(*a > 0.0 && (a - b).abs() <= 1e-3 * a, "the same light reaches the face at both statures: {a} vs {b}");
    }
}
