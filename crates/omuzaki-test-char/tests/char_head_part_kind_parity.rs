//! Parity of the `head` part kind (`omuzaki_char::head::part`) with the prototype's `head.js` `build`: the skin materials,
//! the neck mesh and the skin weights of the head and the neck.
//!
//! One oracle: the committed fixture `fixtures/char_head_part/head_part.json`, made by the prototype's generator script running the
//! prototype's own `head.js` `build` under Node with three.js and a recording context. It holds the prototype's generic
//! style constants and synthetic face profiles only, so it runs everywhere and no character data is committed. (The
//! JS dumps in a local assets folder hold the same numbers for the sample character, so they add nothing a fixture
//! derived from no character does not already cover.)
//!
//! One deliberate difference: the prototype draws the face outline in a fixed `#d49a8e`; the Rust material derives it
//! from the palette's line colour so a recolour reaches it ([`FACE_LINE_LIGHTEN`](omuzaki_char::head::material)). With
//! the prototype's palette the two agree to one level per channel, which is what the test allows.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers load fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_possible_truncation, clippy::float_cmp)] // fixture numbers are f64 JSON; exact constants are compared exactly

use std::collections::BTreeMap;

use omuzaki_char::head::neck;
use omuzaki_char::head::part::{below_chin_line, head_weights};
use omuzaki_char_api::{
    BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, Chin, HeadOutline, HeadParams, MaterialSpec,
    Neck, PartDef, PartId, PartKindId, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_render_api::{Shading, Skin};
use omuzaki_test::meshcmp::{MeshTolerance, compare_meshes};
use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Deserialize)]
struct Outline {
    color: u32,
    width: f32,
}

#[derive(Deserialize)]
struct Material {
    color: u32,
    shade: u32,
    shift: f32,
    soft: f32,
    rim: u32,
    rim_strength: f32,
    double_sided: bool,
    skin_floor: Option<f32>,
    outline: Option<Outline>,
    receive_shadow: bool,
}

#[derive(Deserialize)]
struct Weighted {
    y: f32,
    w: Vec<(String, f32)>,
}

#[derive(Deserialize)]
struct NeckDump {
    positions: Vec<f32>,
    normals: Vec<f32>,
    indices: Vec<u32>,
    weights: Vec<Weighted>,
}

#[derive(Deserialize)]
struct Constants {
    #[serde(rename = "W0")]
    half_width: f64,
    #[serde(rename = "TOP")]
    top: f64,
    #[serde(rename = "H")]
    height: f64,
    #[serde(rename = "P")]
    exponent: f64,
    puff: f64,
    chin: Option<f64>,
    #[serde(rename = "stretchTop")]
    stretch_top: Option<f64>,
    #[serde(rename = "minRatio")]
    min_ratio: Option<f64>,
}

#[derive(Deserialize)]
struct HeadWeights {
    constants: Constants,
    weights: Vec<Weighted>,
}

#[derive(Deserialize)]
struct Fixture {
    centre: [f64; 3],
    hue_keep: f32,
    materials: BTreeMap<String, Material>,
    neck: NeckDump,
    head_weights: BTreeMap<String, HeadWeights>,
}

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("fixtures/char_head_part/head_part.json")).unwrap()
}

/// The sRGB colour of a prototype hex number.
fn colour(hex: u32) -> Colour {
    Colour::hex(hex)
}

/// Whether two colours differ by at most `levels` in each channel.
fn near(a: Colour, b: Colour, levels: u8) -> bool {
    [(a.r, b.r), (a.g, b.g), (a.b, b.b)].iter().all(|(x, y)| x.abs_diff(*y) <= levels)
}

/// The mock character's palette is the prototype's skin colours; swap its head for the real `head` kind.
fn built_materials() -> BTreeMap<String, MaterialSpec> {
    let mut def: CharacterDef = mock_character().unwrap();
    def.parts.clear();
    let part = |kind: &str| PartDef {
        kind: PartKindId::new(kind).unwrap(),
        parent: None,
        enabled: true,
        layer: 0,
        attach: None,
        params: json!({}),
    };
    def.parts.insert(PartId::new("body").unwrap(), part("mock-body"));
    def.parts.insert(PartId::new("head").unwrap(), part("head"));
    let built = RegistryBuilder
        .build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    built.materials.into_iter().map(|(name, spec)| (name.as_str().to_owned(), spec)).collect()
}

#[test]
fn the_skin_materials_equal_the_prototypes_field_by_field() {
    let (fixture, built) = (fixture(), built_materials());
    assert_eq!(fixture.materials.len(), 3);
    for (js_name, js) in &fixture.materials {
        let spec = &built[&format!("head-{js_name}")];
        let Shading::Toon(toon) = &spec.shading else { panic!("{js_name}: toon") };
        assert_eq!(spec.base, colour(js.color), "{js_name} base");
        assert_eq!(toon.shade, colour(js.shade), "{js_name} shade");
        assert_eq!(toon.rim.colour, colour(js.rim), "{js_name} rim colour");
        assert_eq!((toon.shift, toon.softness, toon.rim.strength), (js.shift, js.soft, js.rim_strength), "{js_name}");
        assert_eq!(toon.rim.power, 3.0, "{js_name} rim power is the prototype's default");
        assert_eq!(spec.double_sided, js.double_sided, "{js_name} sides");
        assert_eq!(spec.receives_shadows, js.receive_shadow, "{js_name} receives shadows");
        // The prototype's neck has no skin treatment. Ours carries the default floor and the hue-keep, so a coloured
        // light can't turn the neck a different hue from the face beside it (#1165); the others are the prototype's.
        let floor = if *js_name == "skin-neck" { Some(Skin::default().floor) } else { js.skin_floor };
        assert_eq!(toon.skin.map(|s| s.floor), floor, "{js_name} skin floor");
        if let Some(skin) = toon.skin {
            assert_eq!(skin.hue_keep, fixture.hue_keep, "{js_name} hue keep");
        }
        match (&spec.outline, &js.outline) {
            (None, None) => {}
            (Some(ours), Some(theirs)) => {
                assert_eq!(ours.width, theirs.width, "{js_name} outline width");
                // The face line is derived from the palette (see the module docs); the ears' is the slot itself.
                let levels = u8::from(js_name == "skin-head");
                assert!(near(ours.colour, colour(theirs.color), levels), "{js_name} outline {:?}", ours.colour);
            }
            other => panic!("{js_name}: outline {other:?}"),
        }
        // The face carries the painted face's Base layer and its Overlay (the sweat drop, in front of the hair).
        let overlays = if js_name == "skin-head" { 2 } else { 0 };
        assert_eq!(spec.overlays.len(), overlays, "{js_name} overlays");
    }
}

fn vec3s(flat: &[f32]) -> Vec<Vec3> {
    flat.as_chunks::<3>().0.iter().map(|&[x, y, z]| Vec3::new(x, y, z)).collect()
}

#[test]
fn the_neck_mesh_is_within_t_geom_of_the_prototypes_cylinder() {
    let fixture = fixture();
    let expected = MeshData {
        positions: vec3s(&fixture.neck.positions),
        normals: vec3s(&fixture.neck.normals),
        indices: fixture.neck.indices.clone(),
        ..MeshData::default()
    };
    // `head.js` translates the cylinder to y 0.555 (top at 0.61) and z -0.022; the head joint is at 0.56 and the weight
    // line at 0.60.
    let top = Vec3::new(0.0, 0.56 + neck::TOP_ABOVE_JOINT, fixture.centre[2] as f32);
    let actual = neck::mesh(&Neck::default(), top, 0.56 + neck::WEIGHT_LINE_ABOVE_JOINT).unwrap().unwrap();
    let report = compare_meshes(&actual, &expected, MeshTolerance::T_GEOM);
    assert!(report.passed(), "{report}");
    println!("neck: {report}");
}

/// The prototype's `setWeights`: drop empty entries, keep the largest four, normalise.
fn normalised(w: &[(String, f32)]) -> Vec<(String, f32)> {
    let kept: Vec<_> = w.iter().filter(|(_, v)| *v > 0.0).cloned().collect();
    let sum: f32 = kept.iter().map(|(_, v)| v).sum();
    kept.into_iter().map(|(n, v)| (n, v / sum)).collect()
}

/// Our weights as bone-name pairs for the bones `names` (joint index to bone).
fn named(joints: [u16; 4], weights: [f32; 4], names: &[&str]) -> Vec<(String, f32)> {
    joints.iter().zip(weights).filter(|(_, w)| *w > 0.0).map(|(j, w)| (names[usize::from(*j)].to_owned(), w)).collect()
}

fn assert_same(ours: &[(String, f32)], theirs: &[(String, f32)], what: &str) {
    let mut ours = ours.to_vec();
    let mut theirs = theirs.to_vec();
    ours.sort_by(|a, b| a.0.cmp(&b.0));
    theirs.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(ours.len(), theirs.len(), "{what}: {ours:?} against {theirs:?}");
    for ((n, a), (m, b)) in ours.iter().zip(&theirs) {
        assert!(n == m && (a - b).abs() < 1e-5, "{what}: {ours:?} against {theirs:?}");
    }
}

#[test]
fn the_neck_weights_match_the_prototypes_at_every_height() {
    let fixture = fixture();
    // The prototype's neck line, world 0.60, over a head joint at 0.56.
    let line = 0.56 + neck::WEIGHT_LINE_ABOVE_JOINT;
    for sample in &fixture.neck.weights {
        let (joints, weights) = neck::weights(sample.y, line);
        assert_same(
            &named(joints, weights, &["head", "neck", "chest"]),
            &normalised(&sample.w),
            &format!("y {}", sample.y),
        );
    }
}

/// The contract's parameters for a synthetic prototype profile, on the prototype's head centre.
fn params_for(c: &Constants, centre: [f64; 3]) -> HeadParams {
    let chin = c.chin.map(|y| Chin {
        y: y - centre[1],
        stretch_top: c.stretch_top.unwrap() - centre[1],
        min_ratio: c.min_ratio.unwrap(),
    });
    let mut params = HeadParams::default();
    params.cranium.centre = centre;
    params.outline = HeadOutline {
        half_width: c.half_width,
        top: c.top - centre[1],
        height: c.height,
        exponent: c.exponent,
        exponent_lower: None,
        chin,
        cheek_puff: c.puff,
    };
    params
}

#[test]
fn the_head_weights_match_the_prototypes_for_faces_with_and_without_a_chin() {
    let fixture = fixture();
    assert_eq!(fixture.head_weights.len(), 3);
    for (name, profile) in &fixture.head_weights {
        let line = below_chin_line(&params_for(&profile.constants, fixture.centre));
        for sample in &profile.weights {
            let local_y = (f64::from(sample.y) - fixture.centre[1]) as f32;
            let (joints, weights) = head_weights(local_y, line);
            assert_same(
                &named(joints, weights, &["head", "neck"]),
                &normalised(&sample.w),
                &format!("{name} y {}", sample.y),
            );
        }
    }
}
