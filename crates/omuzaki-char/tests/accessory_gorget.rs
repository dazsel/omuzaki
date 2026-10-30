//! The gorget (#2450): the `filigree-collar` with a height and a chest piece, on every body template and shape. Its plate
//! rises up the neck and lies on the collarbones without passing through the body's own mesh, its chest piece follows
//! the chest and its neck plate the neck, its centre stone takes each setting, and the library's preset builds.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;

use omuzaki_char::accessory::rigid::jewellery::{CollarParams, Setting};
use omuzaki_char_api::{BodyRegion, BodyShape, BuildServices, FitFact, JointRef, PartOutput};
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::Validate;
use omuzaki_face_mock::MockRasterizer;
use serde_json::{Value, json};

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

/// The gorget the library's preset adds.
fn preset() -> Value {
    let text = include_str!("../library/items/accessories-gorget.toml");
    let item: toml::Value = toml::from_str(text).unwrap();
    let params = &item["step"][0]["args"]["params"];
    serde_json::to_value(params).unwrap()
}

/// The body of `template` in `shape` with the `filigree-collar` of `params` on it (part [`conformance::PART`]).
fn built(template: &str, shape: &BodyShape, params: Value) -> conformance::Stack {
    let def = conformance::character(template, shape, &[(conformance::PART, "filigree-collar", 50, params)]);
    conformance::build(def, &services()).unwrap()
}

fn positions(out: &PartOutput, mesh: &str) -> Vec<DVec3> {
    out.meshes
        .iter()
        .filter(|m| m.name.ends_with(mesh))
        .flat_map(|m| m.geometry.positions.iter().map(|p| p.as_dvec3()))
        .collect()
}

#[test]
fn the_preset_is_valid_and_makes_a_gorget() {
    let p: CollarParams = serde_json::from_value(preset()).unwrap();
    assert!(p.validate().is_ok());
    assert!(p.gorget());
    assert!(!CollarParams::default().gorget(), "the plain collar stays a band");
}

#[test]
fn it_never_passes_through_the_body_on_any_template_or_shape() {
    for template in conformance::TEMPLATES {
        for (label, shape) in conformance::shapes() {
            let stack = built(template, &shape, preset());
            let out = stack.output(conformance::PART);
            for mesh in &out.meshes {
                mesh.validate().unwrap_or_else(|e| panic!("{template} {label}: {}: {e}", mesh.name));
            }
            conformance::check_body_mesh(out, stack.output(conformance::BODY))
                .unwrap_or_else(|e| panic!("{template} {label}: {e}"));
        }
    }
}

#[test]
fn it_rises_up_the_neck_and_lies_on_the_chest() {
    for template in ["standard", "tall"] {
        let stack = built(template, &BodyShape::default(), preset());
        let out = stack.output(conformance::PART);
        let fit = stack.before(conformance::PART).get::<FitFact>().fits[0].clone();
        let section = |region, t: f32| fit.section(region, t).unwrap();
        let neck = (section(BodyRegion::Neck, 0.0), section(BodyRegion::Neck, 1.0));
        let neck_length = f64::from(neck.1.centre.y - neck.0.centre.y);
        let torso_length =
            f64::from(section(BodyRegion::Torso, 1.0).centre.y - section(BodyRegion::Torso, 0.0).centre.y);
        let band = f64::from(neck.0.centre.y) + 0.08 * neck_length;
        let plate = positions(out, "-primary");
        let top = plate.iter().map(|p| p.y).fold(f64::MIN, f64::max);
        assert!(top - band > 0.4 * neck_length, "{template}: it rises {:.1} mm up the neck", (top - band) * 1e3);
        // Down the front: below the band by most of the chest piece, and close over the body there.
        let front: Vec<DVec3> = plate.iter().copied().filter(|p| p.x.abs() < 0.01 && p.z > 0.0).collect();
        let lowest = front.iter().map(|p| p.y).fold(f64::MAX, f64::min);
        assert!(
            band - lowest > 0.06 * torso_length,
            "{template}: it runs {:.1} mm down the chest",
            (band - lowest) * 1e3
        );
        let body: Vec<[DVec3; 3]> = stack
            .output(conformance::BODY)
            .meshes
            .iter()
            .flat_map(|m| {
                let g = &m.geometry;
                g.indices
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|t| t.map(|k| g.positions[k as usize].as_dvec3()))
                    .collect::<Vec<_>>()
            })
            .collect();
        let gap = |p: DVec3| {
            body.iter()
                .map(|&[a, b, c]| (p - conformance::closest_on_triangle(p, a, b, c)).length())
                .fold(f64::INFINITY, f64::min)
        };
        // The nearest of the plate's points to the body, at each height down the front of the chest piece.
        let mut worst: f64 = 0.0;
        for k in 1..=6 {
            let y = band - (band - lowest) * f64::from(k) / 7.0;
            let near = front.iter().filter(|p| (p.y - y).abs() < 0.0015).map(|&p| gap(p)).fold(f64::INFINITY, f64::min);
            worst = worst.max(near);
        }
        assert!(worst < 0.006, "{template}: the chest piece stands {:.1} mm off the chest", worst * 1e3);
    }
}

#[test]
fn the_neck_plate_follows_the_neck_and_the_chest_piece_the_chest() {
    let stack = built("tall", &BodyShape::default(), preset());
    let out = stack.output(conformance::PART);
    let mesh = out.meshes.iter().find(|m| m.name.ends_with("-primary")).unwrap();
    let g = &mesh.geometry;
    let weight_of = |joint: &JointRef, v: usize| -> f32 {
        let Some(slot) = mesh.joint_bones.iter().position(|j| j == joint) else { return 0.0 };
        (0..4).filter(|&k| usize::from(g.joints[v][k]) == slot).map(|k| g.weights[v][k]).sum()
    };
    let by_height = |pick: fn(f32, f32) -> bool| {
        (0..g.positions.len()).reduce(|a, b| if pick(g.positions[b].y, g.positions[a].y) { b } else { a }).unwrap()
    };
    let (top, bottom) = (by_height(|a, b| a > b), by_height(|a, b| a < b));
    assert!(weight_of(&JointRef::NECK, top) > 0.99, "the top of the neck plate turns with the neck");
    assert!(weight_of(&JointRef::CHEST, bottom) > 0.99, "the foot of the chest piece stays on the chest");
}

#[test]
fn the_centre_stone_takes_every_setting() {
    let mut counts = Vec::new();
    for setting in [Setting::Bezel, Setting::Claws, Setting::Halo] {
        let mut params = preset();
        params["centre"]["setting"] = serde_json::to_value(setting).unwrap();
        let stack = built("standard", &BodyShape::default(), params);
        let out = stack.output(conformance::PART);
        counts.push(positions(out, "-primary").len());
    }
    assert!(counts[2] > counts[0], "a halo adds its beads to the bezel: {counts:?}");
    assert_ne!(counts[1], counts[0], "claws are not a bezel: {counts:?}");
    // On the plain collar the centre stone takes the middle motif's foot.
    let plain = |size: f64| {
        let stack = built("standard", &BodyShape::default(), json!({"centre": {"size": size}}));
        let out = stack.output(conformance::PART);
        positions(out, "-primary").len()
    };
    assert!(plain(0.005) > plain(0.0), "the plain collar sets its centre stone in a bezel");
}

#[test]
fn it_builds_the_same_twice() {
    let a = built("standard", &BodyShape::default(), preset());
    let b = built("standard", &BodyShape::default(), preset());
    let bytes = |stack: &conformance::Stack| {
        stack.output(conformance::PART).meshes.iter().map(|m| format!("{:?}", m.geometry)).collect::<Vec<_>>()
    };
    assert_eq!(bytes(&a), bytes(&b));
}

#[test]
fn a_pointed_edge_hangs_points_down_the_chest_and_stays_off_the_body() {
    let mut pointed = preset();
    pointed["points"] = json!(7);
    pointed["point_depth"] = json!(0.6);
    for template in ["standard", "tall"] {
        for (label, shape) in conformance::shapes() {
            let stack = built(template, &shape, pointed.clone());
            let out = stack.output(conformance::PART);
            for mesh in &out.meshes {
                mesh.validate().unwrap_or_else(|e| panic!("{template} {label}: {}: {e}", mesh.name));
            }
            conformance::check_body_mesh(out, stack.output(conformance::BODY))
                .unwrap_or_else(|e| panic!("{template} {label}: {e}"));
        }
    }
    // Down the front the plate reaches as far as the smooth one (a point hangs there); a little to the side, between
    // the middle point and the next, its edge rises.
    let lowest_near = |params: Value, x: f64| {
        let stack = built("standard", &BodyShape::default(), params);
        positions(stack.output(conformance::PART), "-primary")
            .into_iter()
            .filter(|p| (p.x - x).abs() < 0.002 && p.z > 0.0)
            .map(|p| p.y)
            .fold(f64::MAX, f64::min)
    };
    let (smooth, points) = (lowest_near(preset(), 0.0), lowest_near(pointed.clone(), 0.0));
    assert!((points - smooth).abs() < 0.002, "a point at the front: {:.1} vs {:.1} mm", points * 1e3, smooth * 1e3);
    let side =
        (0..40).map(|k| f64::from(k) * 0.001).map(|x| lowest_near(pointed.clone(), x) - lowest_near(preset(), x));
    let rise = side.fold(0.0_f64, f64::max);
    assert!(rise > 0.004, "between two points the edge rises {:.1} mm", rise * 1e3);
}

#[test]
fn the_parameters_are_checked() {
    for bad in [
        json!({"height": 0.9}),
        json!({"along": 0.3, "height": 0.7}),
        json!({"chest": 0.4}),
        json!({"plate": 0.01}),
        json!({"scroll_scale": 5.0}),
        json!({"scroll_depth": 0.01}),
        json!({"centre": {"size": 0.05}}),
        json!({"points": 16}),
        json!({"point_depth": 0.95}),
    ] {
        let p: CollarParams = serde_json::from_value(bad.clone()).unwrap();
        assert!(p.validate().is_err(), "{bad} is refused");
    }
}
