//! The mitten and simple hands (CB-02): watertight at every size and `HandParams` extreme, within the triangle budget,
//! skinned to valid normalised weights, mirrored exactly, deterministic.
//!
//! Parity with the prototype's hands is in `crates/omuzaki-test-char/tests/char_hands_mitten_simple_parity.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // fixtures that must exist
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // welding quantises positions; counts are small
#![allow(clippy::chunks_exact_to_as_chunks)] // triangles as slices of 3

use std::collections::HashMap;

use omuzaki_char::body::hands;
use omuzaki_char::template::registry;
use omuzaki_char_api::{BodyParams, HandParams, HandStyle, MaterialName, PartMesh, Proportions};
use omuzaki_core::glam::Vec3;

/// A hand's triangle budget at Live quality (the issue's 1.2k; the hand's detail is one fixed level).
const BUDGET: usize = 1200;

fn material() -> MaterialName {
    MaterialName::new("body-skin").unwrap()
}

fn params(hands: HandStyle, hand: HandParams, hand_size: f64) -> BodyParams {
    BodyParams {
        hands,
        hand,
        proportions: Proportions { hand_size, ..Proportions::default() },
        ..BodyParams::default()
    }
}

fn build(template: &str, p: &BodyParams) -> Vec<PartMesh> {
    let layout = registry::find(template).unwrap().try_layout(&p.proportions, &p.shape, &p.hand).unwrap();
    hands::build(p, &layout, &material()).unwrap()
}

fn mesh<'a>(parts: &'a [PartMesh], name: &str) -> &'a PartMesh {
    parts.iter().find(|m| m.name == name).unwrap_or_else(|| panic!("no mesh `{name}`"))
}

/// Triangles with their welded vertex ids, the zero-area ones (a collapsed cap row) dropped.
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
    let tris = m.geometry.indices.chunks_exact(3).map(|t| [id[t[0] as usize], id[t[1] as usize], id[t[2] as usize]]);
    tris.filter(|t| t[0] != t[1] && t[1] != t[2] && t[0] != t[2]).collect()
}

/// The segments round the block's ring and the rings in the block, per style (`hands::mitten` and `hands::simple`).
fn block_vertices(style: HandStyle) -> (usize, usize) {
    let around = if style == HandStyle::Mitten { 24 } else { 30 };
    (around + 1, 12 * (around + 1))
}

/// Every directed edge once, and its reverse once, but for the two open ends the prototype leaves: the wrist ring (a
/// cuff hides it) and the thumb's root ring (inside the palm). Closed and consistently wound everywhere else.
fn assert_watertight(label: &str, m: &PartMesh, style: HandStyle) {
    let (ring, block) = block_vertices(style);
    let mut ids: HashMap<[i64; 3], usize> = HashMap::new();
    let q = |p: Vec3| [p.x, p.y, p.z].map(|v| (f64::from(v) * 1e6).round() as i64);
    let mut open = std::collections::HashSet::new();
    for (k, p) in m.geometry.positions.iter().enumerate() {
        let next = ids.len();
        let id = *ids.entry(q(*p)).or_insert(next);
        if k < ring || (block..block + 17).contains(&k) {
            open.insert(id);
        }
    }
    let mut edges: HashMap<(usize, usize), u32> = HashMap::new();
    for t in welded(m) {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            *edges.entry((a, b)).or_default() += 1;
        }
    }
    for (&(a, b), &n) in &edges {
        assert_eq!(n, 1, "{label}: edge {a}-{b} is used {n} times in one direction");
        assert!(
            edges.contains_key(&(b, a)) || (open.contains(&a) && open.contains(&b)),
            "{label}: edge {a}-{b} is a hole"
        );
    }
}

/// Winding agrees with the vertex normals, and the normals point away from the mesh's centre for most vertices (a thumb makes the shape non-convex, so not all)
/// (the ends are open, so a signed volume would depend on the origin).
fn assert_outward(label: &str, m: &PartMesh) {
    let (p, n) = (&m.geometry.positions, &m.geometry.normals);
    for t in m.geometry.indices.chunks_exact(3).map(|t| [t[0] as usize, t[1] as usize, t[2] as usize]) {
        let face = (p[t[1]] - p[t[0]]).cross(p[t[2]] - p[t[0]]);
        let mean = n[t[0]] + n[t[1]] + n[t[2]];
        assert!(face.dot(mean) >= 0.0, "{label}: a triangle is wound against its normals");
    }
    let centre = p.iter().copied().sum::<Vec3>() / p.len() as f32;
    let outward = p.iter().zip(n).filter(|(p, n)| n.dot(**p - centre) > 0.0).count();
    assert!(outward * 4 >= p.len() * 3, "{label}: only {outward} of {} normals point outward", p.len());
}

/// `HandParams` with one number at `v`, and all of them at `lo` and at `hi`.
fn extremes() -> Vec<HandParams> {
    let mut all = Vec::new();
    for (lo, spread) in [(0.5, -30.0), (2.0, 30.0)] {
        all.push(HandParams {
            palm_width: lo,
            palm_length: lo,
            finger_length: lo,
            finger_thickness: lo,
            thumb_length: lo,
            thumb_spread_deg: spread,
            taper: lo,
        });
    }
    for v in [0.5, 2.0] {
        let d = HandParams::default;
        all.push(HandParams { palm_width: v, ..d() });
        all.push(HandParams { palm_length: v, ..d() });
        all.push(HandParams { thumb_length: v, ..d() });
    }
    for spread in [-30.0, 30.0] {
        all.push(HandParams { thumb_spread_deg: spread, ..HandParams::default() });
    }
    all
}

#[test]
fn hands_are_closed_but_for_their_hidden_ends_at_hand_sizes_and_every_param_extreme() {
    for template in ["soft-sd", "standard"] {
        for style in [HandStyle::Mitten, HandStyle::Simple] {
            for size in [0.5, 1.0, 2.0] {
                for hand in extremes() {
                    let parts = build(template, &params(style, hand.clone(), size));
                    for side in ["l", "r"] {
                        let m = mesh(&parts, &format!("hand-{side}"));
                        let label = format!("{template} {style:?} size {size} {hand:?} {side}");
                        assert_watertight(&label, m, style);
                        assert_outward(&label, m);
                    }
                }
            }
        }
    }
}

#[test]
fn a_hand_stays_within_the_triangle_budget_and_has_its_decals() {
    for (style, meshes, lines) in [(HandStyle::Mitten, 4, true), (HandStyle::Simple, 2, false)] {
        let parts = build("soft-sd", &params(style, HandParams::default(), 1.0));
        assert_eq!(parts.len(), meshes, "{style:?}");
        let tris = |side: &str| {
            parts.iter().filter(|m| m.name.ends_with(side)).map(|m| m.geometry.triangle_count()).sum::<usize>()
        };
        for side in ["-l", "-r"] {
            eprintln!("{style:?} {side}: {} triangles", tris(side));
            assert!(tris(side) <= BUDGET, "{style:?} {side}: {} triangles", tris(side));
        }
        let names: Vec<&str> = parts.iter().map(|m| m.name.as_str()).collect();
        let want: &[&str] =
            if lines { &["hand-l", "hand-lines-l", "hand-r", "hand-lines-r"] } else { &["hand-l", "hand-r"] };
        assert_eq!(names, want, "{style:?} mesh names");
        if lines {
            // Three tubes of 8 points and 5 segments each.
            assert_eq!(mesh(&parts, "hand-lines-l").geometry.positions.len(), 3 * 8 * 6);
        }
    }
}

#[test]
fn weights_are_valid_and_normalised() {
    for style in [HandStyle::Mitten, HandStyle::Simple] {
        for part in build("soft-sd", &params(style, HandParams::default(), 1.0)) {
            part.validate().unwrap();
            for w in &part.geometry.weights {
                let sum: f32 = w.iter().sum();
                assert!((sum - 1.0).abs() < 1e-3, "{}: weights sum to {sum}", part.name);
                assert!(w.iter().all(|x| (0.0..=1.0).contains(x)), "{}: {w:?}", part.name);
            }
        }
    }
}

#[test]
fn the_hand_bone_carries_the_palm_and_the_finger_bones_the_finger_block() {
    use omuzaki_anim_api::HumanoidBone::{
        LeftHand, LeftIndexProximal, LeftLittleProximal, LeftLowerArm, LeftThumbDistal,
    };
    let parts = build("soft-sd", &params(HandStyle::Mitten, HandParams::default(), 1.0));
    let m = mesh(&parts, "hand-l");
    let weight_of = |bone: omuzaki_anim_api::HumanoidBone| -> f32 {
        let slot = m.joint_bones.iter().position(|j| *j == omuzaki_char_api::JointRef::Humanoid(bone)).unwrap();
        m.geometry
            .joints
            .iter()
            .zip(&m.geometry.weights)
            .map(|(js, ws)| js.iter().zip(ws).filter(|(j, _)| usize::from(**j) == slot).map(|(_, w)| w).sum::<f32>())
            .sum()
    };
    for bone in [LeftHand, LeftLowerArm, LeftIndexProximal, LeftLittleProximal, LeftThumbDistal] {
        assert!(weight_of(bone) > 1.0, "{bone:?} moves no vertex");
    }
    assert!(weight_of(LeftHand) > weight_of(LeftLowerArm), "the hand bone leads at the wrist");
}

#[test]
fn the_right_hand_is_the_exact_mirror_of_the_left() {
    for style in [HandStyle::Mitten, HandStyle::Simple] {
        let parts = build("soft-sd", &params(style, HandParams::default(), 1.0));
        for (l, r) in
            parts.iter().filter(|m| m.name.ends_with("-l")).zip(parts.iter().filter(|m| m.name.ends_with("-r")))
        {
            assert_eq!(l.geometry.positions.len(), r.geometry.positions.len());
            for (a, b) in l.geometry.positions.iter().zip(&r.geometry.positions) {
                assert_eq!(Vec3::new(-a.x, a.y, a.z), *b, "{style:?} {}", l.name);
            }
            assert_eq!(l.joint_bones.len(), r.joint_bones.len());
            assert!(l.joint_bones.iter().zip(&r.joint_bones).all(|(a, b)| a.mirrored() == *b), "{style:?}: bones");
        }
    }
}

#[test]
fn hands_are_deterministic_and_the_styles_differ() {
    let p = |style| params(style, HandParams::default(), 1.0);
    let bytes = |parts: &[PartMesh]| {
        parts
            .iter()
            .flat_map(|m| m.geometry.positions.iter().flat_map(Vec3::to_array))
            .map(f32::to_bits)
            .collect::<Vec<_>>()
    };
    assert_eq!(bytes(&build("soft-sd", &p(HandStyle::Mitten))), bytes(&build("soft-sd", &p(HandStyle::Mitten))));
    assert_ne!(bytes(&build("soft-sd", &p(HandStyle::Mitten))), bytes(&build("soft-sd", &p(HandStyle::Simple))));
}

#[test]
fn hand_size_scales_the_hand_and_five_finger_adds_nothing_here() {
    let extent = |size: f64| {
        let parts = build("soft-sd", &params(HandStyle::Mitten, HandParams::default(), size));
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in &mesh(&parts, "hand-l").geometry.positions {
            (lo, hi) = (lo.min(*p), hi.max(*p));
        }
        (hi - lo).length()
    };
    let ratio = extent(2.0) / extent(1.0);
    assert!((1.5..2.1).contains(&ratio), "double hand size gives {ratio}x");
    assert_eq!(build("soft-sd", &params(HandStyle::FiveFinger, HandParams::default(), 1.0)).len(), 0);
}

#[test]
fn non_finite_numbers_are_clamped_and_the_hand_still_builds() {
    let hand = HandParams { palm_width: f64::NAN, thumb_spread_deg: f64::INFINITY, ..HandParams::default() };
    let parts = build("soft-sd", &params(HandStyle::Simple, hand, f64::NAN));
    assert!(parts.iter().all(|m| m.geometry.positions.iter().all(|p| p.is_finite())));
}

#[test]
fn the_hand_reaches_the_finger_joints_the_layout_places() {
    use omuzaki_anim_api::HumanoidBone::{LeftMiddleDistal, LeftThumbDistal};
    let p = params(HandStyle::Mitten, HandParams::default(), 1.0);
    let layout = registry::find("soft-sd").unwrap().try_layout(&p.proportions, &p.shape, &p.hand).unwrap();
    let parts = hands::build(&p, &layout, &material()).unwrap();
    let hand = mesh(&parts, "hand-l");
    for bone in [LeftMiddleDistal, LeftThumbDistal] {
        let joint = layout.joint64(bone).unwrap().as_vec3();
        let nearest = hand.geometry.positions.iter().map(|v| v.distance(joint)).fold(f32::MAX, f32::min);
        assert!(nearest < 0.012, "{bone:?} is {nearest} m from the skin");
    }
}
