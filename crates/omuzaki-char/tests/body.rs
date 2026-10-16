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
use omuzaki_core::mesh::MeshData;
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
        assert_eq!(
            names,
            [
                "torso",
                "arm.L",
                "arm.R",
                "leg.L",
                "leg.R",
                "foot.L",
                "foot.R",
                "hand-l",
                "hand-lines-l",
                "hand-r",
                "hand-lines-r"
            ],
            "{}",
            t.id()
        );
        // The skin material: toon with the skin treatment and a coloured outline, from the palette's slots.
        let material = &built.materials[&MaterialName::new("body-skin").unwrap()];
        assert!(material.outline.is_some(), "{}", t.id());
        assert!(
            matches!(&material.shading, omuzaki_render_api::Shading::Toon(toon) if toon.skin.is_some()),
            "{}",
            t.id()
        );
        assert_eq!(material.base, omuzaki_core::colour::Colour::rgb(0xf9, 0xd3, 0xc5));
        // The hands have their own copy with the vertex colours on (the fingertip tint); the body's skin has them off.
        assert!(!material.vertex_colours, "{}", t.id());
        let hand_id = MaterialName::new("body-hands").unwrap();
        let hand_material = &built.materials[&hand_id];
        assert!(hand_material.vertex_colours, "{}", t.id());
        assert_eq!(hand_material.base, material.base, "{}", t.id());
        for m in &built.meshes {
            let hand = m.mesh.name.starts_with("hand-");
            assert_eq!(m.mesh.material == hand_id, hand, "{}: {}", t.id(), m.mesh.name);
            let lines = m.mesh.name.starts_with("hand-lines");
            assert!(!hand || lines || !m.mesh.geometry.colours.is_empty(), "{}: {} has no tint", t.id(), m.mesh.name);
        }
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
        // Two budgets: the body's own (§10, 6 000, the meshes that are not hands) and the whole character's, which adds
        // the two hands' 1.8k each (#157's 1.2k, amended to the prototype's hand, the parity target).
        let body_triangles: usize = built
            .meshes
            .iter()
            .filter(|m| !m.mesh.name.starts_with("hand-"))
            .map(|m| m.mesh.geometry.triangle_count())
            .sum();
        assert!(body_triangles <= 6000, "{}: the body is {body_triangles} triangles", t.id());
        assert!(built.report.triangles() <= 6000 + 2 * 1800, "{}: {}", t.id(), built.report.triangles());
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
/// `OMUZAKI_SHEET_DIR=<dir> cargo test -p omuzaki-char --test body -- --ignored dump`; the live mesh unless
/// `OMUZAKI_SHEET_DETAIL=offline`.
#[test]
#[ignore = "writes the sheets' meshes to $OMUZAKI_SHEET_DIR"]
#[allow(clippy::disallowed_methods, reason = "a developer tool writing to a directory the person named, not an op")]
fn dump() {
    let dir = PathBuf::from(std::env::var("OMUZAKI_SHEET_DIR").expect("set OMUZAKI_SHEET_DIR to a directory"));
    fs::create_dir_all(&dir).unwrap();
    let detail = if std::env::var("OMUZAKI_SHEET_DETAIL").is_ok_and(|d| d == "offline") {
        Detail::OFFLINE
    } else {
        Detail::LIVE
    };
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
            for (region, m) in body(template.id(), &preset.shape, detail) {
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
    let value: fn(Vec3) -> f64 = if side { |p| f64::from(p.z) } else { |p| f64::from(p.x) };
    let xs = sweep(meshes, |p| f64::from(p.y), value, y0, y1, step);
    let ys = (0..xs.len()).map(|k| y0 + step * k as f64).collect();
    Outline { ys, xs }
}

/// The top edge of `meshes` seen from the front between `x0` and `x1`, sampled every `step`: at each `x`, the largest `y`
/// of the triangle edges that cross that vertical plane, NaN where the meshes have nothing. [`outline`] with `x` and `y`
/// swapped.
fn top_edge(meshes: &[&PartMesh], x0: f64, x1: f64, step: f64) -> Vec<f64> {
    sweep(meshes, |p| f64::from(p.x), |p| f64::from(p.y), x0, x1, step)
}

/// Samples the largest `value` of the triangle edges of `meshes` that cross each plane `key = s` for `s` from `s0` to
/// `s1`, `step` apart: each triangle contributes where its edges cut the plane, and NaN where none do.
fn sweep(meshes: &[&PartMesh], key: fn(Vec3) -> f64, value: fn(Vec3) -> f64, s0: f64, s1: f64, step: f64) -> Vec<f64> {
    let n = ((s1 - s0) / step).round() as usize + 1;
    let mut out = vec![f64::NAN; n];
    for m in meshes {
        let g = &m.geometry;
        for t in g.indices.chunks_exact(3) {
            let v = [t[0], t[1], t[2]].map(|i| g.positions[i as usize]);
            let (lo, hi) = v.iter().fold((f64::MAX, f64::MIN), |(a, b), p| (a.min(key(*p)), b.max(key(*p))));
            let first = ((lo - s0) / step).ceil().max(0.0) as usize;
            let last = n.min(((hi - s0) / step).floor().max(-1.0) as usize + 1);
            for (k, slot) in out.iter_mut().enumerate().take(last).skip(first) {
                let s = s0 + step * k as f64;
                for e in [(0, 1), (1, 2), (2, 0)] {
                    let (a, b) = (v[e.0], v[e.1]);
                    let (ka, kb) = (key(a), key(b));
                    if (ka - s) * (kb - s) <= 0.0 && (ka - kb).abs() > 1e-12 {
                        let x = value(a) + (value(b) - value(a)) * (s - ka) / (kb - ka);
                        *slot = if slot.is_nan() { x } else { slot.max(x) };
                    }
                }
            }
        }
    }
    out
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
/// view, measured on the `detail` mesh (the offline one is the reference where facets do not read as corners; the live
/// one is what viewers see).
fn junction_turns(template: &str, shape: &BodyShape, detail: Detail) -> Vec<(&'static str, f64, f64)> {
    let t = registry::find(template).unwrap();
    let layout = t.layout(&Proportions::default(), shape, &HandParams::default());
    let unit = layout.head_base().unwrap().y / 0.56;
    let meshes = body(template, shape, detail);
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
            for (name, front, _) in junction_turns(template.id(), &preset.shape, Detail::OFFLINE) {
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

/// The live mesh (what viewers see while streaming, with half the rings) holds the ankle to the same turn as the
/// offline one, on the ten presets and the 32 corners of the shape cube: a corner's shin can end outside the foot's dome,
/// where a coarse end cap would cut across it.
#[test]
fn the_live_ankle_turns_no_more_than_the_offline_limit() {
    let limit = FRONT_LIMITS.iter().find(|l| l.0 == "ankle").unwrap().1;
    let shapes: Vec<(String, BodyShape)> = shape::presets()
        .unwrap()
        .iter()
        .map(|p| (p.id.as_str().to_string(), p.shape.clone()))
        .chain(shape_cube_corners())
        .collect();
    for template in registry::templates() {
        for (name, shape) in &shapes {
            let turn =
                junction_turns(template.id(), shape, Detail::LIVE).into_iter().find(|j| j.0 == "ankle").unwrap().1;
            assert!(turn <= limit, "{} {name}: the live ankle turns {turn:.1} degrees (limit {limit})", template.id());
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
            for (k, (_, front, side)) in
                junction_turns(template.id(), &preset.shape, Detail::OFFLINE).into_iter().enumerate()
            {
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
/// of its x a little above and below (1 for a straight or convex run, less for a waist), on the `detail` mesh.
fn ankle_dip(template: &str, shape: &BodyShape, detail: Detail) -> f64 {
    let t = registry::find(template).unwrap();
    let layout = t.layout(&Proportions::default(), shape, &HandParams::default());
    let unit = layout.head_base().unwrap().y / 0.56;
    let meshes = body(template, shape, detail);
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
            for (detail, name) in [(Detail::OFFLINE, "offline"), (Detail::LIVE, "live")] {
                let dip = 1.0 - ankle_dip(template.id(), &preset.shape, detail);
                assert!(
                    dip <= MAX_ANKLE_DIP,
                    "{} {}: the {name} foot dome has a waist of {dip:.3}",
                    template.id(),
                    preset.id.as_str()
                );
            }
        }
    }
}

/// The 32 corners of the shape cube, `corner0` to `corner31`: bit 0 sets the height, 1 the build, 2 the curves, 3 the
/// musculature and 4 the shoulder-hip, each at its high end (+1) when set and its low end (-1) when clear.
fn shape_cube_corners() -> Vec<(String, BodyShape)> {
    (0..32_u32)
        .map(|c| {
            let pick = |bit: u32| if c >> bit & 1 == 1 { 1.0 } else { -1.0 };
            let shape = BodyShape {
                height: pick(0),
                build: pick(1),
                curves: pick(2),
                musculature: pick(3),
                shoulder_hip: pick(4),
                preset: None,
            };
            (format!("corner{c}"), shape)
        })
        .collect()
}

/// The ankle's tail follows the dome down, and the leg's end cap lies past it: neither pokes out of the sole, on every
/// template and on both meshes, over the ten presets and the 32 corners of the shape cube.
#[test]
fn the_leg_stays_above_the_sole() {
    let shapes: Vec<(String, BodyShape)> = shape::presets()
        .unwrap()
        .iter()
        .map(|p| (p.id.as_str().to_string(), p.shape.clone()))
        .chain(shape_cube_corners())
        .collect();
    let mut worst = (f64::MAX, String::new());
    for template in registry::templates() {
        for (name, shape) in &shapes {
            for (detail, label) in [(Detail::LIVE, "live"), (Detail::OFFLINE, "offline")] {
                let lowest = body(template.id(), shape, detail)
                    .iter()
                    .filter(|(r, _)| matches!(r, BodyRegion::LegL | BodyRegion::LegR))
                    .flat_map(|(_, m)| m.geometry.positions.iter().map(|p| f64::from(p.y)))
                    .fold(f64::MAX, f64::min);
                assert!(
                    lowest >= -1e-6,
                    "{} {name} {label}: the leg reaches {lowest} m, below the sole",
                    template.id()
                );
                if lowest < worst.0 {
                    worst = (lowest, format!("{} {name} {label}", template.id()));
                }
            }
        }
    }
    eprintln!("lowest leg vertex {:.4} m ({})", worst.0, worst.1);
}

/// Prints the ankle's worst front turn and waist over the ten presets and the 32 corners of the shape cube, per template.
#[test]
#[ignore = "prints the ankle table"]
fn ankle_table() {
    for template in registry::templates() {
        let shapes: Vec<(String, BodyShape)> = shape::presets()
            .unwrap()
            .iter()
            .map(|p| (p.id.as_str().to_string(), p.shape.clone()))
            .chain(shape_cube_corners())
            .collect();
        for (detail, label) in [(Detail::OFFLINE, "offline"), (Detail::LIVE, "live")] {
            let mut worst = vec![(0.0_f64, String::new()); 2];
            for (name, shape) in &shapes {
                let turn = junction_turns(template.id(), shape, detail).into_iter().find(|j| j.0 == "ankle").unwrap().1;
                let dip = 1.0 - ankle_dip(template.id(), shape, detail);
                for (w, v) in worst.iter_mut().zip([turn, dip]) {
                    if v > w.0 {
                        *w = (v, name.clone());
                    }
                }
            }
            let presets: Vec<String> = shape::presets()
                .unwrap()
                .iter()
                .map(|p| {
                    let turn = junction_turns(template.id(), &p.shape, detail).into_iter().find(|j| j.0 == "ankle");
                    format!("{} {:.1}", p.id.as_str(), turn.map_or(0.0, |j| j.1))
                })
                .collect();
            println!("{:8} {label:7} presets: {}", template.id(), presets.join(", "));
            println!(
                "{:8} {label:7} turn {:.1} ({})  dip {:.3} ({})",
                template.id(),
                worst[0].0,
                worst[0].1,
                worst[1].0,
                worst[1].1
            );
        }
    }
}

/// The mesh twin of `shoulder_rise` in `tests/junctions.rs`, read from the `detail` mesh: how far the front-view top of
/// the torso and the left arm climbs again after the outline has started down from the neck, as a fraction of the
/// torso's greatest half-width. The layout's rise is the reference. The rise is reported here and asserted by #799,
/// which fixes the mesher's torso corner; until then the mesh shows no climb and this reads (about) zero.
fn mesh_shoulder_rise(template: &str, shape: &BodyShape, detail: Detail) -> f64 {
    let layout = registry::find(template).unwrap().layout(&Proportions::default(), shape, &HandParams::default());
    let unit = layout.head_base().unwrap().y / 0.56;
    let r =
        (0..=20).map(|k| layout.section64(BodyRegion::Torso, f64::from(k) / 20.0).unwrap().radii.x).fold(0.0, f64::max);
    // From the neck's base outward: the torso's top is flat to there.
    let neck = layout.section64(BodyRegion::Torso, 1.0).unwrap().radii.x;
    let meshes = body(template, shape, detail);
    let chosen: Vec<&PartMesh> =
        meshes.iter().filter(|(k, _)| matches!(k, BodyRegion::Torso | BodyRegion::ArmL)).map(|(_, m)| m).collect();
    let far = chosen.iter().flat_map(|m| &m.geometry.positions).map(|p| f64::from(p.x)).fold(f64::MIN, f64::max);
    rise_of(&top_edge(&chosen, neck, far, 0.000_25 * unit)) / r
}

/// The climb of a top edge sampled by [`top_edge`]: the most any sample rises above the lowest sample before it. NaN
/// samples, where the meshes have nothing, are skipped.
fn rise_of(tops: &[f64]) -> f64 {
    let (mut lowest, mut rise) = (f64::MAX, 0.0_f64);
    for top in tops.iter().copied().filter(|t| !t.is_nan()) {
        lowest = lowest.min(top);
        rise = rise.max(top - lowest);
    }
    rise
}

/// A flat quad over `x0..x1` at height `0..top`, as a mesh for the rise tests: its top edge is what [`top_edge`] reads.
fn quad(x0: f32, x1: f32, top: f32) -> PartMesh {
    let positions =
        vec![Vec3::new(x0, 0.0, 0.0), Vec3::new(x1, 0.0, 0.0), Vec3::new(x1, top, 0.0), Vec3::new(x0, top, 0.0)];
    PartMesh {
        name: "quad".to_owned(),
        geometry: MeshData { positions, indices: vec![0, 1, 2, 0, 2, 3], ..MeshData::default() },
        joint_bones: Vec::new(),
        material: MaterialName::new("body-skin").unwrap(),
        casts_shadow: false,
        render_order: 0,
        dynamic: false,
    }
}

#[test]
fn a_step_up_in_the_top_edge_is_the_rise_and_a_flat_top_has_none() {
    let (slab, block) = (quad(0.0, 1.0, 1.0), quad(1.0, 2.0, 1.1));
    let step = 0.01;
    let rise = rise_of(&top_edge(&[&slab, &block], 0.0, 2.0, step));
    assert!((rise - 0.1).abs() <= step, "a step of 0.1 reads as {rise}");
    assert!(rise_of(&top_edge(&[&slab], 0.0, 2.0, step)).abs() < 1e-12);
}

/// The largest mesh shoulder rise over `shapes` and the name of the shape it is on (0 and "" for none).
fn worst_rise<'a>(template: &str, detail: Detail, shapes: &'a [(String, BodyShape)]) -> (f64, &'a str) {
    shapes
        .iter()
        .map(|(name, s)| (mesh_shoulder_rise(template, s, detail), name.as_str()))
        .fold((0.0_f64, ""), |w, v| if v.0 > w.0 { v } else { w })
}

/// Prints each template's mesh shoulder rise as a percentage of its torso's half-width, offline then live: on its own
/// shape, the worst over the ten presets and which one, and the worst over the 32 corners of the shape cube and which.
#[test]
#[ignore = "prints the shoulder table"]
fn shoulder_table() {
    for template in registry::templates() {
        let id = template.id();
        let presets: Vec<(String, BodyShape)> =
            shape::presets().unwrap().iter().map(|p| (p.id.as_str().to_string(), p.shape.clone())).collect();
        let corners = shape_cube_corners();
        for (detail, label) in [(Detail::OFFLINE, "offline"), (Detail::LIVE, "live")] {
            let own = mesh_shoulder_rise(id, &BodyShape::default(), detail);
            let (preset, corner) = (worst_rise(id, detail, &presets), worst_rise(id, detail, &corners));
            println!(
                "{:8} {label:7} own {:.1}%  presets {:.1}% {}  corners {:.1}% {}",
                id,
                own * 100.0,
                preset.0 * 100.0,
                preset.1,
                corner.0 * 100.0,
                corner.1
            );
        }
    }
}
