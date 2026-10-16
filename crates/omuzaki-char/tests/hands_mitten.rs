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

/// The vertices of one ring of the thumb: its segments round, and the first vertex repeated.
const THUMB_RING: usize = 17;

/// The segments round the block's ring and the rings in the block, per style (`hands::mitten` and `hands::simple`).
fn block_vertices(style: HandStyle) -> (usize, usize) {
    let around = if style == HandStyle::Mitten { 28 } else { 30 };
    (around + 1, (4 + 8) * (around + 1))
}

/// Every directed edge once, and its reverse once, but for the two open ends the prototype leaves: the wrist ring (a
/// cuff hides it) and the thumb's stem ring (inside the palm). Closed and consistently wound everywhere else.
fn assert_watertight(label: &str, m: &PartMesh, style: HandStyle) {
    let (ring, block) = block_vertices(style);
    let mut ids: HashMap<[i64; 3], usize> = HashMap::new();
    let q = |p: Vec3| [p.x, p.y, p.z].map(|v| (f64::from(v) * 1e6).round() as i64);
    let mut open = std::collections::HashSet::new();
    for (k, p) in m.geometry.positions.iter().enumerate() {
        let next = ids.len();
        let id = *ids.entry(q(*p)).or_insert(next);
        if k < ring || (block..block + THUMB_RING).contains(&k) {
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
    let (_, block) = block_vertices(HandStyle::Mitten);
    for template in TEMPLATES {
        for (palm_length, palm_width, thumb_length, spread) in
            [(2.0, 0.5, 1.0, 30.0), (0.5, 2.0, 2.0, -30.0), (2.0, 2.0, 0.5, 30.0), (1.0, 1.0, 1.0, 0.0)]
        {
            let hand =
                HandParams { palm_length, palm_width, thumb_length, thumb_spread_deg: spread, ..Default::default() };
            let (layout, parts) = built(template, &params(HandStyle::Mitten, hand, 1.0));
            let positions = &mesh(&parts, "hand-l").geometry.positions;
            // The thumb's first ring is the stem inside the palm; the root ring follows it.
            let ring = &positions[block + THUMB_RING..block + 2 * THUMB_RING];
            let root = ring[..THUMB_RING - 1].iter().copied().sum::<Vec3>() / (THUMB_RING - 1) as f32;
            let joint = layout.joint64(LeftThumbProximal).unwrap().as_vec3();
            let error = root.distance(joint);
            assert!(
                error < 1e-4,
                "{template} length {palm_length} width {palm_width} spread {spread}: the thumb root is {error} m off"
            );
        }
    }
}

#[test]
fn the_thumbs_first_ring_lies_in_the_palm_at_every_extreme_and_template() {
    use omuzaki_anim_api::HumanoidBone::{LeftHand, LeftLowerArm};
    let (ring, block) = block_vertices(HandStyle::Mitten);
    for template in TEMPLATES {
        for size in [0.5, 1.0, 2.0] {
            for hand in extremes() {
                let (layout, parts) = built(template, &params(HandStyle::Mitten, hand.clone(), size));
                let (wrist, elbow) = (layout.joint64(LeftHand).unwrap(), layout.joint64(LeftLowerArm).unwrap());
                let axis = (wrist - elbow).normalize().as_vec3();
                let wrist = wrist.as_vec3();
                let positions = &mesh(&parts, "hand-l").geometry.positions;
                let first = &positions[block..block + THUMB_RING - 1];
                let centre = first.iter().copied().sum::<Vec3>() / first.len() as f32;
                let along = |p: Vec3| (p - wrist).dot(axis);
                let off = centre - wrist - axis * along(centre);
                let side = off.normalize_or_zero();
                // The block's ring nearest the thumb's first ring along the arm, and how far it reaches towards the thumb.
                let nearest = positions[..block]
                    .chunks_exact(ring)
                    .min_by(|a, b| {
                        let d = |r: &[Vec3]| (along(r[0]) - along(centre)).abs();
                        d(a).total_cmp(&d(b))
                    })
                    .unwrap();
                let reach = nearest.iter().map(|p| (*p - wrist).dot(side)).fold(f32::MIN, f32::max);
                assert!(
                    off.length() < reach,
                    "{template} size {size} {hand:?}: the thumb's first ring is {} m from the axis, the palm reaches {reach} m",
                    off.length()
                );
            }
        }
    }
}

/// Renders the left hand of the built body (front and side) through the mock toon renderer when
/// `OMUZAKI_HANDS_PPM=<directory>` is set, as binary PPM files, on soft-sd and standard, with the thumb opened and
/// lengthened so its placement shows. The fingertip tint is the hands' vertex colours.
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
    for template in ["soft-sd", "standard"] {
        let node = serde_json::json!({
            "name": "Hands",
            "template": template,
            "seed": 3,
            "palette": { "skin": "#f9d3c5", "skin_shade": "#f2b4ab", "skin_line": "#c98a80" },
            "parts": { "body": { "kind": "body", "params": { "hand": { "thumb_spread_deg": 25.0, "thumb_length": 1.3 } } } }
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
            std::fs::write(directory.join(format!("{template}-{view}.ppm")), ppm).unwrap();
        }
    }
}

#[test]
fn the_wrist_end_is_white_and_the_tips_are_the_warmer_tint_relative_to_it() {
    // The renderer multiplies the material's albedo by the vertex colour, so the hand meets the bare arm in white.
    let (ring, block) = block_vertices(HandStyle::Mitten);
    let parts = build("soft-sd", &params(HandStyle::Mitten, HandParams::default(), 1.0));
    let colours = &mesh(&parts, "hand-l").geometry.colours;
    assert!(colours[..ring].iter().all(|c| c.iter().all(|v| (v - 1.0).abs() < 1e-6)), "the wrist ring is not white");
    // The tip: the prototype's tip tone over its base tone, per channel (0.68 / 0.80 and 0.60 / 0.70), never darker
    // than that and never brighter than white.
    let tip = colours[block - 1];
    assert!(
        (tip[1] - 0.85).abs() < 1e-6 && (tip[2] - 0.60 / 0.70).abs() < 1e-6 && (tip[0] - 1.0).abs() < 1e-6,
        "{tip:?}"
    );
    assert!(colours.iter().all(|c| c[..3].iter().all(|v| (0.84..=1.0).contains(v))));
}

#[test]
fn the_open_wrist_ring_lies_inside_the_forearms_surface_on_every_template_size_and_extreme() {
    use omuzaki_anim_api::HumanoidBone::LeftHand;
    use omuzaki_char::template::layout::section_basis;
    use omuzaki_char_api::BodyRegion;
    let (ring, _) = block_vertices(HandStyle::Mitten);
    for template in TEMPLATES {
        for size in [0.5, 1.0, 2.0] {
            for hand in extremes() {
                for style in [HandStyle::Mitten, HandStyle::Simple] {
                    let (layout, parts) = built(template, &params(style, hand.clone(), size));
                    let arm = layout.region(BodyRegion::ArmL).unwrap();
                    let wrist = layout.joint64(LeftHand).unwrap();
                    let around = if style == HandStyle::Mitten { ring } else { block_vertices(style).0 };
                    for p in &mesh(&parts, "hand-l").geometry.positions[..around] {
                        let point = p.as_dvec3();
                        let axis = arm.section(1.0).axis;
                        let t = 1.0 - (wrist - point).dot(axis) / arm.length();
                        let section = arm.section(t);
                        let (across, through) = section_basis(BodyRegion::ArmL, section.axis.as_vec3());
                        let d = point - section.centre;
                        let x = d.dot(across.as_dvec3()) / section.radii.x;
                        let y = d.dot(through.as_dvec3()) / section.radii.y;
                        assert!(
                            x * x + y * y <= 1.0,
                            "{template} {style:?} size {size} {hand:?}: a wrist ring vertex is at {} of the forearm",
                            (x * x + y * y).sqrt()
                        );
                    }
                }
            }
        }
    }
}
