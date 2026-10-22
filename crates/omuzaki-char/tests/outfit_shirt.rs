//! The `shirt` garment (#190) through the garment conformance suite on every template and shape, its parameters, the
//! meshes and materials it makes, the anchor it publishes and how it follows the fit it is given.
//!
//! Set `OMUZAKI_SHIRT_PPM=<directory>` to also render the shirt on every template from four sides through the mock toon
//! renderer as binary PPM files; taste is judged on the pictures.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;
#[path = "support/outfit_render.rs"]
mod render;

use std::collections::BTreeSet;

use omuzaki_char::outfit::common::{FabricStyle, Garment, GarmentCommon, LAYER_SHIRT, StripeDirection};
use omuzaki_char::outfit::shirt::{
    COLLAR_RING, CuffParams, NeckParams, SHIRT_EXPONENT, Shirt, ShirtParams, SleeveParams, sleeve, torso,
};
use omuzaki_char_api::{
    AnchorId, AnchorsFact, BodyRegion, BuildServices, JointRef, KindProvenance, PartCategory, PartId, find_kind,
};
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{ColourRef, Validate};
use omuzaki_face_mock::MockRasterizer;

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

fn part() -> PartId {
    PartId::new(conformance::PART).unwrap()
}

/// The shirt built on `template` with `shape`.
fn stack(template: &str, params: &ShirtParams) -> conformance::Stack {
    let shape = conformance::shapes()[0].1.clone();
    let def = conformance::character(
        template,
        &shape,
        &[(conformance::PART, "shirt", LAYER_SHIRT, serde_json::to_value(params).unwrap())],
    );
    conformance::build(def, &services()).unwrap()
}

/// The vertices of the mesh called `name` in the shirt's output.
fn vertices(stack: &conformance::Stack, name: &str) -> Vec<DVec3> {
    let output = stack.output(conformance::PART);
    let mesh = output
        .meshes
        .iter()
        .find(|m| m.name == format!("{}-{name}", conformance::PART))
        .unwrap_or_else(|| panic!("no mesh `{name}`"));
    mesh.geometry.positions.iter().map(|p| p.as_dvec3()).collect()
}

// ------------------------------------------------------------------------------------------------------ the kind

#[test]
fn the_kind_is_a_project_owned_garment_that_needs_a_body() {
    let reg = find_kind("shirt").expect("registered");
    let meta = (reg.meta)();
    assert_eq!((meta.category, meta.requires), (PartCategory::Garment, &["body"][..]));
    assert_eq!((reg.provenance)(), KindProvenance::ProjectOwned);
    assert_eq!((reg.defaults)(), serde_json::to_value(ShirtParams::default()).unwrap());
}

#[test]
#[allow(clippy::type_complexity)] // a table of field names and edits
fn the_defaults_are_valid_and_every_bad_number_is_refused_naming_its_field() {
    assert!(ShirtParams::default().validate().is_ok());
    let bad: [(&str, Box<dyn Fn(&mut ShirtParams)>); 12] = [
        ("length", Box::new(|p| p.length = 0.9)),
        ("length", Box::new(|p| p.length = f64::NAN)),
        ("tuck", Box::new(|p| p.tuck = 0.5)),
        ("sleeve/length", Box::new(|p| p.sleeve.length = 0.1)),
        ("sleeve/width", Box::new(|p| p.sleeve.width = -0.001)),
        ("sleeve/ease_at_elbow", Box::new(|p| p.sleeve.ease_at_elbow = f64::INFINITY)),
        ("cuff/height", Box::new(|p| p.cuff.height = 0.5)),
        ("cuff/height", Box::new(|p| p.cuff.height = 0.0)),
        ("neck/rise", Box::new(|p| p.neck.rise = 0.0)),
        ("neck/rise", Box::new(|p| p.neck.rise = f64::NAN)),
        ("common/ease", Box::new(|p| p.common.ease = 0.5)),
        ("common/cloth/enabled", Box::new(|p| p.common.cloth.enabled = true)),
    ];
    for (field, change) in bad {
        let mut p = ShirtParams::default();
        change(&mut p);
        let e = p.validate().expect_err(field);
        assert!(e.0.iter().any(|e| e.path == field), "{field}: {e}");
    }
}

// ----------------------------------------------------------------------------------------------- the suite

#[test]
fn a_shirt_passes_the_suite_on_every_template_and_shape() {
    let rows = conformance::assert_conforms::<Shirt>(&ShirtParams::default(), &services());
    assert_eq!(rows.len(), 12, "4 templates x 3 shapes");
    for row in &rows {
        assert!(row.min_clearance_mm >= 0.5 * row.ease_mm, "{row:?}: at least half the ease off the body");
        assert!(row.triangles <= TRIANGLE_BUDGET, "{row:?}: over the budget of {TRIANGLE_BUDGET}");
    }
}

/// The triangles a shirt may have (body, sleeves, cuffs, buttons, seams and holes): the prototype's shirt has 11 200
/// without its collar, and a template with longer arms has more stations.
const TRIANGLE_BUDGET: usize = 14_000;

#[test]
fn a_short_sleeved_shirt_with_a_loose_cut_passes_too() {
    let params = ShirtParams {
        common: GarmentCommon { ease: 0.008, ..ShirtParams::default().common },
        length: 0.15,
        sleeve: SleeveParams { length: 0.5, width: 0.01, ease_at_elbow: 0.004 },
        neck: NeckParams { rise: 0.03 },
        cuff: CuffParams { height: 0.03, button: false },
        ..ShirtParams::default()
    };
    assert!(!params.has_cuffs());
    conformance::assert_conforms::<Shirt>(&params, &services());
}

#[test]
fn a_patterned_shirt_passes_and_its_cloth_carries_the_pattern() {
    let params = ShirtParams {
        common: GarmentCommon {
            fabric: FabricStyle::Stripes {
                ground: ColourRef::rgb(250, 250, 250),
                stripe: ColourRef::rgb(120, 150, 200),
                period: 0.01,
                width: 0.003,
                direction: StripeDirection::Vertical,
            },
            ..ShirtParams::default().common
        },
        ..ShirtParams::default()
    };
    conformance::assert_conforms::<Shirt>(&params, &services());
    let stack = stack("soft-sd", &params);
    let output = stack.output(conformance::PART);
    assert_eq!(output.textures.len(), 1, "the cloth's stripes");
    let body = output.meshes.iter().find(|m| m.name.ends_with("-shirt")).unwrap();
    let v = body.geometry.uvs.iter().map(|uv| uv.y).fold(0.0_f32, f32::max);
    assert!(v > 5.0, "the texture counts tiles up the shirt: {v}");
}

#[test]
fn the_documented_open_borders_follow_the_parameters() {
    let default = ShirtParams::default();
    assert_eq!(
        Shirt::open_borders(&default),
        12,
        "hem, neck, two armholes; per side the sleeve's two ends and the cuff's two edges"
    );
    let short = ShirtParams { sleeve: SleeveParams { length: 0.6, ..default.sleeve.clone() }, ..default.clone() };
    assert_eq!(Shirt::open_borders(&short), 8, "no cuffs on a short sleeve");
    let tucked = ShirtParams { tuck: 0.1, ..default };
    assert_eq!(Shirt::open_borders(&tucked), 13, "a tucked hem's lining");
}

// ------------------------------------------------------------------------------------------------ what it builds

#[test]
fn it_builds_the_prototypes_grids_and_names_everything_after_the_part() {
    let stack = stack("soft-sd", &ShirtParams::default());
    let output = stack.output(conformance::PART);
    let names: Vec<_> = output.meshes.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["garment-shirt", "garment-lines", "garment-holes"], "no lining without a tuck");
    let materials: BTreeSet<_> = output.materials.iter().map(|(m, _)| m.as_str().to_owned()).collect();
    assert!(
        ["garment-fabric", "garment-lines", "garment-holes"].iter().all(|m| materials.contains(*m)),
        "{materials:?}"
    );
    let shirt = &output.meshes[0].geometry;
    // The torso is 37 rows of 61, a sleeve 30 rows of 25 and a cuff band 16 rows of 41, less what the shoulders' weld
    // trims; the two sides are mirror images.
    let key = |p: omuzaki_core::glam::Vec3| [p.x, p.y, p.z].map(|c| (c * 1e5).round() as i64);
    let all: BTreeSet<[i64; 3]> = shirt.positions.iter().map(|p| key(*p)).collect();
    let mirror = |p: omuzaki_core::glam::Vec3| omuzaki_core::glam::Vec3::new(-p.x, p.y, p.z);
    // The weld at the shoulders slides a vertex towards its nearest clear neighbour, and of two at the same distance it
    // takes the first: a few vertices there slide along the mirror image of their twin's edge, not onto its point.
    let unmatched: Vec<_> = shirt.positions.iter().filter(|p| !all.contains(&key(mirror(**p)))).collect();
    assert!(unmatched.len() * 100 < shirt.positions.len(), "the two sides are mirror images: {unmatched:?}");
    for p in &unmatched {
        let twin = shirt.positions.iter().map(|q| q.distance(mirror(**p))).fold(f32::INFINITY, f32::min);
        assert!(twin < 0.01 && p.y > 0.4, "{p} is the weld's at a shoulder, by its twin {twin} m away");
    }
    assert!(shirt.positions.len() > 37 * 61, "the torso's grid, and the sleeves and cuffs on it");
    assert!(!output.meshes[2].geometry.positions.is_empty(), "the cuffs have their pearls");
    assert_eq!(output.meshes[1].geometry.positions.len(), 2 * 41 * 7, "a fold seam ring on each cuff");
    assert!(!output.meshes[1].casts_shadow && !output.meshes[2].casts_shadow, "ink casts no shadow");
    assert_eq!(output.meshes[2].geometry.positions.len() % 8, 0, "four holes in each pearl");
}

#[test]
fn the_cloth_has_the_prototypes_look() {
    let stack = stack("soft-sd", &ShirtParams::default());
    let output = stack.output(conformance::PART);
    let (_, cloth) = output.materials.iter().find(|(m, _)| m.as_str() == "garment-fabric").unwrap();
    let omuzaki_render_api::Shading::Toon(toon) = &cloth.shading else { panic!("toon") };
    assert_eq!((toon.shift, toon.softness, toon.rim.strength), (-0.12, 0.2, 0.14));
    assert!(cloth.vertex_colours);
    assert_eq!(cloth.outline.map(|o| o.width), Some(0.0015));
    let (_, ink) = output.materials.iter().find(|(m, _)| m.as_str() == "garment-lines").unwrap();
    assert_eq!(ink.shading, omuzaki_render_api::Shading::Unlit);
}

#[test]
fn a_tuck_pulls_the_hips_in_and_adds_a_lining_that_closes_the_bottom() {
    let plain = stack("soft-sd", &ShirtParams::default());
    let tucked = stack("soft-sd", &ShirtParams { tuck: 0.1, ..ShirtParams::default() });
    let lining = vertices(&tucked, "lining");
    assert_eq!(lining.len(), 10 * 61, "three closing rings and seven rows");
    let bottom = lining.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
    let first_ring = &lining[..61];
    assert!(first_ring.iter().all(|p| p.distance(first_ring[0]) < 1e-9), "the first ring is a point on the axis");
    assert!((bottom - 0.325).abs() < 1e-4, "the lining starts at the torso's bottom: {bottom}");
    // The tuck pulls the shirt's hem in: 10 % of the radius at the hips end, none at the waist.
    let width = |stack: &conformance::Stack| {
        let v = vertices(stack, "shirt");
        v[..37 * 61].iter().map(|p| p.x.abs()).fold(0.0, f64::max)
    };
    assert!(width(&tucked) <= width(&plain), "the widest point is higher up and unchanged");
    let hem = |stack: &conformance::Stack| vertices(stack, "shirt")[..61].iter().map(|p| p.x.abs()).fold(0.0, f64::max);
    assert!((hem(&tucked) - hem(&plain)).abs() < 1e-4, "the hem is above the tuck's start: it is not pulled in");
    assert_eq!(vertices(&plain, "shirt").len(), vertices(&tucked, "shirt").len());
}

#[test]
fn it_publishes_the_collar_ring_at_the_centre_of_the_neck_opening() {
    for template in conformance::TEMPLATES {
        let stack = stack(template, &ShirtParams::default());
        let anchors = stack.published.get::<AnchorsFact>();
        let id: AnchorId = COLLAR_RING.parse().unwrap();
        let anchor = anchors.get(&(part(), id)).expect("the shirt publishes the collar ring");
        assert_eq!(anchor.bone, JointRef::NECK);
        let shirt = vertices(&stack, "shirt");
        let highest = shirt.iter().filter(|p| p.x.abs() < 0.03).map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (highest - f64::from(anchor.position.y)).abs() < 1e-3,
            "{template}: the ring is at the opening's height"
        );
        assert!(anchor.position.x.abs() < 1e-5, "{template}: and on the middle");
        let neck_top = stack.published.get::<omuzaki_char_api::FitFact>().fits[0]
            .section(BodyRegion::Torso, 1.0)
            .unwrap()
            .centre
            .y;
        assert!(
            (f64::from(anchor.position.y) - f64::from(neck_top) - 0.018).abs() < 1e-3,
            "{template}: the opening stands 18 mm over the torso's top"
        );
    }
}

#[test]
fn the_envelope_covers_the_torso_and_arms_with_the_shirts_ease() {
    let stack = stack("soft-sd", &ShirtParams::default());
    let fits = &stack.published.get::<omuzaki_char_api::FitFact>().fits;
    assert_eq!(fits.len(), 2, "the body, then the shirt");
    let scale = omuzaki_char::outfit::common::enclosing_scale(SHIRT_EXPONENT) as f32;
    for region in [BodyRegion::Torso, BodyRegion::Neck, BodyRegion::ArmL, BodyRegion::ArmR] {
        let (a, b) = (fits[0].section(region, 0.5).unwrap(), fits[1].section(region, 0.5).unwrap());
        assert!(((a.radii + omuzaki_core::glam::Vec2::splat(0.004)) * scale - b.radii).abs().max_element() < 1e-6);
    }
    assert_eq!(fits[0].section(BodyRegion::LegL, 0.5), fits[1].section(BodyRegion::LegL, 0.5), "legs pass through");
}

// --------------------------------------------------------------------------------------------- following the fit

#[test]
fn the_sleeve_follows_the_arm_and_widens_with_its_parameters() {
    let stack = stack("soft-sd", &ShirtParams::default());
    let fit = stack.published.get::<omuzaki_char_api::FitFact>().fits[0].clone();
    // The widest ring's radius round its centre, at station `row`.
    let ring_radius = |spec: &sleeve::SleeveSpec, row: usize| -> f64 {
        let built = sleeve::build(fit.as_ref(), spec, None).unwrap();
        let v: Vec<DVec3> = built.mesh.mesh().positions.iter().map(|p| p.as_dvec3()).collect();
        let ring = &v[row * 25..(row + 1) * 25];
        let centre = ring.iter().copied().sum::<DVec3>() / 25.0;
        ring.iter().map(|p| p.distance(centre)).fold(0.0, f64::max)
    };
    let base = sleeve::SleeveSpec { ease: 0.004, width: 0.0035, ease_at_elbow: 0.0, length: 1.0 };
    let elbow_row = 19; // 8 dome stations, then one every 10.5 mm from 16 mm: the 12th regular station is at the elbow, 12 cm down the arm
    let wide = ring_radius(&sleeve::SleeveSpec { width: 0.0135, ..base }, elbow_row);
    let loose = ring_radius(&sleeve::SleeveSpec { ease_at_elbow: 0.01, ..base }, elbow_row);
    let plain = ring_radius(&base, elbow_row);
    assert!((wide - plain - 0.01).abs() < 1e-3, "10 mm more width: {plain} to {wide}");
    assert!(
        (loose - plain - 0.01).abs() < 2e-3,
        "extra ease at the elbow reaches the ring at the elbow: {plain} to {loose}"
    );
    let shoulder = ring_radius(&base, 7);
    assert!(shoulder > ring_radius(&base, 0) + 0.02, "the dome closes the sleeve behind the arm's start");
}

#[test]
fn the_sleeve_has_the_prototypes_thirty_stations() {
    let s = sleeve::stations(0.2248 + sleeve::WRIST_OVERLAP + sleeve::SHOULDER_SPAN);
    assert_eq!(s.len(), 30, "eight easing out of the shoulder, twenty-one regular, the end");
    assert!(s.windows(2).all(|w| w[1] > w[0]), "rising");
    assert!((s[7] - sleeve::SHOULDER_SPAN).abs() < 1e-12 && (s[8] - sleeve::SHOULDER_SPAN - 0.016).abs() < 1e-12);
    assert!((s[9] - s[8] - 0.0105).abs() < 1e-12, "the regular stations are 10.5 mm apart");
}

#[test]
fn a_shirt_takes_the_torso_it_is_given_in_fractions_of_its_length() {
    let heights = |template: &str| {
        let stack = stack(template, &ShirtParams::default());
        let v = vertices(&stack, "shirt");
        let torso_body = &stack.published.get::<omuzaki_char_api::FitFact>().fits[0];
        let (bottom, top) = (
            f64::from(torso_body.section(BodyRegion::Torso, 0.0).unwrap().centre.y),
            f64::from(torso_body.section(BodyRegion::Torso, 1.0).unwrap().centre.y),
        );
        let front: Vec<&DVec3> = v.iter().filter(|p| p.x.abs() < 0.03 && p.z > 0.0).collect();
        let lowest = front.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
        let highest = front.iter().map(|p| p.y).fold(f64::NEG_INFINITY, f64::max);
        ((lowest - bottom) / (top - bottom), highest - top)
    };
    for template in conformance::TEMPLATES {
        let (hem, rise) = heights(template);
        assert!(
            (hem - torso::DEFAULT_HEM).abs() < 1e-3,
            "{template}: the hem is the same fraction of the torso: {hem}"
        );
        assert!((rise - 0.018).abs() < 1e-3, "{template}: the neck opening is 18 mm over the torso: {rise}");
    }
}

#[test]
fn a_cuff_without_a_button_has_no_holes_and_a_taller_cuff_reaches_further_past_the_wrist() {
    let plain = stack(
        "soft-sd",
        &ShirtParams { cuff: CuffParams { button: false, ..CuffParams::default() }, ..ShirtParams::default() },
    );
    assert!(
        plain.output(conformance::PART).meshes.iter().all(|m| !m.name.ends_with("-holes")),
        "no holes without a pearl"
    );
    let reach = |height: f64| {
        let stack = stack(
            "soft-sd",
            &ShirtParams { cuff: CuffParams { height, ..CuffParams::default() }, ..ShirtParams::default() },
        );
        let v = vertices(&stack, "shirt");
        let (elbow, wrist) = (DVec3::new(0.155, 0.38, -0.01), DVec3::new(0.2, 0.29, 0.01));
        let forearm = (wrist - elbow).normalize();
        // How far along the forearm, past the wrist, the cuff's cloth reaches.
        v.iter()
            .filter(|p| (**p - wrist).cross(forearm).length() < 0.035 && (**p - wrist).dot(forearm) > -0.02)
            .map(|p| (*p - wrist).dot(forearm))
            .fold(f64::NEG_INFINITY, f64::max)
    };
    assert!(reach(0.03) > reach(0.015) + 0.01, "{} vs {}", reach(0.03), reach(0.015));
    // Past where the hand widens out of it, a cuff stops short.
    assert!(reach(0.05) < reach(0.03) + 0.005, "{} vs {}", reach(0.05), reach(0.03));
}

// ------------------------------------------------------------------------------------------------- pictures

/// Renders the shirt on every template from four sides when `OMUZAKI_SHIRT_PPM` names a folder.
#[test]
fn render_the_shirt_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_SHIRT_PPM") else { return };
    for template in conformance::TEMPLATES {
        let stack = stack(template, &ShirtParams::default());
        render::render(&stack, render::Framing::parts(&stack, &["garment"]), template, directory.as_ref());
    }
}
