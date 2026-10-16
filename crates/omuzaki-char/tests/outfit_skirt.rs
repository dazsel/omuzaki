//! The `skirt` garment (#192): the conformance suite on every template and shape, the prototype's grid layout, the hem
//! above the knee on every template, the weights, the plaid, the parameter checks and the cloth hook.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::outfit::common::{FabricStyle, Garment};
use omuzaki_char::outfit::skirt::cloth::cloth_sheet;
use omuzaki_char::outfit::skirt::pleats::{self, Plan};
use omuzaki_char::outfit::skirt::{
    LAYER_SKIRT, MAX_HEM, PLEAT, PLEATS, PROTOTYPE_BLEND, PROTOTYPE_REACH, Pleats, Skirt, SkirtParams, SkirtProfile,
    Tartan, ThighPull,
};
use omuzaki_char_api::{BuildServices, FitFact, JointRef, PartKind};
use omuzaki_core::colour::Colour;
use omuzaki_face_api::{ColourRef, Validate};
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

fn built(template: &str, p: &SkirtParams) -> conformance::Stack {
    let value = serde_json::to_value(p).unwrap();
    let def = conformance::character(
        template,
        &omuzaki_char_api::BodyShape::default(),
        &[("skirt", "skirt", LAYER_SKIRT, value)],
    );
    conformance::build(def, &services()).unwrap()
}

#[test]
fn the_default_skirt_conforms_on_every_template_and_shape() {
    let rows = conformance::assert_conforms::<Skirt>(&SkirtParams::default(), &services());
    assert_eq!(rows.len(), 12);
}

#[test]
fn a_skirt_without_the_roll_or_the_plaid_conforms() {
    let plain = SkirtParams {
        hem_roll: 0.0,
        plaid: None,
        common: omuzaki_char::outfit::common::GarmentCommon {
            fabric: FabricStyle::Plain { colour: ColourRef::rgb(120, 40, 40) },
            ..Default::default()
        },
        ..SkirtParams::default()
    };
    conformance::assert_conforms::<Skirt>(&plain, &services());
}

#[test]
fn a_patterned_common_fabric_is_the_cloth_when_there_is_no_plaid() {
    let stripes = SkirtParams {
        plaid: None,
        common: omuzaki_char::outfit::common::GarmentCommon {
            fabric: FabricStyle::Stripes {
                ground: ColourRef::rgb(30, 30, 60),
                stripe: ColourRef::rgb(200, 200, 220),
                period: 0.01,
                width: 0.003,
                direction: omuzaki_char::outfit::common::StripeDirection::Vertical,
            },
            ..Default::default()
        },
        ..SkirtParams::default()
    };
    let stack = built("standard", &stripes);
    assert_eq!(stack.output("skirt").textures.len(), 1);
}

#[test]
fn a_skirt_of_more_pleats_conforms() {
    let p = SkirtParams { pleats: Pleats { count: 24, ..Pleats::default() }, ..SkirtParams::default() };
    conformance::assert_conforms::<Skirt>(&p, &services());
}

#[test]
fn the_grid_is_the_prototypes() {
    let stack = built("soft-sd", &SkirtParams::default());
    let out = stack.output("skirt");
    let pleats = &out.meshes[0].geometry;
    // 12 rows x 145 columns, twice, and the roll: 145 x 9.
    assert_eq!(pleats.positions.len(), 2 * 12 * 145 + 145 * 9);
    assert_eq!(pleats.indices.len() / 3, 2 * (2 * 11 * 144) + 144 * 8 * 2);
    // Two band lines of 145 rings of 5.
    let band = &out.meshes[1].geometry;
    assert_eq!(band.positions.len(), 2 * 145 * 5);
    assert_eq!(band.indices.len() / 3, 2 * 144 * 4 * 2);
    assert!(!out.meshes[1].casts_shadow, "the band lines cast no shadow");
}

#[test]
fn the_folds_only_go_inwards() {
    let params = SkirtParams::default();
    let stack = built("standard", &params);
    // Every crest is on the base ellipse and every valley inside it: no outer vertex is farther out than the crest at its
    // height, found as the farthest vertex of the same row.
    let columns = pleats::columns(PLEATS, &PLEAT);
    assert_eq!(columns.len(), PLEATS * PLEAT.len() + 1);
    assert!(columns.iter().all(|c| (-1.0..=0.0).contains(&c.fold)));
    let first = columns[0];
    let last = columns[columns.len() - 1];
    assert!((last.phi - first.phi - std::f64::consts::TAU).abs() < 1e-12 && last.fold == 0.0);
    let out = &stack.output("skirt").meshes[0].geometry;
    let row0 = &out.positions[..145];
    assert!((row0[0].x - row0[144].x).abs() < 1e-6, "the closing column repeats the first");
}

#[test]
fn the_pleat_depth_fades_to_nothing_at_the_waist_and_grows_toward_the_hem() {
    let rx = [[0.0, 0.12], [1.0, 0.08]];
    let rz = [[0.0, 0.11], [1.0, 0.07]];
    let cz = [[0.0, 0.0], [1.0, 0.0]];
    let plan = Plan::new(0.25, 0.4, [&rx, &rz, &cz], 0.0125).unwrap();
    assert_eq!(plan.amplitude(0.4), 0.0);
    assert!(plan.amplitude(0.25) > plan.amplitude(0.33), "deeper toward the hem");
    assert!((plan.amplitude(0.25) - 0.0125 * 1.12).abs() < 1e-12);
    // A valley's hem rides higher than a crest's.
    let valley = pleats::Column { phi: 0.0, fold: -1.0 };
    let crest = pleats::Column { phi: 0.0, fold: 0.0 };
    assert!(plan.hem_of(&valley) > plan.hem_of(&crest));
}

#[test]
fn the_hem_stays_above_the_knee_on_every_template() {
    for template in conformance::TEMPLATES {
        let stack = built(template, &SkirtParams::default());
        let fit = stack.before("skirt").get::<FitFact>().fits.first().unwrap().clone();
        let knee = f64::from(fit.joint(&JointRef::Humanoid(HumanoidBone::LeftLowerLeg)).unwrap().y);
        let lowest = stack
            .output("skirt")
            .meshes
            .iter()
            .flat_map(|m| m.geometry.positions.iter())
            .map(|p| f64::from(p.y))
            .fold(f64::INFINITY, f64::min);
        assert!(lowest > knee + 0.02, "{template}: the hem ({lowest}) is within 2 cm of the knee joint ({knee})");
    }
    // Even at the lowest hem the parameters allow.
    let lowest_hem = SkirtParams { hem: MAX_HEM, ..SkirtParams::default() };
    assert!(lowest_hem.validate().is_ok());
    for template in conformance::TEMPLATES {
        let stack = built(template, &lowest_hem);
        let fit = stack.before("skirt").get::<FitFact>().fits.first().unwrap().clone();
        let knee = f64::from(fit.joint(&JointRef::Humanoid(HumanoidBone::LeftLowerLeg)).unwrap().y);
        let lowest = stack.output("skirt").meshes[0]
            .geometry
            .positions
            .iter()
            .map(|p| f64::from(p.y))
            .fold(f64::INFINITY, f64::min);
        assert!(lowest > knee, "{template}: the lowest hem ({lowest}) is below the knee joint ({knee})");
    }
}

#[test]
fn a_longer_hem_makes_a_longer_skirt() {
    let low = |hem: f64| {
        let stack = built("standard", &SkirtParams { hem, ..SkirtParams::default() });
        stack.output("skirt").meshes[0].geometry.positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min)
    };
    assert!(low(0.5) > low(0.65) && low(0.65) > low(0.75));
}

#[test]
fn the_weights_keep_the_waist_and_the_far_sides_on_the_hips_and_the_thighs_on_the_legs() {
    // The prototype's cap: a thigh carries at most 95 % of a vertex.
    let stack = built(
        "soft-sd",
        &SkirtParams {
            thigh_cap: 0.95,
            thigh_reach: PROTOTYPE_REACH,
            hip_blend: PROTOTYPE_BLEND,
            thigh_pull: ThighPull::Falloff,
            ..SkirtParams::default()
        },
    );
    let mesh = &stack.output("skirt").meshes[0];
    let g = &mesh.geometry;
    let hips = JointRef::HIPS;
    let on = |v: usize, joint: &JointRef| -> f32 {
        (0..4).filter(|&k| &mesh.joint_bones[usize::from(g.joints[v][k])] == joint).map(|k| g.weights[v][k]).sum()
    };
    let left = JointRef::Humanoid(HumanoidBone::LeftUpperLeg);
    let right = JointRef::Humanoid(HumanoidBone::RightUpperLeg);
    let mut thigh_cap_seen = false;
    for v in 0..g.positions.len() {
        let total: f32 = g.weights[v].iter().sum();
        assert!((total - 1.0).abs() < 1e-5, "vertex {v}: weights sum to {total}");
        let p = g.positions[v];
        let thighs = on(v, &left) + on(v, &right);
        assert!(thighs <= 0.95 + 1e-5, "vertex {v}: thighs carry {thighs}");
        if p.y > 0.43 {
            assert!(on(v, &hips) > 0.99, "vertex {v} above the hips is on the hips");
        }
        if p.x.abs() > 0.10 {
            assert!(thighs < 0.6, "vertex {v} at the far side is mostly on the hips (thighs carry {thighs})");
        }
        if p.y < 0.3 && (p.x - 0.045).abs() < 0.01 {
            assert!(on(v, &left) > 0.7, "vertex {v} in front of the left thigh is on it (got {})", on(v, &left));
            thigh_cap_seen |= (thighs - 0.95).abs() < 1e-4;
        }
    }
    assert!(thigh_cap_seen, "the 95 % cap is reached");
}

#[test]
fn the_plaid_is_painted_at_the_prototypes_resolution_and_at_twice_it() {
    let stack = built("standard", &SkirtParams::default());
    let texture = &stack.output("skirt").textures[0].1;
    assert_eq!((texture.desc.size.x, texture.desc.size.y), (256, 256));
    let fine = SkirtParams { plaid: Some(Tartan { resolution: 512, ..Tartan::default() }), ..SkirtParams::default() };
    let stack = built("standard", &fine);
    let texture = &stack.output("skirt").textures[0].1;
    assert_eq!((texture.desc.size.x, texture.desc.size.y), (512, 512));
}

#[test]
fn the_plaids_pixels_are_the_canvas_composite() {
    // Away from antialiased edges the canvas composite at a pixel is easy to work out: `base`, `band` at 62 % where one
    // band covers it and again where both do, the lines at 95 %, then the twill at 7 % where a diagonal crosses.
    let tartan = Tartan {
        base: ColourRef::rgb(200, 200, 200),
        band: ColourRef::rgb(100, 100, 100),
        line: ColourRef::rgb(0, 0, 0),
        hairline: ColourRef::rgb(255, 255, 255),
        resolution: 256,
    };
    let p = SkirtParams { plaid: Some(tartan), ..SkirtParams::default() };
    let stack = built("standard", &p);
    let texture = &stack.output("skirt").textures[0].1;
    let at = |x: u32, y: u32| -> [u8; 4] {
        let i = ((y * texture.desc.size.x + x) * 4) as usize;
        [texture.rgba8[i], texture.rgba8[i + 1], texture.rgba8[i + 2], texture.rgba8[i + 3]]
    };
    let mix = |below: f32, over: f32, alpha: f32| below * (1.0 - alpha) + over * alpha;
    // The ground, between the bands and the lines (x = y = 5): the twill is on or off the pixel's diagonal.
    let ground = at(5, 5)[0];
    assert!((i32::from(ground) - 200).abs() <= 14, "the ground is the base colour, got {ground}");
    // The crossing of two bands (x = y = 50): 200 -> 100 at 62 % -> 100 at 62 % again = 100 + 100 x 0.38^2.
    let crossing = at(50, 50)[0];
    let expected = mix(mix(200.0, 100.0, 0.62), 100.0, 0.62);
    assert!((f32::from(crossing) - expected).abs() <= 14.0, "two bands cross: {crossing} against {expected}");
    // One band alone (x = 50, y = 5): between the lines and above the other band.
    let single = at(50, 5)[0];
    let expected = mix(200.0, 100.0, 0.62);
    assert!((f32::from(single) - expected).abs() <= 14.0, "one band: {single} against {expected}");
    // The line either side of the band is nearly black on the ground (x = 17, y = 5).
    let line = at(17, 5)[0];
    assert!(line < 60, "the thin line is dark, got {line}");
    // The second tile is the first (the pattern repeats every 128).
    for (x, y) in [(5, 5), (50, 50), (50, 5), (17, 5)] {
        let d = (i32::from(at(x, y)[0]) - i32::from(at(x + 128, y + 128)[0])).abs();
        assert!(d <= 14, "tiles repeat at ({x}, {y}): {d}");
    }
    let _ = Colour::BLACK;
}

#[test]
fn the_cloth_hook_has_no_sheet_yet() {
    let stack = built("standard", &SkirtParams::default());
    let fit = stack.before("skirt").get::<FitFact>().fits.first().unwrap().clone();
    assert!(cloth_sheet(&SkirtParams::default(), fit.as_ref()).is_none());
}

#[test]
fn the_parameters_are_checked() {
    assert!(SkirtParams::default().validate().is_ok());
    let bad = |edit: &dyn Fn(&mut SkirtParams)| {
        let mut p = SkirtParams::default();
        edit(&mut p);
        p.validate().is_err()
    };
    assert!(bad(&|p| p.hem = MAX_HEM + 0.01), "a hem below the knee");
    assert!(bad(&|p| p.hem = f64::NAN));
    assert!(bad(&|p| p.top = 1.5));
    assert!(bad(&|p| p.flare = -0.01));
    assert!(bad(&|p| p.hem_roll = 0.05));
    assert!(bad(&|p| p.thigh_cap = 0.2));
    assert!(bad(&|p| p.thigh_reach = 5.0));
    assert!(bad(&|p| p.hip_blend = [0.5, 0.1]));
    assert!(bad(&|p| p.hip_blend = [f64::NAN, 0.1]));
    assert!(bad(&|p| p.pleats.count = 1));
    assert!(bad(&|p| p.pleats.depth = 0.5));
    assert!(bad(&|p| p.pleats.fold[2][1] = 0.1), "folds only go inwards");
    assert!(bad(&|p| p.pleats.fold.swap(1, 2)), "the fold's points rise");
    assert!(bad(&|p| p.pleats.fold.last_mut().unwrap()[0] = 1.0));
    assert!(bad(&|p| p.plaid = Some(Tartan { resolution: 8, ..Tartan::default() })));
    assert!(bad(&|p| p.profile = Some(SkirtProfile { rx: vec![], rz: vec![[0.0, 0.1]], cz: vec![[0.0, 0.0]] })));
    assert!(bad(&|p| p.profile =
        Some(SkirtProfile { rx: vec![[0.0, 0.1], [0.0, 0.2]], rz: vec![[0.0, 0.1]], cz: vec![[0.0, 0.0]] })));
    assert!(bad(&|p| p.common.cloth.enabled = true));
}

#[test]
fn an_explicit_profile_is_the_skirt() {
    let profile = SkirtProfile {
        rx: vec![[0.0, 0.12], [0.5, 0.10], [1.0, 0.08]],
        rz: vec![[0.0, 0.11], [0.5, 0.09], [1.0, 0.07]],
        cz: vec![[0.0, 0.0], [1.0, 0.002]],
    };
    let p = SkirtParams { profile: Some(profile), ..SkirtParams::default() };
    assert!(p.validate().is_ok());
    let stack = built("soft-sd", &p);
    let g = &stack.output("skirt").meshes[0].geometry;
    // The hem row of the first pleat's crest is on the profile's hem ellipse (rx 0.12 at the side, rz 0.11 at the front).
    let front = g.positions[..145].iter().map(|v| f64::from(v.z)).fold(f64::NEG_INFINITY, f64::max);
    assert!((front - 0.0).abs() < 1.0, "built");
    let top_row_x = g.positions[11 * 145..12 * 145].iter().map(|v| f64::from(v.x)).fold(f64::NEG_INFINITY, f64::max);
    assert!((top_row_x - 0.12).abs() < 1e-3, "the hem row's widest crest is the profile's rx: {top_row_x}");
}

#[test]
fn the_kind_is_registered() {
    let meta = Skirt::meta();
    assert_eq!((meta.id, meta.requires), ("skirt", &["body"][..]));
    assert!((28..=35).contains(&LAYER_SKIRT));
    assert_eq!(<Skirt as Garment>::open_borders(&SkirtParams::default()), 4);
}

/// Renders the skirt over tights on a body, front, side and back, through the mock toon renderer, when
/// `OMUZAKI_SKIRT_PPM=<directory>` is set, as binary PPM files. Taste is judged on the pictures, not on numbers.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_skirt_over_tights_when_asked() {
    use omuzaki_char::outfit::legwear::{LAYER_LEGWEAR, LegwearParams};
    use omuzaki_core::glam::{Mat4, UVec2, Vec3};
    use omuzaki_render_api::{
        AlphaEncoding, Background, Camera, Instance, Light, Post, Quality, Renderer, RendererExt, Scene, Tonemap,
    };
    use omuzaki_render_mock::MockRenderer;
    use std::collections::HashMap;

    let Some(directory) = std::env::var_os("OMUZAKI_SKIRT_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    for template in ["soft-sd", "standard"] {
        let garments = [
            ("legwear", "legwear", LAYER_LEGWEAR, serde_json::to_value(LegwearParams::default()).unwrap()),
            ("skirt", "skirt", LAYER_SKIRT, serde_json::to_value(SkirtParams::default()).unwrap()),
        ];
        let def = conformance::character(template, &omuzaki_char_api::BodyShape::default(), &garments);
        let stack = conformance::build(def, &services()).unwrap();
        let (mut lo, mut hi) = (Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY));
        for (_, output) in &stack.outputs {
            for mesh in &output.meshes {
                for p in &mesh.geometry.positions {
                    lo = lo.min(*p);
                    hi = hi.max(*p);
                }
            }
        }
        // The camera looks at the hips and the legs: the skirt and what is under it.
        let centre = Vec3::new(0.0, lo.y + 0.45 * (hi.y - lo.y), 0.0);
        let distance = 1.15 * (hi.y - lo.y);
        for (view, azimuth_deg) in [("front", 0.0_f32), ("three-quarter", 35.0), ("side", 90.0), ("back", 180.0)] {
            let mut renderer = MockRenderer::new();
            let mut scene = Scene::new(Camera::look_at(
                centre
                    + Mat4::from_rotation_y(azimuth_deg.to_radians()).transform_vector3(Vec3::new(0.0, 0.05, distance)),
                centre,
                Vec3::Y,
                Camera::default().projection,
            ));
            scene.background = Background::Solid { colour: Colour::hex(0x001e_1c24) };
            scene.lights = vec![
                Light::Hemisphere { sky: Colour::hex(0x00f1_eef8), ground: Colour::hex(0x004a_3e36), intensity: 0.75 },
                Light::Directional {
                    to_light: Vec3::new(1.6, 3.0, 2.4),
                    colour: Colour::hex(0x00ff_f6ec),
                    intensity: 1.7,
                    casts_shadow: false,
                },
            ];
            scene.post = Post { exposure: 1.0, tonemap: Tonemap::None, ..Post::default() };
            for (_, output) in &stack.outputs {
                let mut textures = HashMap::new();
                for (name, data) in &output.textures {
                    textures.insert(name.clone(), renderer.create_texture(&data.desc, Some(&data.rgba8)).unwrap());
                }
                for built in &output.meshes {
                    let mesh = renderer.create_mesh(&built.geometry).unwrap();
                    let (_, spec) = output.materials.iter().find(|(name, _)| *name == built.material).unwrap();
                    let mut spec = spec.clone();
                    spec.overlays.clear();
                    let material = spec.to_material(&|name| textures.get(name).copied(), &|_| None).unwrap();
                    let material = renderer.create_material(&material).unwrap();
                    scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
                }
            }
            let quality = Quality { samples: 4, ..Quality::offline() };
            let image = renderer.render_image(UVec2::splat(640), AlphaEncoding::Straight, &scene, &quality).unwrap();
            let mut ppm = format!("P6\n{} {}\n255\n", image.size.x, image.size.y).into_bytes();
            for y in 0..image.size.y {
                for x in 0..image.size.x {
                    let [r, g, b, _] = image.pixel(x, y).unwrap();
                    ppm.extend([r, g, b]);
                }
            }
            std::fs::write(directory.join(format!("{template}-{view}.ppm")), ppm).unwrap();
        }
    }
}
