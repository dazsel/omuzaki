//! The `boots` garment (#326, #327): the garment conformance suite on 4 templates x 3 body shapes for several
//! parameter sets, the boot over a synthetic legwear layer, the explicit-profile boot, the parameters' checks, the stitch
//! rails and the triangle budget. With `OMUZAKI_BOOTS_PPM=<directory>` set, the boots are rendered on every template from
//! four sides as binary PPM files for a look.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic on small counts
#![allow(clippy::many_single_char_names, clippy::chunks_exact_to_as_chunks)] // geometry reads best as a, b, c

#[path = "../../omuzaki-test-char/tests/support/outfit_conformance.rs"]
mod conformance;
#[path = "support/outfit_render.rs"]
mod render;

use omuzaki_char::outfit::boots::{BootHeight, Boots, BootsParams, LAYER_FOOTWEAR, LIVE_DETAIL_TRIANGLES, MAX_STRAPS};
use omuzaki_char::outfit::common::{Garment, GarmentCommon, LAYER_SHIRT, publish_envelope};
use omuzaki_char::outfit::loft::Surface;
use omuzaki_char_api::{
    BodyRegion, BodyShape, BuildCtx, BuildError, BuildOptions, BuildServices, PartCategory, PartKind, PartMeta,
    PartOutput, Quality,
};
use omuzaki_core::glam::DVec3;
use omuzaki_face_api::{Check, Validate};
use serde_json::json;

fn services() -> BuildServices {
    BuildServices::default()
}

fn params(edit: impl FnOnce(&mut BootsParams)) -> BootsParams {
    let mut p = BootsParams::default();
    edit(&mut p);
    p
}

// ---------------------------------------------------------------------------------------- a synthetic legwear layer

/// Legwear reduced to what the boots see of it: an envelope over the legs and feet, `common.ease` thick.
struct Hose;

impl PartKind for Hose {
    type Params = HoseParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-hose",
            summary: "a synthetic legwear layer",
            category: PartCategory::Garment,
            requires: &["body"],
        }
    }

    fn build(p: &HoseParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let mut out = PartOutput::new();
        out.publishes = publish_envelope(ctx, &Self::regions(p), p.common.ease, 2.0)?;
        Ok(out)
    }
}

impl Garment for Hose {
    fn common(p: &HoseParams) -> &GarmentCommon {
        &p.common
    }

    fn regions(_: &HoseParams) -> Vec<BodyRegion> {
        vec![BodyRegion::LegL, BodyRegion::LegR, BodyRegion::FootL, BodyRegion::FootR]
    }

    fn open_borders(_: &HoseParams) -> usize {
        0
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, default)]
struct HoseParams {
    common: GarmentCommon,
}

impl Validate for HoseParams {
    fn check(&self, c: &mut Check) {
        c.nested("common", &self.common);
    }
}

omuzaki_char_api::register_part_kind!(Hose);

// ----------------------------------------------------------------------------------------------------- the suite

#[test]
fn boots_pass_the_suite_on_every_template_and_shape() {
    let rows = conformance::assert_conforms::<Boots>(&BootsParams::default(), &services());
    assert_eq!(rows.len(), 12, "4 templates x 3 shapes");
}

#[test]
fn knee_boots_with_three_straps_no_toe_cap_and_no_tab_pass_too() {
    let p = params(|p| {
        p.height = BootHeight::Knee;
        p.straps.count = MAX_STRAPS;
        p.toe.cap = false;
        p.pull_tab = false;
        p.toe.roundness = 1.0;
    });
    conformance::assert_conforms::<Boots>(&p, &services());
}

#[test]
fn ankle_boots_without_straps_and_with_a_thick_sole_pass_too() {
    let p = params(|p| {
        p.height = BootHeight::Ankle;
        p.straps.count = 0;
        p.sole.thickness = 0.02;
        p.heel.height = 0.035;
        p.common.ease = 0.003;
    });
    conformance::assert_conforms::<Boots>(&p, &services());
}

// ------------------------------------------------------------------------------------------------ over legwear

#[test]
fn boots_clear_a_legwear_layer_and_the_body_by_a_millimetre_on_every_template_and_shape() {
    for template in conformance::TEMPLATES {
        for (label, shape) in conformance::shapes() {
            let case = format!("boots over legwear on {template}/{label}");
            let def = conformance::character(
                template,
                &shape,
                &[
                    ("hose", "test-hose", LAYER_SHIRT, json!({ "common": { "ease": 0.006 } })),
                    ("boots", "boots", LAYER_FOOTWEAR, serde_json::to_value(BootsParams::default()).unwrap()),
                ],
            );
            let stack = conformance::build(def, &services()).unwrap_or_else(|e| panic!("{case}: {e}"));
            let under = stack.before("boots").outer_fit().expect("the legwear's envelope").clone();
            let body = stack.before("hose").outer_fit().expect("the body").clone();
            let regions = [BodyRegion::LegL, BodyRegion::FootL];
            for (name, fit) in [("the legwear", under), ("the body", body)] {
                for region in regions {
                    let surface = Surface::new(fit.clone(), region, 2.0, 0.0).unwrap();
                    for mesh in &stack.output("boots").meshes {
                        for (k, v) in mesh.geometry.positions.iter().enumerate().step_by(5) {
                            let Some(at) = surface.project(v.as_dvec3()).unwrap() else { continue };
                            // The right boot is the left's mirror image and the left regions don't reach it.
                            assert!(
                                at.outside >= 0.001 || v.x < 0.0,
                                "{case}: vertex {k} of `{}` at {v} is {:.2} mm from {name} on {region:?}",
                                mesh.name,
                                at.outside * 1e3
                            );
                        }
                    }
                }
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------- the details

fn built(template: &str, p: &BootsParams, quality: Quality) -> PartOutput {
    let def = conformance::character(
        template,
        &BodyShape::default(),
        &[("boots", "boots", LAYER_FOOTWEAR, serde_json::to_value(p).unwrap())],
    );
    let options = BuildOptions { quality, ..BuildOptions::default() };
    let mut published = omuzaki_char_api::Published::default();
    let mut last = None;
    for id in omuzaki_char_api::plan(&def).unwrap() {
        let out = omuzaki_char_api::build_part(&def, &id, &options, &services(), &published).unwrap();
        published.absorb(&id, out.publishes.clone()).unwrap();
        last = Some(out);
    }
    last.expect("the boots are the last part")
}

fn triangles(out: &PartOutput, name: &str) -> usize {
    out.meshes.iter().filter(|m| m.name.ends_with(name)).map(|m| m.geometry.indices.len() / 3).sum()
}

#[test]
fn the_details_of_one_boot_fit_the_live_budget_and_keep_every_feature() {
    for template in conformance::TEMPLATES {
        let full = built(template, &BootsParams::default(), Quality::Live);
        let plain = built(
            template,
            &params(|p| {
                p.straps.count = 0;
                p.pull_tab = false;
            }),
            Quality::Live,
        );
        // Everything a boot has besides the upper and the cuff (the plain build's leather): straps, buckles, tails,
        // tab, stitching, seams. Both boots are in the output.
        let details = (triangles(&full, "brass")
            + triangles(&full, "holes")
            + triangles(&full, "stitches")
            + triangles(&full, "seams")
            + triangles(&full, "leather")
            - triangles(&plain, "leather"))
            / 2;
        assert!(details <= LIVE_DETAIL_TRIANGLES, "{template}: a boot's details are {details} triangles at Live");
        assert!(
            details > LIVE_DETAIL_TRIANGLES / 2,
            "{template}: the Live details lost features ({details} triangles)"
        );
    }
}

#[test]
fn offline_keeps_the_prototypes_tessellation() {
    let out = built("soft-sd", &BootsParams::default(), Quality::Offline);
    // Per boot, as the prototype's summary reports them: the sole and heel 1,848 each, brass 2,336 / 2.
    assert_eq!(triangles(&out, "sole"), 2 * (1848 + 1848));
    // The prototype's 2,336 plus the zero-area triangles at the poles of the 4 hinge balls (20 each).
    assert_eq!(triangles(&out, "brass"), 2336 + 80);
    assert_eq!(triangles(&out, "holes"), 96);
    assert_eq!(triangles(&out, "stitches"), 892);
    assert_eq!(triangles(&out, "seams"), 920);
    assert_eq!(triangles(&out, "leather"), 28168);
}

/// The distance from `p` to the nearest triangle of `mesh`'s surface.
fn distance_to(p: DVec3, mesh: &omuzaki_char_api::PartMesh) -> f64 {
    let g = &mesh.geometry;
    let q = |i: u32| g.positions[i as usize].as_dvec3();
    g.indices
        .chunks_exact(3)
        .map(|t| closest_on_triangle(p, q(t[0]), q(t[1]), q(t[2])).distance(p))
        .fold(f64::INFINITY, f64::min)
}

fn closest_on_triangle(p: DVec3, a: DVec3, b: DVec3, c: DVec3) -> DVec3 {
    let (ab, ac, ap) = (b - a, c - a, p - a);
    let (d1, d2) = (ab.dot(ap), ac.dot(ap));
    if d1 <= 0.0 && d2 <= 0.0 {
        return a;
    }
    let bp = p - b;
    let (d3, d4) = (ab.dot(bp), ac.dot(bp));
    if d3 >= 0.0 && d4 <= d3 {
        return b;
    }
    let vc = d1 * d4 - d3 * d2;
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        return a + ab * (d1 / (d1 - d3));
    }
    let cp = p - c;
    let (d5, d6) = (ab.dot(cp), ac.dot(cp));
    if d6 >= 0.0 && d5 <= d6 {
        return c;
    }
    let vb = d5 * d2 - d1 * d6;
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        return a + ac * (d2 / (d2 - d6));
    }
    let va = d3 * d6 - d5 * d4;
    if va <= 0.0 && (d4 - d3) >= 0.0 && (d5 - d6) >= 0.0 {
        return b + (c - b) * ((d4 - d3) / ((d4 - d3) + (d5 - d6)));
    }
    let denom = 1.0 / (va + vb + vc);
    a + ab * (vb * denom) + ac * (vc * denom)
}

#[test]
fn stitches_lie_on_the_leather_within_the_rail_tolerance() {
    // The rails lie 0.3 mm off the leather; a stitch's middle may stray 0.6 mm from there: the leather is a mesh, and on
    // the toe's dome its chords sag away from the surface the rails follow.
    const RAIL: f64 = 0.0006;
    const LIFT: f64 = 0.0003;
    let out = built("soft-sd", &params(|p| p.straps.count = 0), Quality::Offline);
    let leather = out.meshes.iter().find(|m| m.name.ends_with("leather")).unwrap();
    let thread = out.meshes.iter().find(|m| m.name.ends_with("stitches")).unwrap();
    let left: Vec<DVec3> = thread.geometry.positions.iter().map(|p| p.as_dvec3()).filter(|p| p.x > 0.0).collect();
    assert_eq!(left.len(), 199 * 4, "the welt, cuff, back seam and toe-cap rows");
    for dash in left.chunks_exact(4).step_by(2) {
        let centre = dash.iter().sum::<DVec3>() / 4.0;
        let d = distance_to(centre, leather);
        assert!(
            (d - LIFT).abs() <= RAIL,
            "a stitch at {centre} is {:.3} mm from the leather, {:.3} mm off its rail",
            d * 1e3,
            (d - LIFT).abs() * 1e3
        );
    }
}

// ------------------------------------------------------------------------------------------------------ params

/// One change to the parameters that makes them invalid.
type Breaks = Box<dyn Fn(&mut BootsParams)>;

#[test]
fn parameters_are_checked_and_every_range_is_enforced() {
    assert!(BootsParams::default().validate().is_ok());
    let cases: [(&str, Breaks); 12] = [
        ("toe/width", Box::new(|p| p.toe.width = f64::NAN)),
        ("toe/roundness", Box::new(|p| p.toe.roundness = 1.5)),
        ("heel/length", Box::new(|p| p.heel.length = 0.0)),
        ("sole/thickness", Box::new(|p| p.sole.thickness = f64::INFINITY)),
        ("sole/welt", Box::new(|p| p.sole.welt = -0.001)),
        ("cuff/height", Box::new(|p| p.cuff.height = 0.5)),
        ("straps/count", Box::new(|p| p.straps.count = MAX_STRAPS + 1)),
        ("straps/height", Box::new(|p| p.straps.height = 0.0)),
        ("stitching/pitch", Box::new(|p| p.stitching.pitch = 0.0)),
        ("common/ease", Box::new(|p| p.common.ease = 0.5)),
        (
            "common/fabric",
            Box::new(|p| {
                p.common.fabric = omuzaki_char::outfit::common::FabricStyle::Weave {
                    warp: omuzaki_face_api::ColourRef::rgb(0, 0, 0),
                    weft: omuzaki_face_api::ColourRef::rgb(0, 0, 0),
                    thread: 0.002,
                }
            }),
        ),
        (
            "profile/zf",
            Box::new(|p| {
                let mut profile = synthetic_profile();
                profile.zf = vec![[0.0, f64::NAN], [0.1, 0.05]];
                p.profile = Some(profile);
            }),
        ),
    ];
    for (path, edit) in cases {
        let mut p = BootsParams::default();
        edit(&mut p);
        let err = p.validate().expect_err(path).to_string();
        assert!(err.contains(path), "{path} isn't named in `{err}`");
    }
}

#[test]
fn a_profile_whose_front_meets_its_back_is_refused() {
    let mut profile = synthetic_profile();
    profile.zf = profile.zb.clone();
    let err = params(|p| p.profile = Some(profile)).validate().unwrap_err().to_string();
    assert!(err.contains("zf"), "{err}");
}

// ------------------------------------------------------------------------------------------- the explicit profile

/// A boot outline made up for these tests (not any character's).
fn synthetic_profile() -> omuzaki_char::outfit::boots::BootProfile {
    omuzaki_char::outfit::boots::BootProfile {
        cx: vec![[0.0, 0.05], [0.12, 0.05]],
        zb: vec![[0.0, -0.04], [0.12, -0.04]],
        zf: vec![[0.0, 0.09], [0.015, 0.11], [0.05, 0.07], [0.09, 0.045], [0.12, 0.048]],
        rx: vec![[0.0, 0.034], [0.03, 0.04], [0.12, 0.042]],
    }
}

#[test]
fn an_explicit_profile_builds_the_boot_in_the_bind_frame_with_its_sole_on_the_floor() {
    let out = built(
        "soft-sd",
        &params(|p| {
            p.profile = Some(synthetic_profile());
            p.pull_tab = false;
        }),
        Quality::Offline,
    );
    let sole = out.meshes.iter().find(|m| m.name.ends_with("sole")).unwrap();
    let lowest = sole.geometry.positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
    assert!(lowest.abs() < 1e-6, "the sole's bottom is at {lowest}");
    let leather = out.meshes.iter().find(|m| m.name.ends_with("leather")).unwrap();
    let top = leather.geometry.positions.iter().map(|p| p.y).fold(0.0, f32::max);
    assert!((top - 0.1195).abs() < 0.002, "the cuff top is at {top}");
    let (lo, hi) = leather
        .geometry
        .positions
        .iter()
        .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), p| (lo.min(p.x), hi.max(p.x)));
    assert!((lo + hi).abs() < 1e-5, "the right boot mirrors the left: x spans {lo}..{hi}");
}

#[test]
fn a_derived_boot_stands_its_sole_under_the_foot_and_its_cuff_up_the_shin() {
    for template in conformance::TEMPLATES {
        let low = built(template, &params(|p| p.height = BootHeight::Ankle), Quality::Live);
        let high = built(template, &params(|p| p.height = BootHeight::Knee), Quality::Live);
        let top = |out: &PartOutput| {
            out.meshes
                .iter()
                .filter(|m| m.name.ends_with("leather"))
                .flat_map(|m| m.geometry.positions.iter().map(|p| p.y))
                .fold(0.0, f32::max)
        };
        assert!(
            top(&high) > top(&low) + 0.02,
            "{template}: knee boots ({}) aren't taller than ankle boots ({})",
            top(&high),
            top(&low)
        );
        let sole = high.meshes.iter().find(|m| m.name.ends_with("sole")).unwrap();
        let lowest = sole.geometry.positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        assert!(lowest < -0.005, "{template}: the sole's bottom is at {lowest}: the foot should rest on the sole");
    }
}

#[test]
fn the_skin_hands_over_from_the_foot_to_the_shaft_and_the_toes() {
    let out = built("soft-sd", &BootsParams::default(), Quality::Live);
    let leather = out.meshes.iter().find(|m| m.name.ends_with("leather")).unwrap();
    let g = &leather.geometry;
    let names: Vec<String> = leather.joint_bones.iter().map(|j| format!("{j:?}")).collect();
    let used = |bone: &str| {
        let slot = names.iter().position(|n| n.contains(bone)).unwrap_or(usize::MAX);
        g.joints.iter().zip(&g.weights).any(|(j, w)| (0..4).any(|k| usize::from(j[k]) == slot && w[k] > 0.5))
    };
    for bone in ["LeftLowerLeg", "LeftFoot", "RightLowerLeg", "RightFoot"] {
        assert!(used(bone), "no vertex of the leather is mostly on {bone}: {names:?}");
    }
    assert!(names.iter().any(|n| n.contains("LeftToes")) && names.iter().any(|n| n.contains("RightToes")), "{names:?}");
}

// ------------------------------------------------------------------------------------------------------ render

/// Renders the boots on every template from four sides through the mock toon renderer, when
/// `OMUZAKI_BOOTS_PPM=<directory>` is set, as binary PPM files.
#[test]
fn render_the_boots_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_BOOTS_PPM") else { return };
    for template in conformance::TEMPLATES {
        let def = conformance::character(
            template,
            &BodyShape::default(),
            &[("boots", "boots", LAYER_FOOTWEAR, serde_json::to_value(BootsParams::default()).unwrap())],
        );
        let stack = conformance::build(def, &services()).unwrap();
        render::render(&stack, render::Framing::parts(&stack, &["boots"]), template, directory.as_ref());
    }
}
