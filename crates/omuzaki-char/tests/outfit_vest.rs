//! The vest garment (#323): the conformance suite on every template and shape, the outline functions, the pieces it is
//! made of (lining, piping, seams, buttons), its parameters, and the vest over a shirt.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use std::f64::consts::{FRAC_PI_2, PI};

use omuzaki_char::outfit::common::{FabricStyle, LAYER_SHIRT, LAYER_VEST, PlaidBand};
use omuzaki_char::outfit::trim::Finish;
use omuzaki_char::outfit::vest::profile::{
    BACK_DIP, HEM_BACK, HEM_FRONT, HEM_SIDE, NECK_DEPTH, Profile, RIM_HEIGHT, along, angles,
};
use omuzaki_char::outfit::vest::{Vest, VestParams};
use omuzaki_char_api::{BodyShape, BuildServices, FitFact, PartOutput};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{ColourRef, Validate};
use omuzaki_face_mock::MockRasterizer;
use serde_json::json;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

/// The outline of the prototype's vest: the default without the armholes it lacks.
fn profile() -> Profile {
    VestParams { armhole: 0.0, ..VestParams::default() }.profile()
}

/// The vest alone on the soft-sd template, default shape.
fn soft_sd_vest(params: &VestParams) -> PartOutput {
    let def = conformance::character(
        "soft-sd",
        &BodyShape::default(),
        &[("garment", "vest", LAYER_VEST, serde_json::to_value(params).unwrap())],
    );
    conformance::build(def, &services()).expect("the vest builds").outputs.pop().expect("a vest").1
}

fn mesh<'a>(output: &'a PartOutput, name: &str) -> &'a omuzaki_char_api::PartMesh {
    output.meshes.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("no mesh `{name}`"))
}

// ------------------------------------------------------------------------------------------------------ the suite

#[test]
fn the_vest_passes_the_suite_on_every_template_and_shape() {
    let rows = conformance::assert_conforms::<Vest>(&VestParams::default(), &services());
    assert_eq!(rows.len(), 12, "4 templates x 3 shapes");
    for row in &rows {
        assert!(row.min_clearance_mm >= 0.5 * row.ease_mm, "{row:?}: half the ease at least");
        assert!(
            row.triangles >= 16_500 && row.triangles <= 17_200,
            "{row:?}: the prototype's 14160 vest and 2358 button triangles, plus the caps"
        );
    }
}

#[test]
fn a_patterned_vest_without_piping_seams_or_buttons_passes_too() {
    let fabric = FabricStyle::Plaid {
        ground: ColourRef::rgb(0x2a, 0x3a, 0x5a),
        period: 0.03,
        bands: vec![PlaidBand { colour: ColourRef::rgb(0xc8, 0x30, 0x30), offset: 0.004, width: 0.006 }],
    };
    let mut params = VestParams { piping: false, seams: false, ..VestParams::default() };
    params.common.fabric = fabric;
    params.buttons.count = 0;
    let rows = conformance::assert_conforms::<Vest>(&params, &services());
    assert!(rows.iter().all(|r| r.triangles == 2 * 2 * 34 * 84), "two shells of 34 x 84 quads: {rows:?}");
}

#[test]
fn a_wider_cloth_and_a_deeper_neck_pass_too() {
    let mut params = VestParams { thickness: 0.006, neck_depth: NECK_DEPTH + 0.08, ..VestParams::default() };
    params.common.ease = 0.006;
    params.buttons.count = 0;
    conformance::assert_conforms::<Vest>(&params, &services());
}

// ------------------------------------------------------------------------------------------------------- outline

#[test]
fn the_outline_is_the_prototypes() {
    let p = profile();
    // The soft-sd body's torso runs 0.325..0.545 m: the placeholders are the prototype's metres along it.
    let metres = |t: f64| 0.325 + 0.22 * t;
    let near = |a: f64, b: f64| assert!((a - b).abs() < 1e-9, "{a} vs {b}");
    near(metres(p.top(0.0)), 0.432);
    near(metres(p.tip()), 0.432);
    near(metres(p.top(FRAC_PI_2)), 0.5265);
    near(metres(p.top(1.2)), 0.5265);
    near(metres(p.hem(0.0)), 0.336);
    near(metres(p.hem(FRAC_PI_2)), 0.362);
    near(metres(p.hem(PI)), 0.356);
    // The V: a power curve reaching the rim at |sin phi| = 0.74.
    let phi = 0.3_f64;
    near(metres(p.top(phi)), 0.432 + (0.5265 - 0.432) * (phi.sin() / 0.74).powf(1.1));
    // The front hem: 0.85 knee, 0.9 power; the back: a parabola in sin.
    near(metres(p.hem(phi)), 0.336 + (0.362 - 0.336) * (phi.sin() / 0.85).powf(0.9));
    near(metres(p.hem(PI - 0.4)), 0.356 + 0.006 * (0.4_f64).sin().powi(2));
    // The back of the neck dips 12.5 mm, within |sin phi| 0.6 of the middle.
    near(metres(p.top(PI)), 0.5265 - 0.0125);
    near(metres(p.top(PI - 0.7)), 0.5265);
    assert!((p.rim - RIM_HEIGHT).abs() < 1e-12 && (BACK_DIP - 0.0125 / 0.22).abs() < 1e-12);
    assert_eq!((HEM_FRONT, HEM_SIDE, HEM_BACK), (along(0.336), along(0.362), along(0.356)));
}

#[test]
fn the_armholes_cut_the_sides_and_leave_the_front_and_back_alone() {
    let (plain, cut) = (profile(), VestParams::default().profile());
    for phi in [FRAC_PI_2, -FRAC_PI_2, FRAC_PI_2 + 0.2, -FRAC_PI_2 - 0.2] {
        assert!((plain.top(phi) - cut.top(phi) - cut.armhole).abs() < 1e-12, "the floor is flat at {phi}");
    }
    for phi in [0.0, 0.4, 0.6, PI, PI - 0.3, -0.6] {
        assert_eq!(plain.top(phi), cut.top(phi), "the rim is the prototype's at {phi}");
    }
    let between = cut.top(1.2);
    assert!(cut.top(FRAC_PI_2) < between && between < plain.top(1.2), "the edge climbs back: {between}");
    assert_eq!(cut.hem(FRAC_PI_2), plain.hem(FRAC_PI_2), "the hem is untouched");
}

#[test]
fn both_halves_of_the_outline_meet_at_the_sides_and_are_mirrored() {
    let p = profile();
    for side in [FRAC_PI_2, -FRAC_PI_2] {
        for e in [1e-9, -1e-9] {
            assert!((p.top(side + e) - p.top(side)).abs() < 1e-6, "the rim is continuous at {side}");
            assert!((p.hem(side + e) - p.hem(side)).abs() < 1e-6, "the hem is continuous at {side}");
        }
    }
    for phi in [0.2, 0.9, 1.4, 2.0, 3.0] {
        assert_eq!(p.top(phi), p.top(-phi));
        assert_eq!(p.hem(phi), p.hem(-phi));
        assert!(p.top(phi) > p.hem(phi), "a panel at {phi}");
        assert!(VestParams::default().profile().top(phi) > p.hem(phi), "and with the armholes");
    }
}

#[test]
fn the_angles_bunch_toward_the_front() {
    let a = angles(84);
    assert_eq!(a.len(), 85);
    assert_eq!((a[0], a[84]), (-PI, PI));
    assert_eq!(a[42], 0.0);
    assert!(a.windows(2).all(|w| w[1] > w[0]), "increasing");
    assert!(a[43] - a[42] < a[1] - a[0], "denser at the front than the back");
    // The step at the front is (1/42)^1.5 pi.
    assert!((a[43] - (1.0_f64 / 42.0).powf(1.5) * PI).abs() < 1e-12);
}

// ----------------------------------------------------------------------------------------------------- the pieces

#[test]
fn the_vest_is_the_prototypes_pieces_in_order() {
    let output = soft_sd_vest(&VestParams::default());
    assert_eq!(output.meshes.len(), 2);
    let vest = mesh(&output, "garment-vest");
    let g = &vest.geometry;
    // Two 35 x 85 shells, two piping tubes of 85 rings of 7, five seams (13, 15, 11, 15, 11, 13 rings... of 6), then
    // the caps of the six seams' twelve tips, 5 points each.
    let seams = (13 + 2 * 15 + 2 * 11 + 13) * 6;
    assert_eq!(g.positions.len(), 2 * 35 * 85 + 2 * 85 * 7 + seams + 12 * 5);
    assert_eq!(g.colours.len(), g.positions.len(), "the vest has vertex colours");
    let at = |k: usize| omuzaki_core::glam::Vec3::new(g.colours[k][0], g.colours[k][1], g.colours[k][2]);
    // The lining is constant, the outer shell darker toward the hem, the piping and seams brighter than white.
    assert_eq!(at(35 * 85 + 5), omuzaki_core::glam::Vec3::new(0.8, 0.8, 0.82));
    assert!(at(2).x < at(34 * 85 + 2).x, "darker at the hem than at the rim");
    assert_eq!(at(2 * 35 * 85 + 3), omuzaki_core::glam::Vec3::new(2.6, 2.5, 2.8));
    assert_eq!(at(2 * 35 * 85 + 2 * 85 * 7 + 3), omuzaki_core::glam::Vec3::new(2.4, 2.3, 2.6));
}

#[test]
fn the_lining_is_inside_the_shell_by_the_thickness() {
    let params = VestParams::default();
    let output = soft_sd_vest(&params);
    let p = &mesh(&output, "garment-vest").geometry.positions;
    let shell = 35 * 85;
    // At the same angle and height the two grids lie on the same ray from the section's centre: the gap is 2.8 mm.
    for (i, j) in [(10, 42), (20, 10), (30, 60), (5, 80)] {
        let (outer, inner) = (p[i * 85 + j], p[shell + i * 85 + j]);
        let gap = f64::from(outer.distance(inner));
        assert!((gap - params.thickness).abs() < 5e-4, "row {i} column {j}: {gap} m apart");
    }
}

#[test]
fn the_buttons_are_gold_with_the_highlight_band() {
    let output = soft_sd_vest(&VestParams::default());
    let buttons = mesh(&output, "garment-buttons");
    let gold = output.materials.iter().find(|(m, _)| *m == buttons.material).expect("the buttons' material");
    let omuzaki_render_api::Shading::Program { name, params } = &gold.1.shading else {
        panic!("gold is a shading program, not toon")
    };
    assert_eq!(name.as_str(), "band-highlight");
    let float = |k: &str| match params.iter().find(|(n, _)| n.as_str() == k).map(|(_, v)| v) {
        Some(omuzaki_render_api::ParamValue::Float(f)) => *f,
        other => panic!("{k}: {other:?}"),
    };
    assert_eq!((float("edge0"), float("edge1"), float("strength")), (0.86, 0.90, 0.6));
    assert!(
        output
            .material_classes
            .iter()
            .any(|(m, c)| *m == buttons.material && *c == omuzaki_char_api::MaterialClass::Metal)
    );
    // Three buttons 26 mm apart down the centre front: one cluster each, 14 mm wide.
    let ys: Vec<f32> = {
        let mut v: Vec<f32> = buttons.geometry.positions.iter().map(|p| p.y).collect();
        v.sort_by(f32::total_cmp);
        v
    };
    let mut clusters = vec![vec![ys[0]]];
    for y in &ys[1..] {
        if *y - clusters.last().unwrap().last().unwrap() > 0.006 {
            clusters.push(Vec::new());
        }
        clusters.last_mut().unwrap().push(*y);
    }
    assert_eq!(clusters.len(), 3);
    let centres: Vec<f32> = clusters.iter().map(|c| 0.5 * (c[0] + c[c.len() - 1])).collect();
    assert!(((centres[1] - centres[0]) - 0.026).abs() < 0.002 && ((centres[2] - centres[1]) - 0.026).abs() < 0.002);
    // And they sit within the vest's front panel: above the hem tip, below the V tip.
    assert!(centres[0] > 0.336 && centres[2] < 0.432 + 0.02, "{centres:?}");
}

#[test]
fn the_vest_is_skinned_to_the_torso_and_every_vertex_has_weights() {
    let output = soft_sd_vest(&VestParams::default());
    for m in &output.meshes {
        let g = &m.geometry;
        assert_eq!(g.joints.len(), g.positions.len(), "{}", m.name);
        assert!(!m.joint_bones.is_empty());
    }
    // The hem is on the hips, the rim on the chest and above.
    let vest = mesh(&output, "garment-vest");
    let bone = |k: usize| vest.joint_bones[vest.geometry.joints[k][0] as usize].clone();
    let weights = &vest.geometry.weights;
    assert!(weights[0][0] > 0.99, "bottom row, almost wholly one bone");
    assert_ne!(bone(0), bone(34 * 85), "the hem and the rim are skinned to different bones");
}

#[test]
fn piping_and_seams_and_buttons_are_each_optional() {
    let count = |p: &VestParams| soft_sd_vest(p).meshes.iter().map(|m| m.geometry.positions.len()).sum::<usize>();
    let full = count(&VestParams::default());
    let mut params = VestParams { piping: false, ..VestParams::default() };
    assert_eq!(full - count(&params), 2 * 85 * 7, "the piping is two tubes");
    params.piping = true;
    params.seams = false;
    assert_eq!(full - count(&params), (13 + 2 * 15 + 2 * 11 + 13) * 6 + 12 * 5, "the seams and their caps");
    params.seams = true;
    params.buttons.count = 0;
    let output = soft_sd_vest(&params);
    assert_eq!(output.meshes.len(), 1, "no buttons mesh");
    params.buttons.count = 4;
    params.buttons.spacing = 0.018;
    assert!(count(&params) > full, "four buttons, not three");
}

#[test]
fn the_buttons_take_their_finish_and_radius_from_the_parameters() {
    let mut params = VestParams::default();
    params.buttons.radius = 0.004;
    params.buttons.finish = Finish::cloth(ColourRef::rgb(10, 20, 30));
    let output = soft_sd_vest(&params);
    let buttons = mesh(&output, "garment-buttons");
    let material = &output.materials.iter().find(|(m, _)| *m == buttons.material).unwrap().1;
    assert_eq!(material.base, Colour::rgb(10, 20, 30));
    assert!(matches!(material.shading, omuzaki_render_api::Shading::Toon(_)), "cloth, not metal");
    let width = buttons.geometry.positions.iter().map(|p| p.x.abs()).fold(0.0, f32::max);
    assert!(width < 0.0045, "a 4 mm button: {width}");
}

#[test]
fn the_neck_depth_moves_the_v() {
    let lowest_rim = |depth: f64| {
        let mut params = VestParams { neck_depth: depth, ..VestParams::default() };
        params.buttons.count = 0;
        let output = soft_sd_vest(&params);
        // The outer shell's rim row at the front (column 42, the last row) is the V's tip.
        mesh(&output, "garment-vest").geometry.positions[34 * 85 + 42].y
    };
    let (shallow, deep) = (lowest_rim(0.30), lowest_rim(0.55));
    assert!(((shallow - deep) - 0.25 * 0.22).abs() < 2e-3, "{shallow} vs {deep}: 0.25 of the torso apart");
}

// ------------------------------------------------------------------------------------------------------- stacking

#[test]
fn the_vest_stands_over_a_shirt_and_publishes_its_outer_shell() {
    let vest = serde_json::to_value(VestParams::default()).unwrap();
    for template in conformance::TEMPLATES {
        for (label, shape) in conformance::shapes() {
            let def = conformance::character(
                template,
                &shape,
                &[
                    ("shirt", "mock-garment", LAYER_SHIRT, json!({ "ease": 0.004 })),
                    ("vest", "vest", LAYER_VEST, vest.clone()),
                ],
            );
            let stack = conformance::build(def, &services()).unwrap();
            let shirt = stack.before("vest").outer_fit().expect("the shirt's envelope");
            for mesh in &stack.output("vest").meshes {
                let g = &mesh.geometry;
                // The suite's tools rebuild the fit's tables per call: a spread of vertices and triangles.
                for (k, p) in g.positions.iter().enumerate().step_by(g.positions.len() / 40) {
                    assert!(
                        !conformance::inside(shirt.as_ref(), p.as_dvec3()),
                        "{template}/{label}: `{}` vertex {k}",
                        mesh.name
                    );
                }
                for (n, tri) in g.indices.chunks_exact(3).enumerate().step_by(g.indices.len() / 3 / 40) {
                    let corners = [0, 1, 2].map(|c| g.positions[tri[c] as usize].as_dvec3());
                    assert!(
                        conformance::crossing(shirt.as_ref(), corners).is_none(),
                        "{template}/{label}: `{}` triangle {n} reaches the shirt",
                        mesh.name
                    );
                }
            }
            // The vest publishes the outside of its shell: ease 4.2 mm + 2.8 mm over the shirt's, widened for the cut.
            let fits = &stack.published.get::<FitFact>().fits;
            assert_eq!(fits.len(), 3, "the body, the shirt, the vest");
            let (a, b) = (
                fits[1].section(omuzaki_char_api::BodyRegion::Torso, 0.5).unwrap(),
                fits[2].section(omuzaki_char_api::BodyRegion::Torso, 0.5).unwrap(),
            );
            let scale = omuzaki_char::outfit::common::enclosing_scale(omuzaki_char::outfit::vest::VEST_EXPONENT) as f32;
            assert!(
                ((a.radii + Vec2::splat(0.007)) * scale - b.radii).abs().max_element() < 1e-6,
                "{template}/{label}"
            );
        }
    }
}

// ------------------------------------------------------------------------------------------------------ parameters

#[test]
fn parameters_are_checked_and_round_trip() {
    let params = VestParams::default();
    assert!(params.validate().is_ok());
    let again: VestParams = serde_json::from_value(serde_json::to_value(&params).unwrap()).unwrap();
    assert_eq!(again, params);
    assert!(serde_json::from_value::<VestParams>(json!({ "neck_dept": 0.2 })).is_err(), "a typo fails");
    let bad = |edit: &dyn Fn(&mut VestParams)| {
        let mut p = VestParams::default();
        edit(&mut p);
        p.validate().expect_err("refused").to_string()
    };
    assert!(bad(&|p| p.thickness = f64::NAN).contains("thickness"));
    assert!(bad(&|p| p.thickness = 0.0).contains("thickness"));
    assert!(bad(&|p| p.common.ease = 0.079).contains("thickness"), "ease and thickness together");
    assert!(bad(&|p| p.rim = 2.0).contains("rim"));
    assert!(bad(&|p| p.neck_depth = f64::INFINITY).contains("neck_depth"));
    assert!(bad(&|p| p.hem.side = -0.1).contains("hem/side"));
    assert!(
        bad(&|p| {
            p.rim = 0.35;
            p.neck_depth = 0.2;
        })
        .contains("outline"),
        "a V that runs into the hem"
    );
    assert!(bad(&|p| p.armhole = 0.7).contains("outline"), "an armhole that cuts down to the hem");
    assert!(bad(&|p| p.armhole = -0.1).contains("armhole"));
    assert!(bad(&|p| p.buttons.count = 65).contains("count"));
    assert!(bad(&|p| p.buttons.radius = 0.0).contains("radius"));
    assert!(bad(&|p| p.buttons.first = 1.5).contains("first"));
}

#[test]
fn buttons_that_run_below_the_hem_are_refused_naming_why() {
    let mut params = VestParams::default();
    params.buttons.count = 6;
    let def = conformance::character(
        "soft-sd",
        &BodyShape::default(),
        &[("garment", "vest", LAYER_VEST, serde_json::to_value(&params).unwrap())],
    );
    let error = conformance::build(def, &services()).err().expect("six buttons don't fit").to_string();
    assert!(error.contains("below the hem") && error.contains("fewer"), "{error}");
}

#[test]
fn the_build_is_deterministic() {
    let params = VestParams::default();
    let (a, b) = (soft_sd_vest(&params), soft_sd_vest(&params));
    for (x, y) in a.meshes.iter().zip(&b.meshes) {
        assert_eq!(x.geometry.positions, y.geometry.positions);
        assert_eq!(x.geometry.indices, y.geometry.indices);
    }
}

// ----------------------------------------------------------------------------------------------------- the pictures

/// Writes pictures of the vest, front, side and back, on soft-sd and standard, over the body alone and over a shirt, to
/// the folder `OMUZAKI_VEST_PPM` names (nothing without it). Taste is judged on the pictures, not on numbers.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_vest_when_asked() {
    use omuzaki_char_api::{BuildOptions, CharId, CharacterBuilder, CharacterDef, RegistryBuilder};
    use omuzaki_core::glam::{Mat4, UVec2, Vec3};
    use omuzaki_render_api::{
        AlphaEncoding, Background, Camera, Instance, Light, Post, Quality, Renderer, RendererExt, Scene, Tonemap,
    };
    use omuzaki_render_mock::MockRenderer;

    let Some(directory) = std::env::var_os("OMUZAKI_VEST_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let vest = serde_json::to_value(VestParams::default()).unwrap();
    for template in ["soft-sd", "standard"] {
        for (variant, shirt) in [("body", false), ("shirt", true)] {
            let mut parts = json!({
                "body": { "kind": "body", "params": serde_json::to_value(omuzaki_char_api::BodyParams::default()).unwrap() },
                "vest": { "kind": "vest", "layer": LAYER_VEST, "params": vest },
            });
            if shirt {
                parts["shirt"] = json!({ "kind": "mock-garment", "layer": LAYER_SHIRT, "params": { "ease": 0.004 } });
            }
            let node = json!({
                "name": "Vest", "template": template,
                "palette": { "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80", "outfit": "#c8c8d0" },
                "parts": parts,
            });
            let def = CharacterDef::from_node(&node).unwrap();
            let built = RegistryBuilder
                .build(&CharId::new("vest").unwrap(), &def, &BuildOptions::default(), &services())
                .unwrap();
            let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
            for m in built.meshes.iter().filter(|m| m.mesh.name.contains("vest")) {
                for p in &m.mesh.geometry.positions {
                    lo = lo.min(*p);
                    hi = hi.max(*p);
                }
            }
            let (centre, size) = ((lo + hi) / 2.0, (hi - lo).max_element());
            for (view, azimuth_deg) in [("front", 0.0_f32), ("side", 90.0), ("back", 180.0)] {
                let mut renderer = MockRenderer::new();
                let eye = centre
                    + Mat4::from_rotation_y(azimuth_deg.to_radians()).transform_vector3(Vec3::new(
                        0.0,
                        0.1,
                        3.4 * size,
                    ));
                let mut scene = Scene::new(Camera::look_at(eye, centre, Vec3::Y, Camera::default().projection));
                scene.background = Background::Solid { colour: Colour::hex(0x001e_1c24) };
                scene.lights = vec![
                    Light::Hemisphere {
                        sky: Colour::hex(0x00f1_eef8),
                        ground: Colour::hex(0x004a_3e36),
                        intensity: 0.75,
                    },
                    Light::Directional {
                        to_light: Vec3::new(1.6, 3.0, 2.4),
                        colour: Colour::hex(0x00ff_f6ec),
                        intensity: 1.7,
                        casts_shadow: false,
                    },
                ];
                scene.post = Post { exposure: 1.0, tonemap: Tonemap::None, ..Post::default() };
                for built_mesh in &built.meshes {
                    let mesh = renderer.create_mesh(&built_mesh.mesh.geometry).unwrap();
                    let mut spec = built.materials[&built_mesh.mesh.material].clone();
                    spec.overlays.clear();
                    let material = renderer.create_material(&spec.to_material(&|_| None, &|_| None).unwrap()).unwrap();
                    scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
                }
                let quality = Quality { samples: 4, ..Quality::offline() };
                let image =
                    renderer.render_image(UVec2::splat(768), AlphaEncoding::Straight, &scene, &quality).unwrap();
                let mut ppm = format!("P6\n{} {}\n255\n", image.size.x, image.size.y).into_bytes();
                for y in 0..image.size.y {
                    for x in 0..image.size.x {
                        let [r, g, b, _] = image.pixel(x, y).unwrap();
                        ppm.extend([r, g, b]);
                    }
                }
                std::fs::write(directory.join(format!("{template}-{variant}-{view}.ppm")), ppm).unwrap();
            }
        }
    }
}
