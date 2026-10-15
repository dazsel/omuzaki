//! Scene recipes against the golden manifest and the light-rig library.
//!
//! The library tests always run. The manifest tests read a local goldens folder (`goldens/js/manifest.json`, not in git) and
//! skip cleanly when it is absent. They check every one of the manifest's cases: the recipe's lights, camera, post,
//! background and pose equal the manifest's, so the library's transcription is proven against the source.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::str::FromStr;

use omuzaki_anim_api::skeleton::HumanoidBone;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_render_api::{Background, Bloom, Light, Post, Shading, Tonemap};
use omuzaki_stage::recipe::lights::Shadow;
use omuzaki_stage::recipe::{GroundRecipe, LightRig, MSAA_SAMPLES, RecipeError, SceneRecipe};
use serde::Deserialize;
use serde_json::{Value, json};

/// The manifest's place under the assets folder (`$OMUZAKI_ASSETS`, default `assets-local`; never in git).
const MANIFEST: &str = "goldens/js/manifest.json";
/// Light, post and pose values must match the manifest within this.
const TOL: f32 = 1e-6;
/// Camera matrices must match within this.
const MATRIX_TOL: f32 = 1e-5;
/// The hips' rest translation in the manifest's normalised rig: every case's hips sit here (pose `t` 0, no `par`).
const REST_HIPS: Vec3 = Vec3::new(0.0, 0.37, 0.0);
/// The seven rigs the library ships, in file order.
const RIGS: [&str; 7] =
    ["viewer-studio", "viewer-candle", "viewer-moonlit", "dev-studio", "dev-night", "dev-warm", "perform-facecam"];

/// The manifest's cases, or `None` when the goldens are absent. The test then skips, saying why on stderr.
///
/// Read once per test process: the tests run in parallel, and several large reads at once over the Windows-drive
/// mount (9p) have failed with "Cannot allocate memory" in CI.
#[allow(clippy::disallowed_methods)] // test fixture: reads the repo's own asset manifest
fn manifest() -> Option<&'static Value> {
    static MANIFEST_JSON: std::sync::OnceLock<Option<Value>> = std::sync::OnceLock::new();
    MANIFEST_JSON
        .get_or_init(|| {
            let path = omuzaki_core::assets::assets_root().join(MANIFEST);
            omuzaki_core::assets::require(&path)?;
            let text = std::fs::read_to_string(&path).expect("the manifest is readable");
            Some(serde_json::from_str(&text).expect("the manifest is JSON"))
        })
        .as_ref()
}

fn cases(manifest: &Value) -> &Vec<Value> {
    manifest["cases"].as_array().expect("the manifest has a cases array")
}

fn vec3(v: &Value) -> Vec3 {
    Vec3::from(<[f32; 3]>::deserialize(v).expect("a 3-vector"))
}

fn number(v: &Value) -> f32 {
    f32::deserialize(v).expect("a number")
}

fn colour(v: &Value) -> Colour {
    Colour::from_str(v.as_str().expect("a hex colour")).expect("a valid hex colour")
}

/// The lights the manifest's `spec.rig` asks for, built straight from the JSON: hemisphere, key, rim, fill, point.
fn expected_lights(rig: &Value) -> Vec<Light> {
    let hemi = &rig["hemi"];
    let mut lights = vec![Light::Hemisphere {
        sky: colour(&hemi["sky"]),
        ground: colour(&hemi["ground"]),
        intensity: number(&hemi["intensity"]),
    }];
    for key in ["key", "rim", "fill"] {
        let light = &rig["directional"][key];
        if light.is_null() {
            continue;
        }
        // A directional light shines from `position` toward the origin; the contract takes that direction.
        lights.push(Light::Directional {
            to_light: vec3(&light["position"]).normalize(),
            colour: colour(&light["color"]),
            intensity: number(&light["intensity"]),
            casts_shadow: !light["shadow"].is_null(),
        });
    }
    let point = &rig["point"];
    if !point.is_null() {
        assert_eq!(number(&point["decay"]), 2.0, "every rig's point light uses three.js's decay 2");
        lights.push(Light::Point {
            position: vec3(&point["position"]),
            colour: colour(&point["color"]),
            intensity: number(&point["intensity"]),
            range: number(&point["distance"]),
        });
    }
    lights
}

/// The largest difference between a recipe light and the manifest's, or infinity when the kinds or colours differ.
fn light_error(got: &Light, want: &Light) -> f32 {
    match (*got, *want) {
        (
            Light::Hemisphere { sky: got_sky, ground: got_ground, intensity: got_intensity },
            Light::Hemisphere { sky: want_sky, ground: want_ground, intensity: want_intensity },
        ) if got_sky == want_sky && got_ground == want_ground => (got_intensity - want_intensity).abs(),
        (
            Light::Directional {
                to_light: got_dir,
                colour: got_colour,
                intensity: got_intensity,
                casts_shadow: got_shadow,
            },
            Light::Directional {
                to_light: want_dir,
                colour: want_colour,
                intensity: want_intensity,
                casts_shadow: want_shadow,
            },
        ) if got_colour == want_colour && got_shadow == want_shadow => {
            (got_dir - want_dir).abs().max_element().max((got_intensity - want_intensity).abs())
        }
        (
            Light::Point { position: got_pos, colour: got_colour, intensity: got_intensity, range: got_range },
            Light::Point { position: want_pos, colour: want_colour, intensity: want_intensity, range: want_range },
        ) if got_colour == want_colour => (got_pos - want_pos)
            .abs()
            .max_element()
            .max((got_intensity - want_intensity).abs())
            .max((got_range - want_range).abs()),
        _ => f32::INFINITY,
    }
}

/// The largest difference between two posts (exposure and bloom), or infinity when the tone mapping or the presence of
/// bloom differs.
fn post_error(got: &Post, want: &Post) -> f32 {
    if got.tonemap != want.tonemap || got.tonemap != Tonemap::Aces {
        return f32::INFINITY;
    }
    let exposure = (got.exposure - want.exposure).abs();
    match (got.bloom, want.bloom) {
        (None, None) => exposure,
        (Some(a), Some(b)) => exposure
            .max((a.strength - b.strength).abs())
            .max((a.radius - b.radius).abs())
            .max((a.threshold - b.threshold).abs()),
        _ => f32::INFINITY,
    }
}

/// The post the manifest's rig and render settings describe.
fn expected_post(case: &Value) -> Post {
    let rig = &case["spec"]["rig"];
    let bloom = &rig["bloom"];
    let enabled = case["spec"]["render"]["bloom"]["enabled"].as_bool().expect("bloom enabled is a bool");
    Post {
        exposure: number(&rig["exposure"]),
        tonemap: Tonemap::Aces,
        bloom: enabled.then(|| Bloom {
            strength: number(&bloom["strength"]),
            radius: number(&bloom["radius"]),
            threshold: number(&bloom["threshold"]),
        }),
        ..Post::default()
    }
}

/// The manifest's projection (three.js, depth -1..1) remapped to the contract's depth 0..1: the z row becomes
/// `(z + w) / 2`.
fn to_unit_depth(gl: Mat4) -> Mat4 {
    let mut m = gl.to_cols_array();
    for col in 0..4 {
        m[col * 4 + 2] = f32::midpoint(m[col * 4 + 2], m[col * 4 + 3]);
    }
    Mat4::from_cols_array(&m)
}

fn max_abs_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0, f32::max)
}

#[derive(Deserialize)]
struct ResolvedCamera {
    #[serde(rename = "matrixWorld")]
    world: [f32; 16],
    #[serde(rename = "matrixWorldInverse")]
    inverse: [f32; 16],
    projection: [f32; 16],
}

#[derive(Deserialize)]
struct ResolvedBone {
    quaternion: [f32; 4],
}

#[test]
fn the_library_holds_the_seven_rigs_in_file_order() {
    assert_eq!(LightRig::names().collect::<Vec<_>>(), RIGS);
}

#[test]
fn every_library_rig_loads_validates_and_round_trips() {
    for name in RIGS {
        let rig = LightRig::load(name).expect("the library rig loads and validates");
        assert_eq!(rig.name, name);
        let text = toml::to_string(&rig).expect("the rig serialises");
        let again = LightRig::from_toml(&text).expect("the serialised rig parses and validates");
        assert_eq!(again, rig, "{name} round-trips");
    }
}

#[test]
fn an_unknown_rig_and_an_out_of_range_intensity_are_errors() {
    assert_eq!(LightRig::load("no-such-rig"), Err(RecipeError::UnknownRig("no-such-rig".into())));
    let mut rig = LightRig::load("viewer-studio").expect("loads");
    rig.key.intensity = -1.0;
    assert!(matches!(rig.validate(), Err(RecipeError::Invalid { .. })));
}

#[test]
fn a_directional_light_with_no_direction_is_refused_by_validate_and_to_lights_alike() {
    // 1e-30 is not zero, but its squared length underflows in f32: validate and to_lights must both refuse it.
    let mut rig = LightRig::load("viewer-studio").expect("loads");
    rig.key.position = [1e-30, 0.0, 0.0];
    let invalid = rig.validate().expect_err("validate refuses a near-zero direction");
    assert!(invalid.to_string().contains("key.position"), "{invalid}");
    let refused = rig.to_lights().expect_err("to_lights refuses it too");
    assert!(refused.to_string().contains("key.position"), "{refused}");
}

#[test]
fn the_ground_is_an_upward_disc_of_48_segments_with_the_shadow_catcher() {
    let ground = GroundRecipe::STAGE;
    let mesh = ground.mesh().expect("the stage ground is a valid disc");
    mesh.validate().expect("the disc is a valid mesh");
    assert_eq!(mesh.triangle_count(), 48);
    for tri in mesh.indices.chunks(3) {
        let [a, b, c] = [tri[0], tri[1], tri[2]].map(|i| mesh.positions[i as usize]);
        assert!((b - a).cross(c - a).y > 0.0, "triangle {tri:?} faces up");
    }
    let radius = GroundRecipe::STAGE.radius;
    assert!(mesh.positions.iter().all(|p| p.y == 0.0 && (p.length() < 1e-6 || (p.length() - radius).abs() < 1e-6)));
    let Shading::Program { name, params } = ground.shading().expect("the shadow catcher") else {
        panic!("the ground is a named shading program");
    };
    assert_eq!(name.as_str(), "shadow-catcher");
    assert_eq!(params.len(), 1);
    assert_eq!(params.values().next(), Some(&omuzaki_render_api::ParamValue::Float(0.25)));
}

#[test]
fn every_manifest_case_matches_the_manifest_lights_post_background_and_sim() {
    let Some(manifest) = manifest() else { return };
    let cases = cases(manifest);
    assert!(!cases.is_empty(), "the manifest has cases to check");
    let mut worst = 0.0_f32;
    for case in cases {
        let id = case["id"].as_str().expect("an id");
        let recipe = SceneRecipe::from_manifest_case(case).unwrap_or_else(|e| panic!("{id}: {e}"));
        let rig = &case["spec"]["rig"];
        let want = expected_lights(rig);
        assert_eq!(recipe.lights.len(), want.len(), "{id}: light count");
        for (got, want) in recipe.lights.iter().zip(&want) {
            worst = worst.max(light_error(got, want));
        }
        worst = worst.max(post_error(&recipe.post, &expected_post(case)));
        let background = Background::Solid { colour: colour(&rig["background"]) };
        assert_eq!(recipe.background, background, "{id}: background");
        assert_eq!(recipe.rig, case["rig"].as_str().expect("a rig name"));
        worst = worst.max((recipe.dt - number(&case["spec"]["sim"]["dt"])).abs());
        assert_eq!(recipe.settle_steps, u32::deserialize(&case["spec"]["sim"]["settleSteps"]).expect("steps"));
        assert_eq!(recipe.samples, MSAA_SAMPLES);
        assert_eq!(recipe.ground, GroundRecipe::STAGE);
    }
    assert!(worst <= TOL, "largest light/post/sim error {worst:e} is above {TOL:e}");
    eprintln!("matched {} manifest cases; max light/post/sim error {worst:e}", cases.len());
}

#[test]
fn every_manifest_camera_matches_resolved_matrices() {
    let Some(manifest) = manifest() else { return };
    let mut worst = 0.0_f32;
    for case in cases(manifest) {
        let id = case["id"].as_str().expect("an id");
        let recipe = SceneRecipe::from_manifest_case(case).expect("the case builds");
        let resolved = ResolvedCamera::deserialize(&case["resolved"]["camera"]).expect("resolved camera");
        let camera = recipe.camera;
        worst = worst.max(max_abs_diff(&camera.transform.to_cols_array(), &resolved.world));
        worst = worst.max(max_abs_diff(&camera.view().to_cols_array(), &resolved.inverse));
        // Every golden is square, so the aspect is 1: projection[0] is 1 / tan(fov / 2).
        let fov = number(&case["spec"]["camera"]["fov"]);
        let focal = 1.0 / (fov.to_radians() * 0.5).tan();
        assert!((resolved.projection[0] - focal).abs() < MATRIX_TOL, "{id}: square aspect");
        let unit_depth = to_unit_depth(Mat4::from_cols_array(&resolved.projection));
        worst = worst.max(max_abs_diff(&camera.projection_matrix(1.0).to_cols_array(), &unit_depth.to_cols_array()));
    }
    assert!(worst <= MATRIX_TOL, "largest camera matrix error {worst:e} is above {MATRIX_TOL:e}");
    eprintln!("camera matrices match the manifest; max error {worst:e}");
}

#[test]
fn every_manifest_pose_has_no_hips_offset_at_the_rest_hips_and_its_rotations() {
    let Some(manifest) = manifest() else { return };
    let mut worst = 0.0_f32;
    for case in cases(manifest) {
        let id = case["id"].as_str().expect("an id");
        let recipe = SceneRecipe::from_manifest_case(case).expect("the case builds");
        let pose = recipe.pose_for(REST_HIPS);
        assert!(pose.hips_offset.length() <= TOL, "{id}: hips offset {:?}", pose.hips_offset);
        let bones = case["resolved"]["bones"].as_object().expect("bones");
        for (name, bone) in bones {
            let bone = ResolvedBone::deserialize(bone).expect("a bone");
            let want = glam_quat(bone.quaternion);
            let got = pose.get(HumanoidBone::from_name(name).expect("a humanoid bone"));
            worst = worst.max(max_abs_diff(&got.to_array(), &want.to_array()));
        }
        // Bones the manifest does not name stay at the rest pose.
        let named: Vec<HumanoidBone> = bones.keys().filter_map(|n| HumanoidBone::from_name(n)).collect();
        for bone in HumanoidBone::ALL.into_iter().filter(|b| !named.contains(b)) {
            assert_eq!(pose.get(bone), Quat::IDENTITY, "{id}: {bone} is not in the manifest");
        }
    }
    assert!(worst <= TOL, "largest rotation error {worst:e} is above {TOL:e}");
    eprintln!("poses: zero hips offset in all cases; max rotation error {worst:e}");
}

fn glam_quat(q: [f32; 4]) -> Quat {
    Quat::from_xyzw(q[0], q[1], q[2], q[3])
}

#[test]
fn the_manifest_stage_block_matches_the_recipe_constants() {
    let Some(manifest) = manifest() else { return };
    let stage = &manifest["stage"];
    assert_eq!(number(&stage["msaa"]), f32::from(MSAA_SAMPLES));
    let ground = &stage["ground"];
    assert_eq!(number(&ground["radius"]), GroundRecipe::STAGE.radius);
    assert_eq!(u16::deserialize(&ground["segments"]).expect("segments"), GroundRecipe::STAGE.segments);
    assert_eq!(number(&ground["shadowOpacity"]), GroundRecipe::STAGE.shadow_opacity);
}

/// A minimal golden case: the given rig, a front camera and a two-bone pose. It needs no goldens, so the recipe's
/// own paths run on every machine.
fn inline_case(rig: &str, bloom: bool) -> Value {
    json!({
        "id": "inline/minimal",
        "rig": rig,
        "spec": {
            "camera": { "fov": 30.0, "near": 0.05, "far": 80.0, "position": [0.0, 0.75, 1.1], "target": [0.0, 0.75, 0.0] },
            "render": { "bloom": { "enabled": bloom } },
            "sim": { "dt": 0.0125, "settleSteps": 480 }
        },
        "resolved": { "bones": {
            "hips": { "position": [0.0, 0.37, 0.0], "quaternion": [0.0, 0.0, 0.0, 1.0] },
            "head": { "position": [0.0, 0.04, 0.0], "quaternion": [0.0, 0.0, 0.0, 1.0] }
        } }
    })
}

#[test]
fn an_inline_case_builds_a_recipe_without_the_manifest() {
    let recipe = SceneRecipe::from_manifest_case(&inline_case("viewer-studio", true)).expect("the case builds");
    assert_eq!(recipe.rig, "viewer-studio");
    assert_eq!(recipe.camera.position(), Vec3::new(0.0, 0.75, 1.1));
    assert_eq!(recipe.lights.len(), 4, "hemisphere, key, rim and fill");
    assert!((recipe.post.exposure - 0.9).abs() < TOL, "the studio rig's exposure");
    assert!(recipe.post.bloom.is_some(), "bloom is on for this case");
    assert_eq!((recipe.dt, recipe.settle_steps, recipe.samples), (0.0125, 480, MSAA_SAMPLES));
    assert_eq!(recipe.ground, GroundRecipe::STAGE);
    let no_bloom = SceneRecipe::from_manifest_case(&inline_case("viewer-studio", false)).expect("builds");
    assert_eq!(no_bloom.post.bloom, None, "bloom is off for this case");
}

#[test]
fn pose_for_moves_the_hips_by_the_offset_from_the_rest_hips() {
    let recipe = SceneRecipe::from_manifest_case(&inline_case("viewer-studio", false)).expect("builds");
    assert_eq!(recipe.pose_for(Vec3::new(0.0, 0.37, 0.0)).hips_offset, Vec3::ZERO);
    let offset = recipe.pose_for(Vec3::new(0.0, 0.24, 0.0)).hips_offset;
    assert!((offset - Vec3::new(0.0, 0.13, 0.0)).length() < TOL, "{offset:?}");
    let pose = recipe.pose_for(Vec3::ZERO);
    assert_eq!(pose.get(HumanoidBone::Hips), Quat::IDENTITY, "the hips are a translation, not a rotation");
    assert_eq!(pose.get(HumanoidBone::Chest), Quat::IDENTITY, "a bone the case does not name stays at rest");
}

#[test]
fn a_case_the_recipe_cannot_read_is_refused_and_the_field_is_named() {
    let refused = |case: Value| SceneRecipe::from_manifest_case(&case).expect_err("the case is refused");
    assert!(matches!(refused(json!({})), RecipeError::Parse { .. }), "an empty case is not a case");
    assert_eq!(refused(inline_case("no-such-rig", true)), RecipeError::UnknownRig("no-such-rig".into()));

    let mut no_hips = inline_case("viewer-studio", true);
    no_hips["resolved"]["bones"].as_object_mut().expect("bones").remove("hips");
    assert!(refused(no_hips).to_string().contains("hips"));

    let mut not_humanoid = inline_case("viewer-studio", true);
    not_humanoid["resolved"]["bones"]["tail"] =
        json!({ "position": [0.0, 0.0, 0.0], "quaternion": [0.0, 0.0, 0.0, 1.0] });
    assert!(refused(not_humanoid).to_string().contains("'tail'"));

    for quaternion in [[0.0, 0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 2.0]] {
        let mut bad = inline_case("viewer-studio", true);
        bad["resolved"]["bones"]["head"]["quaternion"] = json!(quaternion);
        let message = refused(bad).to_string();
        assert!(message.contains("resolved.bones.head.quaternion"), "{quaternion:?}: {message}");
    }

    let mut no_fov = inline_case("viewer-studio", true);
    no_fov["spec"]["camera"]["fov"] = json!(0.0);
    assert!(matches!(refused(no_fov), RecipeError::Invalid { .. }), "a field of view of 0 is not a lens");
    let mut near_past_far = inline_case("viewer-studio", true);
    near_past_far["spec"]["camera"]["near"] = json!(90.0);
    assert!(matches!(refused(near_past_far), RecipeError::Invalid { .. }), "near past far is not a lens");
}

#[test]
fn a_ground_with_bad_values_is_refused() {
    let radius = GroundRecipe { radius: 0.0, ..GroundRecipe::STAGE };
    assert!(matches!(radius.mesh(), Err(RecipeError::Invalid { .. })));
    let segments = GroundRecipe { segments: 2, ..GroundRecipe::STAGE };
    assert!(matches!(segments.mesh(), Err(RecipeError::Invalid { .. })));
    let opacity = GroundRecipe { shadow_opacity: 1.5, ..GroundRecipe::STAGE };
    assert!(matches!(opacity.shading(), Err(RecipeError::Invalid { .. })));
}

/// The largest difference between a library shadow block and the manifest's, after checking both have one or neither.
fn shadow_error(got: Option<&Shadow>, want: &Value, id: &str, label: &str) -> f32 {
    match (got, want.is_null()) {
        (None, true) => 0.0,
        (Some(s), false) => {
            assert_eq!(s.map_size, u32::deserialize(&want["mapSize"]).expect("mapSize"), "{id}: {label} map size");
            [
                (s.left, "left"),
                (s.right, "right"),
                (s.top, "top"),
                (s.bottom, "bottom"),
                (s.near, "near"),
                (s.far, "far"),
                (s.bias, "bias"),
                (s.normal_bias, "normalBias"),
            ]
            .iter()
            .map(|(library, key)| (library - number(&want[*key])).abs())
            .fold(0.0, f32::max)
        }
        _ => panic!("{id}: {label} has a shadow in one of the library and the manifest, but not the other"),
    }
}

#[test]
fn every_rigs_skin_floor_shadow_blocks_and_absent_targets_match_the_manifest() {
    let Some(manifest) = manifest() else { return };
    let mut worst = 0.0_f32;
    for case in cases(manifest) {
        let id = case["id"].as_str().expect("an id");
        let rig_json = &case["spec"]["rig"];
        let rig = LightRig::load(case["rig"].as_str().expect("a rig name")).expect("the library has it");
        worst = worst.max((rig.skin_floor - number(&rig_json["skinFloor"])).abs());
        let lights = [
            ("key", Some(&rig.key), &rig_json["directional"]["key"]),
            ("rim", rig.rim.as_ref(), &rig_json["directional"]["rim"]),
            ("fill", rig.fill.as_ref(), &rig_json["directional"]["fill"]),
        ];
        for (label, library, want) in lights {
            // The library aims every directional light at the origin; a manifest that sets a target would break that.
            assert!(want.get("target").is_none(), "{id}: {label} sets a target, which the library does not model");
            match (library, want.is_null()) {
                (None, true) => {}
                (Some(light), false) => {
                    worst = worst.max(shadow_error(light.shadow.as_ref(), &want["shadow"], id, label));
                }
                _ => panic!("{id}: {label} is in one of the library and the manifest, but not the other"),
            }
        }
    }
    assert!(worst <= TOL, "largest skin floor or shadow error {worst:e} is above {TOL:e}");
    eprintln!("skin floors and shadow blocks match the manifest; max error {worst:e}");
}
