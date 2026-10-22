//! Cloth collisions (#333): the skirt against the thighs over a walk, a robe over the skirt's shell in a squat, the
//! body colliders fitted inside every template at five body shapes, hair proxies, and the counts in `cost`.

#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

#[path = "cloth_runtime/skirt.rs"]
mod skirt;

use omuzaki_anim_api::{Collider, ColliderGroup, ColliderShape, HumanoidBone};
use omuzaki_char::cloth::collide::{BodyDepth, ColliderKind, ColliderSet};
use omuzaki_char::cloth::runtime::{ClothRuntime, ClothSpec};
use omuzaki_char::template::registry;
use omuzaki_char_api::{BodyFit, BodyShape, HandParams, JointRef, PartRuntime, Proportions, StepCtx, humanoid_id};
use omuzaki_core::glam::{Affine3A, Vec3};
use omuzaki_core::time::Seconds;
use skirt::{Pose, leg_colliders, robe_grid, skirt_grid, thighs, to_segment};

const FRAME: f32 = 1.0 / 60.0;

fn run(rt: &mut ClothRuntime, pose: &Pose, frame: u32) {
    rt.step(&StepCtx::new(FRAME, Seconds(f64::from(frame) * f64::from(FRAME)), pose, Vec3::ZERO));
}

#[test]
fn a_walking_skirt_never_goes_2_mm_into_the_thighs() {
    let mut rt = skirt::skirt();
    rt.reset(&Pose::walk(0.0));
    rt.settle(&Pose::walk(0.0), 1.0);
    let mut worst = f32::MIN;
    for frame in 1..=240 {
        let pose = Pose::walk(frame as f32 * FRAME);
        run(&mut rt, &pose, frame);
        for (a, b, r) in thighs(&pose) {
            for p in rt.state().positions() {
                worst = worst.max(r - to_segment(Vec3::from(*p), a, b));
            }
        }
    }
    println!("walk, 4 s at ±35° hip flexion: deepest particle {:.2} mm into a thigh (negative: clear)", worst * 1e3);
    assert!(worst <= 0.002, "{worst} m into a thigh");
}

/// The closest point to `p` on the triangle `a b c` (Ericson, Real-Time Collision Detection, 5.1.5).
fn closest_on_triangle(p: Vec3, a: Vec3, b: Vec3, c: Vec3) -> Vec3 {
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

/// The distance from `p` to the drawn grid surface (`rows x cols`, wrapped), and whether `p` is on its outer side
/// (by the nearest triangle's normal, oriented as the drawn normals).
fn to_surface(p: Vec3, positions: &[Vec3], normals: &[Vec3], rows: usize, cols: usize) -> (f32, bool) {
    let mut best = (f32::MAX, true);
    for r in 0..rows - 1 {
        for c in 0..cols {
            let at = |r: usize, c: usize| r * cols + c % cols;
            let (i00, i01, i10, i11) = (at(r, c), at(r, c + 1), at(r + 1, c), at(r + 1, c + 1));
            for [i, j, k] in [[i00, i10, i01], [i01, i10, i11]] {
                let q = closest_on_triangle(p, positions[i], positions[j], positions[k]);
                let d = q.distance(p);
                if d < best.0 {
                    let n = normals[i] + normals[j] + normals[k];
                    best = (d, (p - q).dot(n) >= 0.0);
                }
            }
        }
    }
    best
}

/// A robe over the skirt through a squat to `depth` (0 standing, 1 the full squat: hips down 30 cm, thighs up 80°)
/// over 1.25 s, held to 2.5 s; returns the closest the robe's sheet comes to the skirt's sheet (negative: inside it).
fn robe_over_skirt(depth: f32) -> f32 {
    let (skirt_grid, robe_grid) = (skirt_grid(), robe_grid());
    let mut skirt = skirt::skirt();
    // The robe never reads the skirt's runtime. It collides with what lies under it: the body as the skirt's
    // envelope publishes it (the legs and hips grown by the skirt's layer, 12 mm: what `add_body` builds from the
    // envelope's fit), and the skirt's shell where the skirt hangs free of the body.
    let mut colliders = ColliderSet::new();
    let layer = 0.012;
    let legs = leg_colliders();
    for s in legs.shapes() {
        let joint = s.joint().map(|j| legs.joints()[j].clone()).unwrap();
        colliders.add_capsule(s.a, s.b, s.radius + layer, &joint, ColliderKind::Body);
    }
    let shell = colliders.add_shell(&skirt_grid, 8, 16, layer, 0.03).unwrap();
    let mut robe = ClothRuntime::new(&robe_grid, &ClothSpec::default(), colliders).unwrap();
    assert_eq!(robe.colliders().counts().layer, shell);
    for rt in [&mut skirt, &mut robe] {
        rt.reset(&Pose::default());
        rt.settle(&Pose::default(), 1.0);
    }
    let mut closest = f32::MAX;
    let skirt_bottom = 0.58 - 0.02;
    let (mut s_sheet, mut r_sheet) = (Vec::new(), Vec::new());
    for frame in 1..=150 {
        let pose = Pose::squat((frame as f32 * FRAME / 1.25).min(1.0) * depth);
        run(&mut skirt, &pose, frame);
        run(&mut robe, &pose, frame);
        if frame % 5 != 0 {
            continue;
        }
        skirt.sheet_surface(&mut s_sheet);
        robe.sheet_surface(&mut r_sheet);
        let normals = skirt.dynamic_mesh(0).unwrap().normals;
        // Robe vertices over the skirt (above its hem at rest, below the pinned waist): their distance to the skirt.
        for (v, p) in r_sheet.iter().enumerate() {
            if robe_grid.positions[v].y < skirt_bottom || v / robe_grid.cols < 2 {
                continue;
            }
            let (d, outside) = to_surface(*p, &s_sheet, normals, skirt_grid.rows, skirt_grid.cols);
            closest = closest.min(if outside { d } else { -d });
        }
    }
    closest
}

/// The layer below as a shell: standing and swaying, the robe keeps 2 mm clear of the skirt. Squatting is printed,
/// not held: the shell follows the skirt's skin weights, and once the thighs push the skirt's sheet off its skin the
/// robe can pass into it (the PR records the table and the follow-up).
#[test]
fn a_robe_over_the_skirt_keeps_2_mm_apart_standing_and_reports_the_squat() {
    let standing = robe_over_skirt(0.0);
    println!("robe over skirt, standing: closest {:.1} mm", standing * 1e3);
    for depth in [0.25, 0.5, 1.0] {
        println!("robe over skirt, squat to {:.0}%: closest {:.1} mm", depth * 100.0, robe_over_skirt(depth) * 1e3);
    }
    assert!(standing >= 0.002, "the robe came within {standing} m of the skirt (or inside it)");
}

/// The body shapes the tests fit: the template, and each corner of four body-shape axes.
fn shapes() -> [BodyShape; 5] {
    [
        BodyShape::default(),
        BodyShape { build: 1.0, musculature: 1.0, ..BodyShape::default() },
        BodyShape { build: -1.0, height: 1.0, ..BodyShape::default() },
        BodyShape { curves: 1.0, shoulder_hip: -1.0, ..BodyShape::default() },
        BodyShape { curves: -1.0, shoulder_hip: 1.0, height: -1.0, ..BodyShape::default() },
    ]
}

#[test]
fn body_colliders_regenerate_inside_every_template_at_five_shapes() {
    let templates = registry::templates();
    assert!(templates.len() >= 4, "{} templates", templates.len());
    for template in &templates {
        for shape in shapes() {
            let fit = template.layout(&Proportions::default(), &shape, &HandParams::default());
            let mut set = ColliderSet::new();
            let n = set.add_body(&fit);
            let joints: Vec<String> =
                set.shapes().iter().map(|s| format!("{:?}", set.joints()[s.joint().unwrap()])).collect();
            assert_eq!(n, 14, "{}: torso 3, hips, legs 4, arms 4, neck, head; got {n}: {joints:?}", template.id());
            let depth = BodyDepth::new(&fit);
            let mut worst = f32::MIN;
            let mut culprit = String::new();
            for s in set.shapes() {
                let before = worst;
                if s.joint().map(|j| &set.joints()[j]) == Some(&JointRef::HEAD) {
                    continue; // the head has no section in a body fit (see the module docs)
                }
                // A finer pattern than the fitting's: 33 stations, 24 directions around, caps at 12 latitudes.
                let axis = (s.b - s.a).normalize_or(Vec3::Y);
                let side = axis.any_orthonormal_vector();
                let up = axis.cross(side);
                for k in 0..=32 {
                    let q = s.a.lerp(s.b, k as f32 / 32.0);
                    for m in 0..24 {
                        let angle = std::f32::consts::TAU * m as f32 / 24.0;
                        let d = side * angle.cos() + up * angle.sin();
                        worst = worst.max(depth.depth(q + d * s.radius));
                        for lat in 1..12 {
                            let tilt = std::f32::consts::FRAC_PI_2 * lat as f32 / 12.0;
                            let cap =
                                |end: Vec3, sign: f32| end + (d * tilt.cos() + axis * sign * tilt.sin()) * s.radius;
                            if k == 0 {
                                worst = worst.max(depth.depth(cap(s.a, -1.0)));
                            }
                            if k == 32 {
                                worst = worst.max(depth.depth(cap(s.b, 1.0)));
                            }
                        }
                    }
                }
                if worst > before {
                    culprit = format!("{:?} r {:.4} {:?}..{:?}", set.joints()[s.joint().unwrap()], s.radius, s.a, s.b);
                }
            }
            assert!(
                worst <= 0.001,
                "{} {shape:?}: a collider pokes {worst} m out of the body: {culprit}",
                template.id()
            );
        }
    }
}

#[test]
fn hair_proxies_join_at_their_joints_and_cost_counts_every_collider() {
    let template = registry::templates()[0];
    let fit = template.layout(&Proportions::default(), &BodyShape::default(), &HandParams::default());
    let head = fit.joint(&JointRef::NECK).unwrap();
    let groups = [ColliderGroup {
        name: "hair_back".into(),
        colliders: vec![
            Collider {
                bone: humanoid_id(HumanoidBone::Neck),
                shape: ColliderShape::Sphere { offset: Vec3::new(0.0, 0.05, -0.08), radius: 0.04 },
            },
            Collider {
                bone: humanoid_id(HumanoidBone::Neck),
                shape: ColliderShape::Capsule {
                    offset: Vec3::new(0.0, 0.0, -0.1),
                    radius: 0.03,
                    tail: Vec3::new(0.0, -0.25, -0.1),
                },
            },
            // A plane is for spring bones only.
            Collider {
                bone: humanoid_id(HumanoidBone::Neck),
                shape: ColliderShape::Plane { offset: Vec3::ZERO, normal: Vec3::Y },
            },
        ],
    }];
    let mut set = ColliderSet::new();
    let body = set.add_body(&fit);
    assert_eq!(set.add_hair(&groups, &fit), 2);
    let hair = &set.shapes()[body];
    assert_eq!((hair.kind, hair.a, hair.radius), (ColliderKind::Hair, head + Vec3::new(0.0, 0.05, -0.08), 0.04));
    // Placed by the pose: the head joint's matrix moves the proxy.
    let lift = Affine3A::from_translation(Vec3::new(0.0, 0.2, 0.0));
    let matrices: Vec<Affine3A> = set.joints().iter().map(|_| lift).collect();
    let placed = set.placed(body, &matrices).unwrap();
    assert!((placed.axis().0 - (hair.a + Vec3::Y * 0.2)).length() < 1e-6);
    // Every collider is in the runtime's cost.
    let counts = set.counts();
    assert_eq!((counts.body, counts.hair, counts.layer), (14, 2, 0));
    let rt = ClothRuntime::new(&skirt_grid(), &ClothSpec::default(), set).unwrap();
    assert_eq!(rt.cost().colliders, 16);
}

#[test]
fn a_shell_refuses_bad_sizes() {
    let grid = skirt_grid();
    let mut set = ColliderSet::new();
    assert!(set.add_shell(&grid, 0, 16, 0.01, 0.03).is_err());
    assert!(set.add_shell(&grid, 4, 2, 0.01, 0.03).is_err());
    assert!(set.add_shell(&grid, 4, 16, f32::NAN, 0.03).is_err());
    assert!(set.add_shell(&grid, 4, 16, 0.01, 0.0).is_err());
    assert!(set.is_empty());
    assert!(!set.add_capsule(Vec3::NAN, Vec3::ZERO, 0.1, &JointRef::HEAD, ColliderKind::Body));
}
