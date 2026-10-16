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

/// Every body template.
const TEMPLATES: [&str; 4] = ["soft-sd", "standard", "mini", "tall"];

/// A hand's triangle budget at Live quality (the hand's detail is one fixed level). #157 asked 1.2k, but the prototype's
/// hand, which is the parity target, is 1480 triangles and its finger hints 210 more; with the wrist sleeve and the
/// thumb's stem a mitten is 1782 (the Director rules on the amendment).
const BUDGET: usize = 1800;

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

/// The vertices of one ring of the thumb: its segments round, and the first vertex repeated.
const THUMB_RING: usize = 19;
/// The prototype's thumb rings and block rows (13 before the cap, 8 in it).
const THUMB_ROWS: usize = 11;
const BLOCK_ROWS: usize = 21;

/// Where the pieces of a hand mesh lie among its vertices (`hands` module docs): the prototype's block, then its thumb,
/// then the two the kit adds, the wrist sleeve (two block rings) and the thumb's stem (two thumb rings).
struct Pieces {
    /// The vertices of one ring of the block.
    ring: usize,
    /// Where the thumb, the sleeve and the stem start.
    thumb: usize,
    sleeve: usize,
    stem: usize,
}

impl Pieces {
    fn of(style: HandStyle) -> Self {
        let ring = if style == HandStyle::Mitten { 28 } else { 30 } + 1;
        let thumb = BLOCK_ROWS * ring;
        let sleeve = thumb + THUMB_ROWS * THUMB_RING;
        Self { ring, thumb, sleeve, stem: sleeve + 2 * ring }
    }

    /// The prototype's block: its rings one after the other.
    fn block<'a>(&self, positions: &'a [Vec3]) -> &'a [Vec3] {
        &positions[..self.thumb]
    }

    /// The thumb's root ring, which sits on the layout's proximal thumb joint.
    fn root<'a>(&self, positions: &'a [Vec3]) -> &'a [Vec3] {
        &positions[self.thumb..self.thumb + THUMB_RING]
    }

    /// The two open rings: the wrist sleeve's (inside the forearm) and the thumb stem's (inside the palm).
    fn wrist_ring(&self) -> std::ops::Range<usize> {
        self.sleeve..self.sleeve + self.ring
    }

    fn stem_ring(&self) -> std::ops::Range<usize> {
        self.stem..self.stem + THUMB_RING
    }
}

/// Every directed edge once, and its reverse once, but for the two open ends: the wrist sleeve's ring (inside the
/// forearm) and the thumb stem's (inside the palm). Closed and consistently wound everywhere else, the pieces joined
/// where their rings meet.
fn assert_watertight(label: &str, m: &PartMesh, style: HandStyle) {
    let pieces = Pieces::of(style);
    let mut ids: HashMap<[i64; 3], usize> = HashMap::new();
    let q = |p: Vec3| [p.x, p.y, p.z].map(|v| (f64::from(v) * 1e6).round() as i64);
    let mut open = std::collections::HashSet::new();
    for (k, p) in m.geometry.positions.iter().enumerate() {
        let next = ids.len();
        let id = *ids.entry(q(*p)).or_insert(next);
        if pieces.wrist_ring().contains(&k) || pieces.stem_ring().contains(&k) {
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
    for template in TEMPLATES {
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

/// The layout of `template` for `p`, and the hands built on it.
fn built(template: &str, p: &BodyParams) -> (omuzaki_char::template::layout::BodyLayout, Vec<PartMesh>) {
    let layout = registry::find(template).unwrap().try_layout(&p.proportions, &p.shape, &p.hand).unwrap();
    let parts = hands::build(p, &layout, &material()).unwrap();
    (layout, parts)
}

/// How much longer a template's hand is than soft-sd's, by the middle finger's distal joint from the wrist.
fn grown(template: &str) -> f32 {
    let reach = |t: &str| {
        let (layout, _) = built(t, &params(HandStyle::Mitten, HandParams::default(), 1.0));
        let at = |b| layout.joint64(b).unwrap();
        at(omuzaki_anim_api::HumanoidBone::LeftMiddleDistal).distance(at(omuzaki_anim_api::HumanoidBone::LeftHand))
    };
    (reach(template) / reach("soft-sd")) as f32
}

#[test]
fn the_hand_reaches_every_finger_and_thumb_joint_the_layout_places_on_every_template() {
    use omuzaki_anim_api::HumanoidBone::{
        LeftIndexDistal, LeftLittleDistal, LeftMiddleDistal, LeftRingDistal, LeftThumbDistal,
    };
    for template in TEMPLATES {
        for size in [0.5, 1.0, 2.0] {
            // A mitten ignores the finger length (the API contract), so longer fingers than the block are not tested.
            for hand in extremes().into_iter().map(|h| HandParams { finger_length: h.finger_length.min(1.0), ..h }) {
                for style in [HandStyle::Mitten, HandStyle::Simple] {
                    let (layout, parts) = built(template, &params(style, hand.clone(), size));
                    let skin = &mesh(&parts, "hand-l").geometry.positions;
                    // A simple hand's thumb is splayed 18 degrees past the layout's, so its thumb joints are not on it.
                    let thumb = (style == HandStyle::Mitten).then_some(LeftThumbDistal);
                    for bone in
                        [LeftIndexDistal, LeftMiddleDistal, LeftRingDistal, LeftLittleDistal].into_iter().chain(thumb)
                    {
                        let joint = layout.joint64(bone).unwrap().as_vec3();
                        let nearest = skin.iter().map(|v| v.distance(joint)).fold(f32::MAX, f32::min);
                        // The skin wraps the joint within the thumb's radius (soft-sd's 12 mm, grown with the template's
                        // hand), where a hand that ignored the template would end 150 mm short on `tall`.
                        let tolerance = 0.012 * size as f32 * grown(template);
                        assert!(
                            nearest < tolerance,
                            "{template} {style:?} size {size} {hand:?}: {bone:?} is {nearest} m from the skin"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_hand_grows_with_the_templates_hand() {
    let reach = |template: &str| {
        let (layout, parts) = built(template, &params(HandStyle::Mitten, HandParams::default(), 1.0));
        let wrist = layout.joint64(omuzaki_anim_api::HumanoidBone::LeftHand).unwrap().as_vec3();
        mesh(&parts, "hand-l").geometry.positions.iter().map(|p| p.distance(wrist)).fold(0.0, f32::max)
    };
    let (small, large) = (reach("soft-sd"), reach("tall"));
    assert!(large > 2.5 * small, "tall's hand reaches {large} m, soft-sd's {small} m");
}

#[test]
fn the_wrist_end_lies_inside_the_forearm_at_every_size_and_template() {
    use omuzaki_anim_api::HumanoidBone::{LeftHand, LeftLowerArm};
    for template in TEMPLATES {
        for size in [0.5, 1.0, 2.0] {
            for hand in extremes() {
                let (layout, parts) = built(template, &params(HandStyle::Mitten, hand.clone(), size));
                let (wrist, elbow) = (layout.joint64(LeftHand).unwrap(), layout.joint64(LeftLowerArm).unwrap());
                let axis = (wrist - elbow).normalize().as_vec3();
                let back = mesh(&parts, "hand-l")
                    .geometry
                    .positions
                    .iter()
                    .map(|p| (*p - wrist.as_vec3()).dot(axis))
                    .fold(f32::MAX, f32::min);
                // The arm ends in a cap that reaches about half a ring's width past the wrist and closes: the hand's
                // opening has to start before the wrist, in the arm's full-width part.
                assert!(back < 0.0, "{template} size {size} {hand:?}: the hand starts {back} m past the wrist");
            }
        }
    }
}

#[test]
fn the_thumb_root_lands_on_the_layouts_joint_whatever_the_palm_and_spread() {
    use omuzaki_anim_api::HumanoidBone::LeftThumbProximal;
    let pieces = Pieces::of(HandStyle::Mitten);
    let centre = |ring: &[Vec3]| ring[..THUMB_RING - 1].iter().copied().sum::<Vec3>() / (THUMB_RING - 1) as f32;
    for template in TEMPLATES {
        // The last two put the joint outside the palm: the stem, not the root ring, is what moves into it.
        for (palm_length, palm_width, thumb_length, spread) in
            [(0.5, 2.0, 2.0, -30.0), (1.0, 1.0, 1.0, 0.0), (2.0, 0.5, 1.0, 30.0), (2.0, 2.0, 0.5, 30.0)]
        {
            let hand =
                HandParams { palm_length, palm_width, thumb_length, thumb_spread_deg: spread, ..Default::default() };
            let (layout, parts) = built(template, &params(HandStyle::Mitten, hand, 1.0));
            let positions = &mesh(&parts, "hand-l").geometry.positions;
            let joint = layout.joint64(LeftThumbProximal).unwrap().as_vec3();
            let error = centre(pieces.root(positions)).distance(joint);
            assert!(
                error < 1e-4,
                "{template} length {palm_length} width {palm_width} spread {spread}: the thumb root is {error} m off"
            );
        }
    }
}

/// The extremes, and the two palm-and-spread combinations a review found the thumb's open ring leaving the palm at.
fn thumb_cases() -> Vec<HandParams> {
    let mut all = extremes();
    for (palm_length, palm_width, thumb_spread_deg) in [(2.0, 0.5, 30.0), (1.0, 0.5, -30.0)] {
        all.push(HandParams { palm_length, palm_width, thumb_spread_deg, ..HandParams::default() });
    }
    all
}

/// Whether `p` lies inside the closed tube made of `triangles` (indices into `positions`), which runs along `axis`: the
/// triangles are cut by the plane through `p` across `axis`, and a ray from `p` within that plane crosses the cut an
/// odd number of times. The cut is the surface's own section at `p`'s place along the axis, whatever its shape.
fn inside_cut(p: Vec3, positions: &[Vec3], triangles: impl Iterator<Item = [u32; 3]>, axis: Vec3) -> bool {
    let e1 = axis.any_orthonormal_vector();
    let e2 = axis.cross(e1);
    let level = |q: Vec3| (q - p).dot(axis);
    let flat = |q: Vec3| ((q - p).dot(e1), (q - p).dot(e2));
    let mut crossings = 0;
    for t in triangles {
        let corners = t.map(|v| positions[v as usize]);
        // Where the triangle's edges cross the plane (a corner on it counts as above): none, or the two ends of the
        // triangle's piece of the cut.
        let cut: Vec<(f32, f32)> = [(0, 1), (1, 2), (2, 0)]
            .into_iter()
            .filter_map(|(a, b)| {
                let (sa, sb) = (level(corners[a]), level(corners[b]));
                ((sa >= 0.0) != (sb >= 0.0)).then(|| flat(corners[a] + (corners[b] - corners[a]) * (sa / (sa - sb))))
            })
            .collect();
        if let [(xa, ya), (xb, yb)] = cut[..] {
            // The ray runs along `e1` from `p`, the origin here.
            if (ya > 0.0) != (yb > 0.0) && xa + (xb - xa) * (ya / (ya - yb)) > 0.0 {
                crossings += 1;
            }
        }
    }
    crossings % 2 == 1
}

/// The triangles of `m`, as index triples.
fn triangles(m: &PartMesh) -> impl Iterator<Item = [u32; 3]> + '_ {
    m.geometry.indices.chunks_exact(3).map(|t| [t[0], t[1], t[2]])
}

/// Whether `p` lies inside the hand's palm-and-finger block: the triangles among its first `pieces.thumb` vertices.
fn inside_block(p: Vec3, m: &PartMesh, pieces: &Pieces, axis: Vec3) -> bool {
    let limit = u32::try_from(pieces.thumb).unwrap();
    inside_cut(p, &m.geometry.positions, triangles(m).filter(|t| t.iter().all(|&v| v < limit)), axis)
}

#[test]
fn every_vertex_of_the_thumbs_open_stem_ring_lies_in_the_palm_at_every_extreme_and_template() {
    use omuzaki_anim_api::HumanoidBone::{LeftHand, LeftLowerArm};
    for template in TEMPLATES {
        for size in [0.5, 1.0, 2.0] {
            for hand in thumb_cases() {
                for style in [HandStyle::Mitten, HandStyle::Simple] {
                    let pieces = Pieces::of(style);
                    let (layout, parts) = built(template, &params(style, hand.clone(), size));
                    let (wrist, elbow) = (layout.joint64(LeftHand).unwrap(), layout.joint64(LeftLowerArm).unwrap());
                    let axis = (wrist - elbow).normalize().as_vec3();
                    let m = mesh(&parts, "hand-l");
                    for &v in &m.geometry.positions[pieces.stem_ring()] {
                        assert!(
                            inside_block(v, m, &pieces, axis),
                            "{template} {style:?} size {size} {hand:?}: a vertex of the thumb's stem ring is outside \
                             the palm's surface"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_palm_test_finds_a_point_outside_the_surface() {
    // The containment test itself: the block's own centre line is inside, and a point pushed out past the widest
    // section is not.
    use omuzaki_anim_api::HumanoidBone::{LeftHand, LeftLowerArm};
    let pieces = Pieces::of(HandStyle::Mitten);
    let (layout, parts) = built("soft-sd", &params(HandStyle::Mitten, HandParams::default(), 1.0));
    let (wrist, elbow) = (layout.joint64(LeftHand).unwrap(), layout.joint64(LeftLowerArm).unwrap());
    let axis = (wrist - elbow).normalize().as_vec3();
    let m = mesh(&parts, "hand-l");
    let ring = &m.geometry.positions[5 * pieces.ring..6 * pieces.ring - 1];
    let centre = ring.iter().copied().sum::<Vec3>() / ring.len() as f32;
    assert!(inside_block(centre, m, &pieces, axis));
    let out = ring[0] + (ring[0] - centre) * 0.05;
    assert!(!inside_block(out, m, &pieces, axis));
    let just_in = centre + (ring[0] - centre) * 0.95;
    assert!(inside_block(just_in, m, &pieces, axis));
}

/// Renders the left hand of the built body (front and side) through the mock toon renderer when
/// `OMUZAKI_HANDS_PPM=<directory>` is set, as binary PPM files: on soft-sd and standard with the thumb opened and
/// lengthened so its placement shows, and on soft-sd at the extreme where the thumb's joint lies outside the palm
/// (palm twice as long and half as wide, thumb spread 30 degrees), so the stem seated in the palm shows. The fingertip
/// tint is the hands' vertex colours.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_hands_when_asked() {
    use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, RegistryBuilder};
    use omuzaki_core::colour::Colour;
    use omuzaki_core::glam::{Mat4, UVec2};
    use omuzaki_render_api::{
        AlphaEncoding, Background, Camera, Instance, Light, Post, Quality, Renderer, RendererExt, Scene, Tonemap,
    };
    use omuzaki_render_mock::MockRenderer;

    let Some(directory) = std::env::var_os("OMUZAKI_HANDS_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let opened = serde_json::json!({ "thumb_spread_deg": 25.0, "thumb_length": 1.3 });
    let narrow = serde_json::json!({ "palm_length": 2.0, "palm_width": 0.5, "thumb_spread_deg": 30.0 });
    for (label, template, hand) in [
        ("soft-sd", "soft-sd", opened.clone()),
        ("standard", "standard", opened),
        ("soft-sd-narrow", "soft-sd", narrow),
    ] {
        let node = serde_json::json!({
            "name": "Hands",
            "template": template,
            "seed": 3,
            "palette": { "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80" },
            "parts": { "body": { "kind": "body", "params": { "hand": hand } } }
        });
        let def = CharacterDef::from_node(&node).unwrap();
        let built = RegistryBuilder
            .build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
            .unwrap();
        let hand = built.meshes.iter().find(|m| m.mesh.name == "hand-l").unwrap();
        let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
        for p in &hand.mesh.geometry.positions {
            (lo, hi) = (lo.min(*p), hi.max(*p));
        }
        let (centre, size) = ((lo + hi) / 2.0, (hi - lo).max_element());
        for (view, azimuth_deg) in [("front", 0.0_f32), ("side", 90.0)] {
            let mut renderer = MockRenderer::new();
            let eye = centre
                + Mat4::from_rotation_y(azimuth_deg.to_radians()).transform_vector3(Vec3::new(0.0, 0.0, 3.0 * size));
            let mut scene = Scene::new(Camera::look_at(eye, centre, Vec3::Y, Camera::default().projection));
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
            for built_mesh in &built.meshes {
                let mesh = renderer.create_mesh(&built_mesh.mesh.geometry).unwrap();
                let mut spec = built.materials[&built_mesh.mesh.material].clone();
                spec.overlays.clear();
                let material = renderer.create_material(&spec.to_material(&|_| None, &|_| None).unwrap()).unwrap();
                scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
            }
            let quality = Quality { samples: 4, ..Quality::offline() };
            let image = renderer.render_image(UVec2::splat(384), AlphaEncoding::Straight, &scene, &quality).unwrap();
            let mut ppm = format!("P6\n{} {}\n255\n", image.size.x, image.size.y).into_bytes();
            for y in 0..image.size.y {
                for x in 0..image.size.x {
                    let [r, g, b, _] = image.pixel(x, y).unwrap();
                    ppm.extend([r, g, b]);
                }
            }
            std::fs::write(directory.join(format!("{label}-{view}.ppm")), ppm).unwrap();
        }
    }
}

#[test]
fn the_wrist_end_is_white_and_the_tips_are_the_warmer_tint_relative_to_it() {
    // The renderer multiplies the material's albedo by the vertex colour, so the hand meets the bare arm in white.
    let pieces = Pieces::of(HandStyle::Mitten);
    let parts = build("soft-sd", &params(HandStyle::Mitten, HandParams::default(), 1.0));
    let colours = &mesh(&parts, "hand-l").geometry.colours;
    let white = |c: &[f32; 4]| c.iter().all(|v| (v - 1.0).abs() < 1e-6);
    assert!(colours[pieces.wrist_ring()].iter().all(white), "the wrist sleeve's ring is not white");
    assert!(colours[..pieces.ring].iter().all(white), "the block's first ring is not white");
    // The tip: the prototype's tip tone over its base tone, per channel (0.86 / 0.93 and 0.83 / 0.91), never darker
    // than that and never brighter than white.
    let tip = colours[pieces.thumb - 1];
    let want = [1.0, 0.86 / 0.93, 0.83 / 0.91];
    assert!(tip[..3].iter().zip(want).all(|(v, w)| (v - w).abs() < 1e-6), "{tip:?}");
    assert!(colours.iter().all(|c| c[..3].iter().all(|v| (0.91..=1.0).contains(v))));
}

/// The whole character with the body of `p` on `template`: its left forearm's direction, its `arm.L` mesh and its
/// `hand-l` mesh.
fn arm_and_hand(template: &str, p: &BodyParams) -> (Vec3, PartMesh, PartMesh) {
    use omuzaki_anim_api::HumanoidBone::{LeftHand, LeftLowerArm};
    use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, RegistryBuilder};
    let node = serde_json::json!({
        "name": "Hands",
        "template": template,
        "seed": 3,
        "palette": { "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80" },
        "parts": { "body": { "kind": "body", "params": serde_json::to_value(p).unwrap() } }
    });
    let def = CharacterDef::from_node(&node).unwrap();
    let built = RegistryBuilder
        .build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let part = |name: &str| built.meshes.iter().find(|m| m.mesh.name == name).unwrap().mesh.clone();
    let layout = registry::find(template).unwrap().try_layout(&p.proportions, &p.shape, &p.hand).unwrap();
    let axis = (layout.joint64(LeftHand).unwrap() - layout.joint64(LeftLowerArm).unwrap()).normalize().as_vec3();
    (axis, part("arm.L"), part("hand-l"))
}

#[test]
fn the_open_wrist_ring_lies_inside_the_forearms_surface_on_every_template_size_and_extreme() {
    for template in TEMPLATES {
        for size in [0.5, 1.0, 2.0] {
            for hand in thumb_cases() {
                for style in [HandStyle::Mitten, HandStyle::Simple] {
                    let (axis, arm, hand_mesh) = arm_and_hand(template, &params(style, hand.clone(), size));
                    for &v in &hand_mesh.geometry.positions[Pieces::of(style).wrist_ring()] {
                        assert!(
                            inside_cut(v, &arm.geometry.positions, triangles(&arm), axis),
                            "{template} {style:?} size {size} {hand:?}: a vertex of the wrist sleeve's open ring is \
                             outside the forearm's surface"
                        );
                    }
                }
            }
        }
    }
}
