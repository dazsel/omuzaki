//! Human and elf ears: the frame the angles give, the shell's shape, the elf tip, the anchors and cover, and the
//! ears through the real head part. Every number is the contract default or plainly synthetic; the comparison with the
//! prototype's dump is `crates/omuzaki-test/tests/char_head_ears_human_elf_parity.rs`.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::float_cmp)] // exact values that were copied, not computed

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

use omuzaki_char::head::cranium;
use omuzaki_char::head::ears::elf::{self, MAX_TIP, MIN_TIP};
use omuzaki_char::head::ears::human::CLEARANCE;
use omuzaki_char::head::ears::{self, Frame, Side};
use omuzaki_char::head::mesh::{Resolution, build_head};
use omuzaki_char::template::Template;
use omuzaki_char::template::skeleton::TemplateFit;
use omuzaki_char::template::soft_sd::SoftSd;
use omuzaki_char_api::{
    BodyFit, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef,
    EarKind, Ears, FitFact, HeadParams, HeadSurface, JointRef, PartCategory, PartDef, PartId, PartKind, PartKindId,
    PartMeta, PartOutput, Publishes, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::narrow;
use serde_json::{Value, json};

/// The `test-template-body` kind: publishes the soft-sd fit, which is all the head needs of a body.
struct TemplateBody;

impl PartKind for TemplateBody {
    type Params = omuzaki_char_api::BodyParams;

    fn meta() -> PartMeta {
        PartMeta { id: "test-ears-body", summary: "a stand-in body", category: PartCategory::Body, requires: &[] }
    }

    fn build(p: &Self::Params, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let layout = SoftSd.layout(&p.proportions, &p.shape, &p.hand);
        let fit: Arc<dyn BodyFit> = Arc::new(TemplateFit::new(layout));
        Ok(PartOutput { publishes: Publishes::default().with::<FitFact>(fit), ..PartOutput::default() })
    }
}

omuzaki_char_api::register_part_kind!(TemplateBody);

fn pid(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

fn built_with(params: &HeadParams) -> BuiltCharacter {
    let part = |kind: &str, params: Value| PartDef {
        kind: PartKindId::new(kind).unwrap(),
        parent: None,
        enabled: true,
        layer: 0,
        attach: None,
        params,
    };
    let mut def: CharacterDef = mock_character().unwrap();
    def.parts.clear();
    def.parts.insert(pid("body"), part("test-ears-body", json!({})));
    def.parts.insert(pid("head"), part("head", serde_json::to_value(params).unwrap()));
    let id = CharId::new("t").unwrap();
    RegistryBuilder.build(&id, &def, &BuildOptions::default(), &BuildServices::default()).unwrap()
}

/// The default head as built, shared by the tests that do not vary it.
fn default_head() -> &'static Arc<dyn HeadSurface> {
    static HEAD: OnceLock<Arc<dyn HeadSurface>> = OnceLock::new();
    HEAD.get_or_init(|| build_head(&HeadParams::default(), Resolution::LIVE, None).unwrap().surface)
}

/// `ears::build` against the default head.
fn build(ears: &Ears, side: Side, root: Vec3) -> Result<Option<MeshData>, omuzaki_core::mesh::MeshError> {
    ears::build(ears, side, root, default_head().as_ref())
}

fn ears_of(kind: EarKind, size: f64) -> Ears {
    Ears { kind, size, ..Ears::default() }
}

/// The head as built, and the left ear's root on it.
fn head_and_root(params: &HeadParams) -> (Arc<dyn HeadSurface>, Vec3) {
    let head = build_head(params, Resolution::LIVE, None).unwrap().surface;
    let root = ears::root(&params.ears, head.as_ref());
    (head, root)
}

fn mirror_x(p: Vec3) -> Vec3 {
    Vec3::new(-p.x, p.y, p.z)
}

#[test]
fn the_default_frame_is_the_prototypes_basis_on_both_sides() {
    // `head.js` 168 to 173, spelled out: the width is `up x normal`, then the height is made square again.
    let basis = |nx: f64| {
        let normal = DVec3::new(nx, 0.05, 0.66).normalize();
        let up = DVec3::new(0.0, 1.0, 0.12).normalize();
        let across = up.cross(normal).normalize();
        (across, normal.cross(across).normalize(), normal)
    };
    for (side, nx) in [(Side::Left, 0.74), (Side::Right, -0.74)] {
        let frame = Frame::of(&Ears::default(), side);
        let (across, up, normal) = basis(nx);
        assert!((frame.across - across).length() < 1e-12, "{side:?} across");
        assert!((frame.up - up).length() < 1e-12, "{side:?} up");
        assert!((frame.normal - normal).length() < 1e-12, "{side:?} normal");
    }
}

#[test]
fn the_frame_is_orthonormal_and_right_handed_for_any_angles() {
    for (flare, tilt) in [(0.0, 0.0), (42.0, 7.0), (90.0, -20.0), (-30.0, 45.0)] {
        for side in Side::BOTH {
            let f = Frame::of(&Ears { flare_deg: flare, tilt_deg: tilt, ..Ears::default() }, side);
            assert!((f.across.length() - 1.0).abs() < 1e-12 && (f.up.length() - 1.0).abs() < 1e-12);
            assert!(f.across.dot(f.up).abs() < 1e-12 && f.across.dot(f.normal).abs() < 1e-12);
            assert!((f.across.cross(f.up) - f.normal).length() < 1e-12, "right-handed");
        }
    }
}

#[test]
fn flare_turns_the_normal_about_the_vertical_and_tilt_leans_the_height() {
    let azimuth = |ears: &Ears| {
        let n = Frame::of(ears, Side::Left).normal;
        n.z.atan2(n.x).to_degrees()
    };
    let base = azimuth(&Ears::default());
    let flared = azimuth(&Ears { flare_deg: 52.0, ..Ears::default() });
    assert!((flared - base - 10.0).abs() < 1e-9, "10 degrees more flare turns the normal 10 degrees to the front");
    let lean = |tilt: f64| {
        let f = Frame::of(&Ears { tilt_deg: tilt, ..Ears::default() }, Side::Left);
        // The height is squared to the normal, so lean is read from the rotation it was given: its angle to the vertical
        // changes with the tilt in the prototype's sense.
        f.up.angle_between(DVec3::Y).to_degrees()
    };
    assert!(lean(17.0) > lean(7.0) && lean(7.0) > lean(-3.0));
    // The right ear flares the other way: a mirror image of the left's normal.
    let (l, r) = (Frame::of(&Ears::default(), Side::Left), Frame::of(&Ears::default(), Side::Right));
    assert_eq!((l.normal.x, l.normal.y, l.normal.z), (-r.normal.x, r.normal.y, r.normal.z));
}

#[test]
fn a_human_ear_is_the_prototypes_grid_at_every_size() {
    let params = HeadParams::default();
    // Clear of the head (as the prototype's own root is), so nothing is pushed out of it and only the size acts.
    let root = head_and_root(&params).1 + Vec3::X * 0.5;
    let unit = build(&ears_of(EarKind::Human, 1.0), Side::Left, root).unwrap().expect("a human ear");
    assert_eq!((unit.positions.len(), unit.triangle_count()), (369, 640));
    assert!(unit.normals.iter().all(|n| (n.length() - 1.0).abs() < 1e-4), "every vertex has a unit normal");
    for size in [0.2, 0.5, 1.0, 2.0, 3.0] {
        let ear = build(&ears_of(EarKind::Human, size), Side::Left, root).unwrap().expect("an ear");
        ear.validate().unwrap();
        assert_eq!((ear.positions.len(), ear.indices.as_slice()), (369, unit.indices.as_slice()), "size {size}");
        // The shell grows about its root: every vertex is `size` times as far from it.
        for (a, b) in ear.positions.iter().zip(&unit.positions) {
            assert!(((*a - root).length() - (*b - root).length() * narrow(size)).abs() < 1e-5, "size {size}");
        }
    }
}

/// Welds vertices within 1 micron of each other (the seam and the concha's centre repeat a point) and returns the
/// welded id of each vertex.
fn weld(positions: &[Vec3]) -> Vec<usize> {
    let mut reps: Vec<usize> = Vec::new();
    positions
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let found = reps.iter().find(|&&r| (positions[r] - *p).length() < 1e-6).copied();
            found.unwrap_or_else(|| {
                reps.push(i);
                i
            })
        })
        .collect()
}

/// The ear is a thin sheet (its material is double-sided), so it is watertight when it has no hole but the rim: after
/// welding, every interior edge has two triangles, the open edges are one closed loop (the rim) and no directed edge
/// is used twice.
#[test]
fn the_welded_ear_shell_is_a_sheet_with_no_hole_but_its_rim() {
    let params = HeadParams::default();
    let (_, root) = head_and_root(&params);
    for kind in [EarKind::Human, EarKind::Elf] {
        for size in [0.2, 0.6, 1.0, 1.5, 3.0] {
            for (flare, tilt) in [(42.0, 7.0), (0.0, 0.0), (90.0, -30.0)] {
                let ears = Ears { kind, size, flare_deg: flare, tilt_deg: tilt, ..Ears::default() };
                for side in Side::BOTH {
                    let root = if side == Side::Left { root } else { mirror_x(root) };
                    let what = format!("{kind:?} size {size} flare {flare} tilt {tilt} {side:?}");
                    let ear = build(&ears, side, root).unwrap().expect("an ear");
                    let welded = weld(&ear.positions);
                    let mut directed: HashMap<(usize, usize), u32> = HashMap::new();
                    for tri in ear.indices.as_chunks::<3>().0 {
                        let w = tri.map(|i| welded[i as usize]);
                        if w[0] == w[1] || w[1] == w[2] || w[2] == w[0] {
                            continue; // the collapsed cells at the concha's centre
                        }
                        for (a, b) in [(w[0], w[1]), (w[1], w[2]), (w[2], w[0])] {
                            *directed.entry((a, b)).or_default() += 1;
                        }
                    }
                    // (3) consistent orientation.
                    let twice: Vec<_> = directed.iter().filter(|(_, n)| **n > 1).collect();
                    assert!(
                        twice.is_empty(),
                        "{what}: directed edges used twice, first {:?}",
                        &twice[..twice.len().min(5)]
                    );
                    // (1) every undirected edge has one or two triangles; the single ones are the open edges.
                    let mut open: Vec<(usize, usize)> = Vec::new();
                    for &(a, b) in directed.keys() {
                        if !directed.contains_key(&(b, a)) {
                            open.push((a, b));
                        }
                    }
                    open.sort_unstable();
                    // (2) the open edges are exactly one closed loop: every vertex on them has one edge in and one
                    // out, and walking from any of them returns after visiting them all.
                    let next: HashMap<usize, usize> = open.iter().copied().collect();
                    assert_eq!(
                        next.len(),
                        open.len(),
                        "{what}: a vertex starts two open edges, first {:?}",
                        &open[..open.len().min(5)]
                    );
                    let heads: HashSet<usize> = open.iter().map(|e| e.1).collect();
                    assert_eq!(heads.len(), open.len(), "{what}: a vertex ends two open edges");
                    let start = open[0].0;
                    let (mut at, mut steps) = (start, 0);
                    loop {
                        at = next[&at];
                        steps += 1;
                        if at == start || steps > open.len() {
                            break;
                        }
                    }
                    assert!(
                        at == start && steps == open.len(),
                        "{what}: the open edges are not one loop ({steps} of {})",
                        open.len()
                    );
                    // The rim is the grid's last row, once round: 40 edges for either kind.
                    assert_eq!(open.len(), 40, "{what}: rim edges");
                }
            }
        }
    }
}

#[test]
fn the_right_ear_is_the_left_ears_mirror_image() {
    let params = HeadParams::default();
    let (_, root) = head_and_root(&params);
    for kind in [EarKind::Human, EarKind::Elf] {
        let ears = ears_of(kind, 1.3);
        let left = build(&ears, Side::Left, root).unwrap().unwrap();
        let right = build(&ears, Side::Right, mirror_x(root)).unwrap().unwrap();
        // The outline is symmetric across the ear's width, so column `i` of one is column `20 - i` of the other.
        let row = 9;
        for i in 0..=40_usize {
            let j = (20 + 40 - i) % 40;
            for k in 0..row {
                let (a, b) = (left.positions[i * row + k], right.positions[j * row + k]);
                assert!((mirror_x(a) - b).length() < 2e-6, "{kind:?} column {i} row {k}");
            }
        }
    }
}

#[test]
fn no_ear_and_animal_ears_build_nothing_and_publish_nothing() {
    let root = Vec3::new(0.18, 0.7, 0.0);
    for kind in [EarKind::None, EarKind::Animal] {
        let ears = ears_of(kind, 1.0);
        for side in Side::BOTH {
            assert!(build(&ears, side, root).unwrap().is_none(), "{kind:?}");
            assert!(ears::anchor(&ears, side, root).is_none() && ears::cover(&ears, side, root).is_none());
        }
        let built = built_with(&HeadParams { ears, ..HeadParams::default() });
        assert!(built.meshes.iter().all(|m| !m.mesh.name.contains("ear")), "{kind:?}");
        assert!(built.anchors.keys().all(|(_, a)| !a.as_str().starts_with("ear")), "{kind:?}");
    }
}

#[test]
fn the_elf_tip_is_the_highest_point_clear_of_the_head_and_above_the_lobe() {
    for size in [0.2, 0.6, 1.0, 1.5, 2.0, 3.0] {
        let params = HeadParams { ears: ears_of(EarKind::Elf, size), ..HeadParams::default() };
        let (head, root) = head_and_root(&params);
        for side in Side::BOTH {
            let root = if side == Side::Left { root } else { mirror_x(root) };
            let ear = build(&params.ears, side, root).unwrap().unwrap();
            let tip = ear.positions.iter().copied().max_by(|a, b| a.y.total_cmp(&b.y)).unwrap();
            let lobe = ear.positions.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
            let from_centre = tip - head.centre();
            assert!(
                from_centre.length() > head.radius(from_centre.normalize()) + 0.001,
                "size {size}: tip in the head {} vs {}",
                from_centre.length(),
                head.radius(from_centre.normalize())
            );
            assert!(tip.y > lobe + 0.05 * narrow(size), "size {size}: tip {} above lobe {lobe}", tip.y);
            // It is above the human ear's top by the tip's length (less the lean), and not more (the tip also leans out and back, so it rises by less than its length).
            let human = build(&ears_of(EarKind::Human, size), side, root).unwrap().unwrap();
            let rise = tip.y - human.positions.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
            let length = narrow(elf::tip_length(size));
            assert!(rise > 0.5 * length && rise < 1.05 * length, "size {size}: rise {rise} for a {length} tip");
        }
    }
}

#[test]
fn the_tip_length_follows_size_between_its_limits() {
    assert_eq!(elf::tip_length(0.2), MIN_TIP);
    assert_eq!(elf::tip_length(3.0), MAX_TIP);
    assert!((elf::tip_length(1.0) - 0.09).abs() < 1e-12);
    assert!(elf::tip_length(1.2) > elf::tip_length(1.0));
}

#[test]
fn the_elf_shell_is_the_human_shell_away_from_the_tip() {
    let params = HeadParams::default();
    let (_, root) = head_and_root(&params);
    let human = build(&ears_of(EarKind::Human, 1.0), Side::Left, root).unwrap().unwrap();
    let elf = build(&ears_of(EarKind::Elf, 1.0), Side::Left, root).unwrap().unwrap();
    assert_eq!(human.indices, elf.indices);
    let moved: Vec<_> =
        (0..human.positions.len()).filter(|&i| (human.positions[i] - elf.positions[i]).length() > 1e-6).collect();
    // Only the columns near the top (a = pi / 2 is column 10) and only the rows towards the rim move.
    assert!(!moved.is_empty() && moved.len() < 369 / 3, "{} vertices moved", moved.len());
    assert!(moved.iter().all(|i| (i / 9).abs_diff(10) <= 5 && i % 9 > 0));
}

#[test]
fn the_ear_sits_on_the_built_head_where_it_meets_it() {
    let params = HeadParams::default();
    let (head, root) = head_and_root(&params);
    let ear = build(&params.ears, Side::Left, root).unwrap().unwrap();
    // The concha's centre is the first vertex of every column: a few millimetres in front of the root's plane.
    let centre = ear.positions[0];
    let dir = (centre - head.centre()).normalize();
    assert!((centre - head.centre()).length() - head.radius(dir) < 0.006, "the concha floats off the head");
    // The cranium would have left a gap of up to a centimetre at the root.
    let on_cranium = cranium::surface(&params, None).point((root - head.centre()).normalize(), 0.0);
    assert!((on_cranium - head.centre()).length() > (root - head.centre()).length() + 0.004);
}

#[test]
fn the_anchor_is_at_the_root_turned_by_the_frame_and_the_cover_is_the_ear_s_disc() {
    let params = HeadParams::default();
    let (_, root) = head_and_root(&params);
    for side in Side::BOTH {
        let root = if side == Side::Left { root } else { mirror_x(root) };
        let anchor = ears::anchor(&params.ears, side, root).unwrap();
        let frame = Frame::of(&params.ears, side);
        assert_eq!((anchor.bone.clone(), anchor.position), (JointRef::HEAD, root));
        assert!((anchor.rotation * Vec3::Z - frame.normal.as_vec3()).length() < 1e-5);
        assert!((anchor.rotation * Vec3::Y - frame.up.as_vec3()).length() < 1e-5);
        let cover = ears::cover(&params.ears, side, root).unwrap();
        assert_eq!((cover.centre, cover.radius), (root, 0.043));
        assert!((cover.normal - frame.normal.as_vec3()).length() < 1e-6);
    }
    let big = ears::cover(&ears_of(EarKind::Elf, 2.0), Side::Left, root).unwrap();
    assert!((big.radius - 0.086).abs() < 1e-6);
}

#[test]
fn the_head_part_builds_two_rigid_ears_and_publishes_their_anchors() {
    let params = HeadParams::default();
    let built = built_with(&params);
    let names: Vec<_> = built.meshes.iter().map(|m| m.mesh.name.as_str()).collect();
    assert_eq!(names, ["head-head", "head-neck", "head-ear-l", "head-ear-r"]);
    for n in ["head-ear-l", "head-ear-r"] {
        let ear = built.meshes.iter().find(|m| m.mesh.name == n).expect("an ear mesh");
        assert_eq!(ear.mesh.joint_bones, [JointRef::HEAD]);
        assert!(ear.mesh.geometry.weights.iter().all(|w| *w == [1.0, 0.0, 0.0, 0.0]));
        assert!(ear.mesh.geometry.joints.iter().all(|j| *j == [0; 4]));
    }
    let l = &built.anchors[&(pid("head"), "ear_l".parse().unwrap())];
    let r = &built.anchors[&(pid("head"), "ear_r".parse().unwrap())];
    assert_eq!(l.bone, JointRef::HEAD);
    assert!((l.position - mirror_x(r.position)).length() < 1e-6 && l.position.x > 0.0);
    // The anchors sit next to `top`, on the lowered head.
    let top = &built.anchors[&(pid("head"), "top".parse().unwrap())];
    assert!(l.position.y < top.position.y && l.position.y > top.position.y - 0.4);
}

#[test]
fn the_ears_follow_the_necks_drop() {
    let at = |drop: f64| {
        let mut p = HeadParams::default();
        p.neck.drop = drop;
        built_with(&p).anchors[&(pid("head"), "ear_l".parse().unwrap())].position
    };
    let (raised, lowered) = (at(0.0), at(0.03));
    assert!((raised.y - lowered.y - 0.03).abs() < 2e-3, "the ear moves with the head: {raised:?} {lowered:?}");
}

#[test]
fn a_size_that_is_not_finite_is_an_error_not_a_broken_mesh() {
    let mut p = HeadParams::default();
    p.ears.size = f64::NAN;
    assert!(build(&p.ears, Side::Left, Vec3::ZERO).is_err());
}

/// How far outside the head's surface the least clear vertex of the `side` ear of `params` stands, metres (negative
/// inside), measured along the vertex's direction from the head centre, as `HeadSurface::radius` defines it.
fn least_clearance(params: &HeadParams, side: Side) -> f64 {
    let head = build_head(params, Resolution::LIVE, None).unwrap().surface;
    let root = ears::root(&params.ears, head.as_ref());
    let root = if side == Side::Left { root } else { mirror_x(root) };
    let ear = ears::build(&params.ears, side, root, head.as_ref()).unwrap().expect("an ear");
    ear.positions
        .iter()
        .map(|p| {
            let d = *p - head.centre();
            f64::from(d.length() - head.radius(d.normalize()))
        })
        .fold(f64::INFINITY, f64::min)
}

/// The slack the bound gets: `f32` positions at the head's scale round to a few 1e-8 m.
const F32_SLACK: f64 = 1e-6;

fn head_with(ears: Ears, half_width: f64) -> HeadParams {
    let mut params = HeadParams { ears, ..HeadParams::default() };
    params.cranium.half_width = half_width;
    params
}

#[test]
fn the_reviewed_worst_case_clears_the_head_by_the_full_clearance() {
    // Codex's case: a small elf ear at no flare had its tip 17.1 mm inside the head.
    let ears = Ears { kind: EarKind::Elf, size: 0.2, flare_deg: 0.0, ..Ears::default() };
    let least = least_clearance(&head_with(ears, 0.186), Side::Left);
    assert!(least >= CLEARANCE - F32_SLACK, "{least} m");
}

#[test]
fn every_vertex_clears_the_head_at_the_extremes_of_size_shape_and_orientation() {
    let mut least = f64::INFINITY;
    for kind in [EarKind::Human, EarKind::Elf] {
        for size in [0.2, 1.0, 3.0] {
            for flare in [-10.0, 0.0, 42.0, 90.0] {
                for tilt in [-30.0, 7.0, 45.0] {
                    for position in [[0.15, -0.12, -0.06], [0.2, 0.05, 0.04], [0.184, -0.066, -0.012]] {
                        for half_width in [0.15, 0.186, 0.22] {
                            let ears = Ears { kind, size, position, flare_deg: flare, tilt_deg: tilt };
                            for side in Side::BOTH {
                                least = least.min(least_clearance(&head_with(ears.clone(), half_width), side));
                            }
                        }
                    }
                }
            }
        }
    }
    eprintln!("least clearance over the extremes: {:.4} mm", least * 1000.0);
    assert!(least >= CLEARANCE - F32_SLACK, "{least} m");
}

proptest::proptest! {
    /// Over the whole parameter range no vertex of either ear kind, on either side, is nearer the head than the clearance.
    #[test]
    fn no_ear_vertex_is_within_the_clearance_of_the_head(
        elf in proptest::bool::ANY,
        left in proptest::bool::ANY,
        size in 0.2_f64..=3.0,
        flare in -10.0_f64..=90.0,
        tilt in -30.0_f64..=45.0,
        x in 0.15_f64..=0.20,
        y in -0.12_f64..=0.05,
        z in -0.06_f64..=0.04,
        half_width in 0.15_f64..=0.22,
    ) {
        let ears = Ears {
            kind: if elf { EarKind::Elf } else { EarKind::Human },
            size, position: [x, y, z], flare_deg: flare, tilt_deg: tilt,
        };
        let least = least_clearance(&head_with(ears, half_width), if left { Side::Left } else { Side::Right });
        proptest::prop_assert!(least >= CLEARANCE - F32_SLACK, "{least} m from the head");
    }
}
