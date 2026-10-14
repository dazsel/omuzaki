//! The `body` part kind: the lofted torso, arms, legs and feet of every template.
//!
//! - invariants of every region over random proportions and shapes on each template: watertight, no degenerate
//!   triangle, normals and winding outward, positive volume, smooth skin weights;
//! - the junction metric: how sharply the silhouette turns where regions meet, per template and preset;
//! - the kind through the registry builder: provenance, materials, published fit, determinism, build time.
//!
//! Parity with the prototype's loft is in `crates/omuzaki-test/tests/char_body_loft_parity.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures that must exist
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // small counts
#![allow(clippy::many_single_char_names, clippy::similar_names, clippy::chunks_exact_to_as_chunks)] // geometry: a, b, c, x, y, z, rx, ry; triangles as slices of 3

use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::body::mesh::{self, Detail};
use omuzaki_char::kit::skin::{SegmentSet, max_neighbour_jump};
use omuzaki_char::template::registry;
use omuzaki_char::template::shape;
use omuzaki_char::template::skeleton::TemplateFit;
use omuzaki_char_api::{
    BodyFit, BodyRegion, BodyShape, BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, HandParams,
    KindProvenance, MaterialName, PartMesh, Proportions, Quality, RegistryBuilder, find_kind,
};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::rng::Rng;

/// The regions of a body of `template` shaped by `shape`, built at `detail`.
fn body(template: &str, shape: &BodyShape, detail: Detail) -> Vec<(BodyRegion, PartMesh)> {
    let t = registry::find(template).unwrap();
    let layout = t.try_layout(&Proportions::default(), shape, &HandParams::default()).unwrap();
    body_of(layout, detail)
}

fn body_of(layout: omuzaki_char::template::layout::BodyLayout, detail: Detail) -> Vec<(BodyRegion, PartMesh)> {
    let fit = TemplateFit::new(layout);
    let segments = SegmentSet::from_fit(&fit, &mesh::all_bones()).unwrap();
    mesh::build_regions(fit.layout(), &segments, detail, &MaterialName::new("body-skin").unwrap()).unwrap()
}

// ------------------------------------------------------------------------------------------------ invariants

/// 200 random proportions and body shapes per template: the numbers are drawn over the whole valid range.
const SAMPLES: usize = 200;

fn random_layout(
    t: &dyn omuzaki_char::template::Template,
    rng: &mut Rng,
) -> omuzaki_char::template::layout::BodyLayout {
    let mut slider = || f64::from(rng.range_f32(0.5, 2.0));
    let proportions = Proportions {
        neck_length: slider(),
        torso_length: slider(),
        shoulder_width: slider(),
        hip_width: slider(),
        arm_length: slider(),
        leg_length: slider(),
        hand_size: slider(),
        foot_size: slider(),
    };
    let mut axis = || f64::from(rng.range_f32(-1.0, 1.0));
    let shape = BodyShape {
        height: axis(),
        build: axis(),
        curves: axis(),
        musculature: axis(),
        shoulder_hip: axis(),
        preset: None,
    };
    t.try_layout(&proportions, &shape, &HandParams::default()).unwrap()
}

/// Welds vertices that share a position (the seam and pole copies) and returns each triangle as welded ids.
fn welded(m: &PartMesh) -> Vec<[usize; 3]> {
    let mut ids: HashMap<[i64; 3], usize> = HashMap::new();
    let q = |p: Vec3| [p.x, p.y, p.z].map(|v| (f64::from(v) * 1e6).round() as i64);
    let id: Vec<usize> = m
        .geometry
        .positions
        .iter()
        .map(|p| {
            let next = ids.len();
            *ids.entry(q(*p)).or_insert(next)
        })
        .collect();
    m.geometry.indices.chunks_exact(3).map(|t| [id[t[0] as usize], id[t[1] as usize], id[t[2] as usize]]).collect()
}

fn check_region(label: &str, region: BodyRegion, m: &PartMesh) {
    let g = &m.geometry;
    let tris = welded(m);
    // Watertight and consistently wound: every directed edge once, and its reverse once.
    let mut edges: HashMap<(usize, usize), u32> = HashMap::new();
    for t in &tris {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *edges.entry((a, b)).or_default() += 1;
        }
    }
    for (&(a, b), &n) in &edges {
        assert_eq!(n, 1, "{label} {region:?}: edge {a}-{b} is used {n} times in one direction");
        assert!(edges.contains_key(&(b, a)), "{label} {region:?}: edge {a}-{b} has no partner: a hole");
    }
    let mut volume = 0.0_f64;
    for (k, t) in g.indices.chunks_exact(3).enumerate() {
        let p = [t[0], t[1], t[2]].map(|i| g.positions[i as usize].as_dvec3());
        let face = (p[1] - p[0]).cross(p[2] - p[0]);
        assert!(face.length() > 2e-12, "{label} {region:?}: triangle {k} is degenerate ({:e})", face.length());
        // Outward: the corners' normals and the winding agree.
        let n: DVec3 = [t[0], t[1], t[2]].iter().map(|&i| g.normals[i as usize].as_dvec3()).sum();
        assert!(face.dot(n) > 0.0, "{label} {region:?}: triangle {k} is wound against its normals");
        volume += p[0].dot(p[1].cross(p[2])) / 6.0;
    }
    assert!(volume > 0.0, "{label} {region:?}: volume {volume:e}");
    let jump = max_neighbour_jump(m);
    assert!(jump <= 0.35, "{label} {region:?}: adjacent vertices' weights jump by {jump}");
    m.validate().unwrap();
}

#[test]
fn every_region_is_a_sound_closed_surface_over_random_proportions_and_shapes() {
    // One thread per template: 800 bodies are slow in a debug build.
    std::thread::scope(|scope| {
        for t in registry::templates() {
            scope.spawn(move || {
                let mut rng = Rng::new(0x445).fork(t.id().bytes().map(u64::from).sum());
                for sample in 0..SAMPLES {
                    let layout = random_layout(t, &mut rng);
                    for (region, m) in body_of(layout, Detail::LIVE) {
                        check_region(&format!("{} #{sample}", t.id()), region, &m);
                    }
                }
            });
        }
    });
}

#[test]
fn offline_bodies_are_sound_too_and_denser() {
    for t in registry::templates() {
        let live: usize =
            body(t.id(), &BodyShape::default(), Detail::LIVE).iter().map(|(_, m)| m.geometry.triangle_count()).sum();
        let offline = body(t.id(), &BodyShape::default(), Detail::OFFLINE);
        for (region, m) in &offline {
            check_region(t.id(), *region, m);
        }
        let dense: usize = offline.iter().map(|(_, m)| m.geometry.triangle_count()).sum();
        assert!(dense > 3 * live, "{}: {dense} offline against {live} live", t.id());
    }
}

#[test]
fn a_live_body_is_within_the_triangle_budget() {
    for t in registry::templates() {
        let n: usize =
            body(t.id(), &BodyShape::default(), Detail::LIVE).iter().map(|(_, m)| m.geometry.triangle_count()).sum();
        eprintln!("{}: {n} triangles at Live", t.id());
        assert!(n <= 6000, "{}: {n} triangles", t.id());
    }
}

// ------------------------------------------------------------------------------------------------ the kind

fn character(template: &str, body: &serde_json::Value) -> CharacterDef {
    let node = serde_json::json!({
        "name": "Body test",
        "template": template,
        "seed": 3,
        "palette": { "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80" },
        "parts": { "body": { "kind": "body", "params": body } }
    });
    CharacterDef::from_node(&node).unwrap()
}

fn build(def: &CharacterDef, quality: Quality) -> omuzaki_char_api::BuiltCharacter {
    let options = BuildOptions { quality, ..BuildOptions::default() };
    RegistryBuilder
        .build(&CharId::new("t").unwrap(), def, &options, &BuildServices::default())
        .unwrap_or_else(|e| panic!("the body does not build: {e}"))
}

#[test]
fn the_kind_is_registered_project_owned_and_builds_a_rigged_body_on_every_template() {
    let kind = find_kind("body").expect("the body kind registers itself");
    assert_eq!((kind.provenance)(), KindProvenance::ProjectOwned);
    assert_eq!((kind.meta)().category, omuzaki_char_api::PartCategory::Body);
    for t in registry::templates() {
        let built = build(&character(t.id(), &serde_json::json!({})), Quality::Live);
        let names: Vec<&str> = built.meshes.iter().map(|m| m.mesh.name.as_str()).collect();
        assert_eq!(names, ["torso", "arm.L", "arm.R", "leg.L", "leg.R", "foot.L", "foot.R"], "{}", t.id());
        // The skin material: toon with the skin treatment and a coloured outline, from the palette's slots.
        let material = &built.materials[&MaterialName::new("body-skin").unwrap()];
        assert!(material.outline.is_some(), "{}", t.id());
        assert!(
            matches!(&material.shading, omuzaki_render_api::Shading::Toon(toon) if toon.skin.is_some()),
            "{}",
            t.id()
        );
        assert_eq!(material.base, omuzaki_core::colour::Colour::rgb(0xf9, 0xd3, 0xc5));
        // Every skinned bone exists in the rig the template published, and the body is bound in the template's pose.
        for m in &built.meshes {
            for bone in &m.mesh.joint_bones {
                assert!(built.rig.bone(bone).is_some(), "{}: {bone:?} is not in the rig", t.id());
            }
        }
        let layout = t.layout(&Proportions::default(), &BodyShape::default(), &HandParams::default());
        let fit = TemplateFit::new(layout);
        assert_eq!(built.rig.bind.rotations, fit.bind().rotations, "{}", t.id());
        // The body publishes the template's stature, and the build scaled the meshes to it.
        let stature = built.stature.expect("the body publishes its stature");
        assert_eq!(Some(stature), t.stature(fit.layout()), "{}", t.id());
        let top =
            built.meshes.iter().flat_map(|m| m.mesh.geometry.positions.iter().map(|p| p.y)).fold(f32::MIN, f32::max);
        let authored = fit.layout().head_base().unwrap().y as f32 * stature.scale;
        assert!(
            top > 0.8 * authored && top < 1.05 * authored,
            "{}: the body reaches {top} m, the neck base is at {authored} m",
            t.id()
        );
        assert!(built.report.triangles() <= 6000, "{}: {}", t.id(), built.report.triangles());
    }
}

#[test]
fn a_build_is_a_pure_function_of_its_parameters_and_follows_them() {
    let def = character("soft-sd", &serde_json::json!({}));
    let (a, b) = (build(&def, Quality::Live), build(&def, Quality::Live));
    for (x, y) in a.meshes.iter().zip(&b.meshes) {
        assert_eq!(x.mesh, y.mesh, "{}", x.mesh.name);
    }
    let tall =
        character("soft-sd", &serde_json::json!({ "shape": { "height": 1.0 }, "proportions": { "arm_length": 1.3 } }));
    let c = build(&tall, Quality::Live);
    let top = |m: &omuzaki_char_api::BuiltCharacter| {
        m.meshes.iter().flat_map(|m| m.mesh.geometry.positions.iter().map(|p| p.y)).fold(0.0, f32::max)
    };
    assert!(top(&c) > top(&a), "the height axis lengthens the body");
    let reach = |m: &omuzaki_char_api::BuiltCharacter| {
        m.meshes.iter().flat_map(|m| m.mesh.geometry.positions.iter().map(|p| p.x)).fold(0.0, f32::max)
    };
    assert!(reach(&c) > reach(&a), "longer arms reach further out");
    assert!(build(&def, Quality::Offline).report.triangles() > 3 * a.report.triangles());
}

#[test]
fn bad_parameters_and_unknown_templates_are_errors_not_panics() {
    let bad = character("soft-sd", &serde_json::json!({ "proportions": { "leg_length": 9.0 } }));
    let r =
        RegistryBuilder.build(&CharId::new("t").unwrap(), &bad, &BuildOptions::default(), &BuildServices::default());
    assert!(r.is_err());
    let unknown = character("no-such-template", &serde_json::json!({}));
    let e = RegistryBuilder
        .build(&CharId::new("t").unwrap(), &unknown, &BuildOptions::default(), &BuildServices::default())
        .expect_err("an unknown template fails");
    assert!(e.to_string().contains("no-such-template"), "{e}");
}

/// The body's part build time at Live, on this machine: the issue's budget is 40 ms. Debug builds are an order of
/// magnitude slower, so the hard check is loose there; `cargo test --release -- --nocapture` prints the real figure.
///
/// Release (`CARGO_INCREMENTAL=0`): best 6.79 ms alone; best 6.59 ms under a full `omuzaki-char` run in parallel.
/// Debug under a full workspace run in parallel (`cargo test --workspace -- --nocapture`): best 340 ms and 264 ms on
/// this 4-vCPU VM. The cloud VM's gate measured best 490 ms, which the 400 ms bound failed. The debug bound is that
/// figure ×2, rounded up to 1000 ms; the release bound stays at 40 ms.
/// The test asserts on the best of 20 builds, not the mean: the tests run in parallel, so a busy machine can slow many
/// builds in a row, and the mean would then measure the scheduler rather than the work.
#[test]
fn a_live_body_builds_within_the_budget() {
    let def = character("standard", &serde_json::json!({}));
    let _warm = build(&def, Quality::Live);
    let mut times = Vec::with_capacity(20);
    for _ in 0..20 {
        let start = Instant::now();
        let _ = build(&def, Quality::Live);
        times.push(start.elapsed().as_secs_f64() * 1e3);
    }
    let best = times.iter().copied().fold(f64::MAX, f64::min);
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    eprintln!("body build at Live: best {best:.2} ms, mean {mean:.2} ms (whole character build)");
    assert!(best < if cfg!(debug_assertions) { 1000.0 } else { 40.0 }, "{best} ms");
}

/// Writes every template and preset as an OBJ file per body, for `tools/body-kind-sheet.py`.
/// `OMUZAKI_SHEET_DIR=<dir> cargo test -p omuzaki-char --test body -- --ignored dump`.
#[test]
#[ignore = "writes the sheets' meshes to $OMUZAKI_SHEET_DIR"]
#[allow(clippy::disallowed_methods, reason = "a developer tool writing to a directory the person named, not an op")]
fn dump() {
    let dir = PathBuf::from(std::env::var("OMUZAKI_SHEET_DIR").expect("set OMUZAKI_SHEET_DIR to a directory"));
    fs::create_dir_all(&dir).unwrap();
    let presets = shape::presets().unwrap();
    for template in registry::templates() {
        for preset in presets {
            let mut obj = String::new();
            let mut base = 1_usize;
            let t = registry::find(template.id()).unwrap();
            let layout = t.layout(&Proportions::default(), &preset.shape, &HandParams::default());
            for (bone, name) in [
                (HumanoidBone::LeftUpperArm, "shoulder"),
                (HumanoidBone::LeftLowerArm, "elbow"),
                (HumanoidBone::LeftHand, "wrist"),
                (HumanoidBone::LeftUpperLeg, "hip"),
                (HumanoidBone::LeftLowerLeg, "knee"),
                (HumanoidBone::LeftFoot, "ankle"),
                (HumanoidBone::Neck, "neck"),
            ] {
                let p = layout.joint64(bone).unwrap();
                writeln!(obj, "# joint {name} {} {} {}", p.x, p.y, p.z).unwrap();
            }
            let head = layout.head_base().unwrap();
            writeln!(obj, "# joint head {} {} {}", head.x, head.y, head.z).unwrap();
            for (region, m) in body(template.id(), &preset.shape, Detail::LIVE) {
                let g = &m.geometry;
                writeln!(obj, "g {region:?}").unwrap();
                for p in &g.positions {
                    writeln!(obj, "v {} {} {}", p.x, p.y, p.z).unwrap();
                }
                for n in &g.normals {
                    writeln!(obj, "vn {} {} {}", n.x, n.y, n.z).unwrap();
                }
                for t in g.indices.chunks(3) {
                    let [a, b, c] = [t[0], t[1], t[2]].map(|i| i as usize + base);
                    writeln!(obj, "f {a}//{a} {b}//{b} {c}//{c}").unwrap();
                }
                base += g.positions.len();
            }
            fs::write(dir.join(format!("{}-{}.obj", template.id(), preset.id.as_str())), obj).unwrap();
        }
    }
}

// ------------------------------------------------------------------------------------------------ junction metric

/// A silhouette outline: the largest screen-x of the meshes at each height of a window, for the left side.
struct Outline {
    /// Heights, metres, from the window's bottom to its top, `step` apart.
    ys: Vec<f64>,
    /// The outline's x (front view) or z (side view) at each height; NaN where the meshes have nothing.
    xs: Vec<f64>,
}

/// The outline of `meshes` seen from the front (x) or the side (z) between `y0` and `y1`, sampled every `step`: each
/// triangle that crosses a height contributes where it cuts that plane.
fn outline(meshes: &[&PartMesh], side: bool, y0: f64, y1: f64, step: f64) -> Outline {
    let n = ((y1 - y0) / step).round() as usize + 1;
    let ys: Vec<f64> = (0..n).map(|k| y0 + step * k as f64).collect();
    let mut xs = vec![f64::NAN; n];
    for m in meshes {
        let g = &m.geometry;
        let pick = |p: omuzaki_core::glam::Vec3| if side { f64::from(p.z) } else { f64::from(p.x) };
        for t in g.indices.chunks_exact(3) {
            let v = [t[0], t[1], t[2]].map(|i| g.positions[i as usize]);
            let (lo, hi) =
                v.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(f64::from(p.y)), b.max(f64::from(p.y))));
            let first = ((lo - y0) / step).ceil().max(0.0) as usize;
            for k in first..n.min(((hi - y0) / step).floor().max(-1.0) as usize + 1) {
                let y = ys[k];
                for e in [(0, 1), (1, 2), (2, 0)] {
                    let (a, b) = (v[e.0], v[e.1]);
                    let (ya, yb) = (f64::from(a.y), f64::from(b.y));
                    if (ya - y) * (yb - y) <= 0.0 && (ya - yb).abs() > 1e-12 {
                        let x = pick(a) + (pick(b) - pick(a)) * (y - ya) / (yb - ya);
                        xs[k] = if xs[k].is_nan() { x } else { xs[k].max(x) };
                    }
                }
            }
        }
    }
    Outline { ys, xs }
}

/// Steps in each chord of the turn measure: 3 mm at the metric's 1 mm step.
const CHORD: usize = 3;

/// The sharpest *concave* turn of the outline's polyline, in degrees, between consecutive chords of [`CHORD`] steps: 0
/// for a straight or rounded-out edge, `CHORD * step / R` radians where the edge dips in along an arc of radius `R`, and
/// near 90 for a notch or a step in the silhouette. Convex turns (a dome, a shoulder's roundness) are not steps.
fn sharpest_notch(o: &Outline) -> f64 {
    let p = |k: usize| (o.xs[k], o.ys[k]);
    let mut worst = 0.0_f64;
    for k in CHORD..o.ys.len().saturating_sub(CHORD) {
        let (a, b, c) = (p(k - CHORD), p(k), p(k + CHORD));
        if [a.0, b.0, c.0].iter().any(|x| x.is_nan()) {
            continue;
        }
        let (u, v) = ((b.0 - a.0, b.1 - a.1), (c.0 - b.0, c.1 - b.1));
        // Going up the outer edge, a turn towards the body is a counter-clockwise one for a dome and a clockwise one for a
        // notch.
        let turn = (u.0 * v.1 - u.1 * v.0).atan2(u.0 * v.0 + u.1 * v.1).to_degrees();
        worst = worst.max(-turn);
    }
    worst
}

/// The joints the metric looks at: the regions whose outlines meet there (the upper one first), or one region at a
/// joint that has none to meet.
const JUNCTIONS: [(&str, HumanoidBone, &[BodyRegion]); 5] = [
    ("hip", HumanoidBone::LeftUpperLeg, &[BodyRegion::Torso, BodyRegion::LegL]),
    ("shoulder", HumanoidBone::LeftUpperArm, &[BodyRegion::Torso, BodyRegion::ArmL]),
    ("knee", HumanoidBone::LeftLowerLeg, &[BodyRegion::LegL]),
    ("elbow", HumanoidBone::LeftLowerArm, &[BodyRegion::ArmL]),
    ("ankle", HumanoidBone::LeftFoot, &[BodyRegion::LegL, BodyRegion::FootL]),
];

/// The height at which the outline of the first of `regions` crosses the second's: where the union's edge passes from
/// one region to the other, which is where a step would show. `fallback` for one region.
fn seam_height(
    meshes: &[(BodyRegion, PartMesh)],
    regions: &[BodyRegion],
    side: bool,
    y: f64,
    half: f64,
    step: f64,
    fallback: f64,
) -> f64 {
    let [upper, lower] = regions else { return fallback };
    let of = |r: &BodyRegion| {
        let m: Vec<&PartMesh> = meshes.iter().filter(|(k, _)| k == r).map(|(_, m)| m).collect();
        outline(&m, side, y - half, y + half, step)
    };
    let (a, b) = (of(upper), of(lower));
    let diff: Vec<f64> = a.xs.iter().zip(&b.xs).map(|(p, q)| p - q).collect();
    // The last height, going up, where the upper region's edge is still inside the lower region's; failing that (the
    // lower region's edge stays outside, as an arm's does) the top of the lower region, where its edge ends.
    (1..diff.len())
        .rev()
        .find(|&k| diff[k - 1].is_finite() && diff[k].is_finite() && diff[k - 1] <= 0.0 && diff[k] > 0.0)
        .or_else(|| (0..b.xs.len()).rev().find(|&k| b.xs[k].is_finite()))
        .map_or(fallback, |k| a.ys[k])
}

/// The junction step in degrees within 2 cm (scaled by the body) of each junction's seam: the sharpest notch (a concave
/// turn) in the outline, and where two regions meet only how much sharper the union's notch is than either region's own, front view then side
/// view, measured on the offline mesh so facets do not read as corners.
fn junction_turns(template: &str, shape: &BodyShape) -> Vec<(&'static str, f64, f64)> {
    let t = registry::find(template).unwrap();
    let layout = t.layout(&Proportions::default(), shape, &HandParams::default());
    let unit = layout.head_base().unwrap().y / 0.56;
    let meshes = body(template, shape, Detail::OFFLINE);
    JUNCTIONS
        .iter()
        .map(|&(name, bone, regions)| {
            let y = layout.joint64(bone).unwrap().y;
            let chosen: Vec<&PartMesh> = meshes.iter().filter(|(r, _)| regions.contains(r)).map(|(_, m)| m).collect();
            let step = 0.001 * unit;
            let turn = |side: bool| {
                let seam = seam_height(&meshes, regions, side, y, 0.06 * unit, step, y);
                // Above the torso's top is the neck, which the head part builds.
                let top = if regions.contains(&BodyRegion::Torso) {
                    layout.section64(BodyRegion::Torso, 1.0).unwrap().centre.y
                } else {
                    f64::MAX
                };
                let window = |m: &[&PartMesh]| {
                    sharpest_notch(&outline(m, side, seam - 0.02 * unit, (seam + 0.02 * unit).min(top), step))
                };
                // A body's own curves (a pinched waist) are not junction steps: count only what the union dips beyond
                // its sharpest part.
                let parts = regions
                    .iter()
                    .map(|r| window(&meshes.iter().filter(|(k, _)| k == r).map(|(_, m)| m).collect::<Vec<_>>()))
                    .fold(0.0, f64::max);
                if regions.len() > 1 { (window(&chosen) - parts).max(0.0) } else { window(&chosen) }
            };
            (name, turn(false), turn(true))
        })
        .collect()
}

/// The most the front-view outline may dip at each junction, in degrees of the junction metric. Before the junctions were
/// shaped the worst presets measured: hip 31, shoulder 63, ankle 94 (measured then); the ankle was held to 35 until the waist was filled at the foot's
/// dome. The limbs' own bends (knee, elbow) are gentle.
const FRONT_LIMITS: [(&str, f64); 5] =
    [("hip", 10.0), ("shoulder", 5.0), ("knee", 10.0), ("elbow", 10.0), ("ankle", 10.0)];

#[test]
fn the_front_silhouette_has_no_step_at_any_junction_on_any_template_or_preset() {
    for template in registry::templates() {
        for preset in shape::presets().unwrap() {
            for (name, front, _) in junction_turns(template.id(), &preset.shape) {
                let limit = FRONT_LIMITS.iter().find(|l| l.0 == name).unwrap().1;
                assert!(
                    front <= limit,
                    "{} {}: the {name} dips {front:.1} degrees (limit {limit})",
                    template.id(),
                    preset.id.as_str()
                );
            }
        }
    }
}

/// Prints the table of sharpest outline turns (front / side, degrees), the worst of the ten presets per template, and
/// which preset it was.
#[test]
#[ignore = "prints the junction table"]
fn junction_table() {
    for template in registry::templates() {
        let mut worst = vec![((0.0_f64, ""), (0.0_f64, "")); JUNCTIONS.len()];
        for preset in shape::presets().unwrap() {
            for (k, (_, front, side)) in junction_turns(template.id(), &preset.shape).into_iter().enumerate() {
                if front > worst[k].0.0 {
                    worst[k].0 = (front, preset.id.as_str());
                }
                if side > worst[k].1.0 {
                    worst[k].1 = (side, preset.id.as_str());
                }
            }
        }
        let cells: Vec<String> = JUNCTIONS
            .iter()
            .zip(&worst)
            .map(|(j, w)| format!("{} {:.0} {}/{:.0} {}", j.0, w.0.0, w.0.1, w.1.0, w.1.1))
            .collect();
        println!("{:8} {}", template.id(), cells.join("  "));
    }
}

/// How far the front outline of the leg and foot dips at the top of the foot's dome: the outline's x there over the mean
/// of its x a little above and below (1 for a straight or convex run, less for a waist), on the offline mesh.
fn ankle_dip(template: &str, shape: &BodyShape) -> f64 {
    let t = registry::find(template).unwrap();
    let layout = t.layout(&Proportions::default(), shape, &HandParams::default());
    let unit = layout.head_base().unwrap().y / 0.56;
    let meshes = body(template, shape, Detail::OFFLINE);
    let foot: Vec<&PartMesh> = meshes.iter().filter(|(r, _)| *r == BodyRegion::FootL).map(|(_, m)| m).collect();
    let both: Vec<&PartMesh> =
        meshes.iter().filter(|(r, _)| matches!(r, BodyRegion::FootL | BodyRegion::LegL)).map(|(_, m)| m).collect();
    let top = foot.iter().flat_map(|m| &m.geometry.positions).map(|p| f64::from(p.y)).fold(f64::MIN, f64::max);
    let d = 0.012 * unit;
    let o = outline(&both, false, top - d, top + d, d);
    let [below, at, above] = [o.xs[0], o.xs[1], o.xs[2]];
    at / f64::midpoint(below, above)
}

/// The dip at the foot's dome stays shallow on every template and preset.
const MAX_ANKLE_DIP: f64 = 0.05;

#[test]
fn the_front_outline_has_no_waist_at_the_foot_dome() {
    for template in registry::templates() {
        for preset in shape::presets().unwrap() {
            let dip = 1.0 - ankle_dip(template.id(), &preset.shape);
            assert!(
                dip <= MAX_ANKLE_DIP,
                "{} {}: the foot dome has a waist of {dip:.3}",
                template.id(),
                preset.id.as_str()
            );
        }
    }
}

/// Prints the ankle's worst front turn and waist over the ten presets and the 32 corners of the shape cube, per template.
#[test]
#[ignore = "prints the ankle table"]
fn ankle_table() {
    for template in registry::templates() {
        let corner = |c: u32| {
            let pick = |bit: u32| if c >> bit & 1 == 1 { 1.0 } else { -1.0 };
            BodyShape {
                height: pick(0),
                build: pick(1),
                curves: pick(2),
                musculature: pick(3),
                shoulder_hip: pick(4),
                preset: None,
            }
        };
        let shapes: Vec<(String, BodyShape)> = shape::presets()
            .unwrap()
            .iter()
            .map(|p| (p.id.as_str().to_string(), p.shape.clone()))
            .chain((0..32).map(|c| (format!("corner{c}"), corner(c))))
            .collect();
        let mut worst = vec![(0.0_f64, String::new()); 2];
        for (name, shape) in &shapes {
            let turn = junction_turns(template.id(), shape).into_iter().find(|j| j.0 == "ankle").unwrap().1;
            let dip = 1.0 - ankle_dip(template.id(), shape);
            for (w, v) in worst.iter_mut().zip([turn, dip]) {
                if v > w.0 {
                    *w = (v, name.clone());
                }
            }
        }
        println!("{:8} turn {:.1} ({})  dip {:.3} ({})", template.id(), worst[0].0, worst[0].1, worst[1].0, worst[1].1);
    }
}
