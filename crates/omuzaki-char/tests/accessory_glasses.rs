//! The `glasses` accessory (CB-43, #332): four frames on the four templates, in front of the eyes off the face, below
//! the brow, with the temple arms on the ear anchors.

#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // test arithmetic
#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

mod accessory_common;

use accessory_common::{
    TEMPLATES, above_head, assert_closed, assert_mirror_symmetric, assert_recolours_in_place, assert_rides_the_bone,
    assert_weights_valid, build_budget_ms, build_ms, built_scene, character, mesh_named, meshes, pid, scene, styles,
    vertices,
};
use omuzaki_anim_api::HumanoidBone;
use omuzaki_char_api::find_kind;
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_face_api::FaceLayout;
use omuzaki_render_api::AlphaMode;
use serde_json::json;

/// Triangles one accessory kind may use at live quality.
const TRIANGLE_BUDGET: usize = 2000;
/// How far the far lens may stand past the head's outline in a three-quarter view, metres. At the default eye spacing
/// the far eye itself is near the outline from 35 degrees, so its lens shows past it by about its own distance off the
/// face (1 cm) and the rim's wire, never by the extra a flat lens gains where the head curves away from it.
const FAR_LENS_PAST_OUTLINE: f32 = 0.015;

#[test]
fn every_style_builds_closed_shells_on_the_four_templates() {
    for template in TEMPLATES {
        for (name, params) in styles("glasses") {
            let s = built_scene(template, "glasses", params.clone(), None);
            assert_closed(&s.built, &format!("glasses {name} on {template}"));
            assert_weights_valid(&s.built, &format!("glasses {name} on {template}"));
        }
    }
}

#[test]
fn the_kind_is_an_accessory_that_needs_the_head_and_refuses_frames_that_reach_the_brow() {
    let registration = find_kind("glasses").expect("glasses is registered");
    let meta = (registration.meta)();
    assert_eq!(meta.category, omuzaki_char_api::PartCategory::Accessory);
    assert_eq!(meta.requires, &["head"]);
    let defaults = (registration.defaults)();
    assert_eq!((registration.check)(&defaults).unwrap(), defaults);
    assert!((registration.check)(&json!({ "no_such_field": 1 })).is_err());
    for bad in [json!({ "gap": 0.0 }), json!({ "eye_y": 5.0 })] {
        assert!((registration.check)(&bad).is_err(), "{bad} should be refused");
    }
}

/// Metres per unit of the face layout: the scale the head was built at (the face layout is in the template's head's
/// units, and the head is scaled to the character's real size).
fn face_scale(s: &accessory_common::Scene) -> f32 {
    s.built.stature.map_or(1.0, |stature| stature.scale)
}

/// The vertices in front of the face: the lenses and rims, not the arms running back along the head.
fn front(s: &accessory_common::Scene) -> Vec<Vec3> {
    let c = s.head.centre();
    vertices(&s.built).into_iter().filter(|p| p.z > c.z + 0.3 * s.head.radius(Vec3::Z)).collect()
}

#[test]
fn frames_that_would_reach_the_brow_or_touch_each_other_are_a_build_error() {
    for (what, params) in
        [("brow", json!({ "size": 1.6 })), ("each other", json!({ "eye_spacing": 0.05, "size": 1.0 }))]
    {
        let err = scene(&character("standard", "glasses", params.clone(), None))
            .err()
            .unwrap_or_else(|| panic!("{params} built"))
            .to_string();
        assert!(err.contains(what), "{err}");
    }
}

#[test]
fn the_glasses_follow_the_characters_face_layout() {
    // Moving the eye line down the face moves the lenses with it, and a high brow lets bigger frames through.
    let lens_height = |eye_y: f64, brow_gap: f64, size: f64| {
        let mut def = character("soft-sd", "glasses", json!({ "size": size }), None);
        def.face.layout.eye_y = eye_y;
        def.face.layout.brow_gap = brow_gap;
        let s = scene(&def).unwrap();
        let lens = mesh_named(&s.built, "-lens");
        let ys: Vec<f32> = lens.geometry.positions.iter().map(|p| p.y).collect();
        (ys.iter().sum::<f32>() / ys.len() as f32) - s.head.centre().y
    };
    let (default, lower) = (lens_height(-0.09, 0.08, 1.0), lens_height(-0.12, 0.08, 1.0));
    assert!(
        (default - lower - 0.03).abs() < 0.006,
        "the lenses moved {} m for a 0.03 m lower eye line",
        default - lower
    );
    let err = scene(&{
        let mut def = character("soft-sd", "glasses", json!({ "size": 1.3 }), None);
        def.face.layout.brow_gap = 0.05;
        def
    })
    .err()
    .expect("frames that fit a high brow do not fit a low one")
    .to_string();
    assert!(err.contains("brow"), "{err}");
    assert!(lens_height(-0.09, 0.11, 1.3).is_finite());
}

#[test]
fn the_frames_stay_6_mm_off_the_face_and_below_the_brow_line() {
    let layout = FaceLayout::default();
    for template in TEMPLATES {
        for (name, params) in styles("glasses") {
            let s = built_scene(template, "glasses", params.clone(), None);
            let brow = (layout.eye_y + layout.brow_gap) as f32 * face_scale(&s);
            let near = front(&s).iter().map(|p| above_head(s.head.as_ref(), *p)).fold(f32::MAX, f32::min);
            assert!(near >= 0.006, "glasses {name} on {template}: {near} m from the face");
            let c = s.head.centre();
            let top = front(&s).iter().map(|p| p.y - c.y).fold(f32::MIN, f32::max);
            assert!(top < brow, "glasses {name} on {template}: the frame reaches {top} m, the brow is at {brow}");
        }
    }
}

#[test]
fn the_lenses_sit_over_the_painted_eyes_on_every_template() {
    // The painted face is projected from the front through the head's overlay window, which is the face layout scaled
    // with the head: seen from the front, each lens's centre is on its painted eye (the layout's eye, scaled).
    let layout = FaceLayout::default();
    for template in TEMPLATES {
        for (name, params) in styles("glasses") {
            let s = built_scene(template, "glasses", params.clone(), None);
            let (c, k) = (s.head.centre(), face_scale(&s));
            let lens = mesh_named(&s.built, "-lens");
            let left: Vec<Vec3> = lens.geometry.positions.iter().copied().filter(|p| p.x > c.x).collect();
            let (lo, hi) =
                left.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), p| (l.min(*p), h.max(*p)));
            let centre = (lo + hi) / 2.0 - c;
            let eye = Vec3::new(layout.eye_spacing as f32, layout.eye_y as f32, 0.0) * k;
            let off = (centre.truncate() - eye.truncate()).length();
            // The lens stands straight in front of the eye; its outline (a cat-eye's lifted corner, the wrap round the
            // head) moves the middle of its extent by a few millimetres at most.
            assert!(
                off <= 0.1 * eye.x,
                "glasses {name} on {template}: the lens is {off} m from its eye, seen from the front"
            );
        }
    }
}

/// How far the far lens and its rim (the character's right, in front of the face) stand past the head's outline when the
/// head is seen from `azimuth_deg` towards its left, metres: the largest margin, over their vertices, between a vertex
/// and the head's own outline at the vertex's height.
fn far_side_past_the_outline(s: &accessory_common::Scene, azimuth_deg: f32) -> f32 {
    let (sin, cos) = azimuth_deg.to_radians().sin_cos();
    // Screen right for a camera on that azimuth, looking at the head.
    let right = Vec3::new(cos, 0.0, -sin);
    let c = s.head.centre();
    // The head's outline at each millimetre of height: the leftmost its surface reaches on screen. Latitudes a quarter
    // degree apart put samples of every longitude into every millimetre.
    let mut outline = std::collections::BTreeMap::<i32, f32>::new();
    for k in 0..360 {
        for j in 0..=720 {
            let (theta, phi) = ((k as f32).to_radians(), (j as f32 / 4.0 - 90.0).to_radians());
            let d = Vec3::new(theta.sin() * phi.cos(), phi.sin(), theta.cos() * phi.cos());
            let q = d * s.head.radius(d);
            let edge = outline.entry((q.y * 1000.0).round() as i32).or_insert(f32::MAX);
            *edge = edge.min(q.dot(right));
        }
    }
    let edge_at = |y: f32| {
        let mm = (y * 1000.0).round() as i32;
        outline.range(mm - 1..=mm + 1).map(|(_, edge)| *edge).fold(f32::MAX, f32::min)
    };
    front(s)
        .iter()
        .filter(|p| p.x < c.x)
        .map(|p| edge_at(p.y - c.y) - (*p - c).dot(right))
        .filter(|past| past.is_finite() && *past < 1.0)
        .fold(f32::MIN, f32::max)
}

#[test]
fn seen_from_three_quarters_the_far_lens_keeps_to_the_heads_outline() {
    // The lenses wrap round the head, so from a three-quarter view (35 degrees, the evidence sheet's) the far lens's
    // outer edge stays at the outline instead of standing out past it.
    for template in ["soft-sd", "standard"] {
        for (name, params) in styles("glasses") {
            let s = built_scene(template, "glasses", params.clone(), None);
            let past = far_side_past_the_outline(&s, 35.0);
            println!("glasses {name} on {template}: the far lens stands {past:.4} m past the head's outline");
            assert!(
                past <= FAR_LENS_PAST_OUTLINE,
                "glasses {name} on {template}: the far lens stands {past} m past the head"
            );
        }
    }
}

#[test]
fn the_lenses_stand_1_cm_in_front_of_the_eyes() {
    for template in TEMPLATES {
        let s = built_scene(template, "glasses", json!({ "style": "round" }), None);
        let lens = mesh_named(&s.built, "-lens");
        // The back of the lens, at its centre, is the closest it comes to the face.
        let gap = lens.geometry.positions.iter().map(|p| above_head(s.head.as_ref(), *p)).fold(f32::MAX, f32::min);
        assert!((0.008..=0.014).contains(&gap), "{template}: the lens is {gap} m off the face");
    }
}

#[test]
fn the_temple_arms_reach_the_ear_anchors_within_3_mm() {
    for template in TEMPLATES {
        for (name, params) in styles("glasses") {
            let s = built_scene(template, "glasses", params.clone(), None);
            let points = vertices(&s.built);
            for ((_, anchor), ear) in s.built.anchors.iter().filter(|((_, a), _)| a.as_str().starts_with("ear_")) {
                let nearest = points.iter().map(|p| p.distance(ear.position)).fold(f32::MAX, f32::min);
                assert!(nearest <= 0.003, "glasses {name} on {template}: the arm is {nearest} m from {anchor}");
            }
        }
    }
}

#[test]
fn without_a_side_ear_the_arms_end_on_the_side_of_the_head() {
    let mut def = character("standard", "glasses", json!({}), None);
    def.parts.get_mut(&pid("head")).unwrap().params["ears"] = json!({ "kind": "none" });
    let s = scene(&def).unwrap();
    assert_closed(&s.built, "glasses without ears");
    let c = s.head.centre();
    let rearmost = vertices(&s.built).iter().map(|p| p.z - c.z).fold(f32::MAX, f32::min);
    assert!(rearmost < 0.02, "the arms reach back to {rearmost}: they should run to the side of the head");
}

#[test]
fn the_arms_do_not_sink_into_the_head_away_from_the_ears() {
    // The ear anchors come from the head part's default ear position, which is sculpted for soft-sd: on the other templates
    // (#825) an anchor can lie off the surface, and the arm follows it, so only the arm's length away from the ears counts.
    for template in TEMPLATES {
        let s = built_scene(template, "glasses", json!({}), None);
        let ears: Vec<Vec3> = s
            .built
            .anchors
            .iter()
            .filter(|((_, a), _)| a.as_str().starts_with("ear_"))
            .map(|(_, e)| e.position)
            .collect();
        let lowest = vertices(&s.built)
            .iter()
            .filter(|p| ears.iter().all(|e| p.distance(*e) > 0.05))
            .map(|p| above_head(s.head.as_ref(), *p))
            .fold(f32::MAX, f32::min);
        assert!(lowest > 0.0, "{template}: a vertex is {lowest} m inside the head");
    }
}

#[test]
fn the_pair_is_an_exact_mirror_image() {
    for (name, params) in styles("glasses") {
        let s = built_scene("standard", "glasses", params, None);
        assert_mirror_symmetric(&vertices(&s.built), s.head.centre().x, 5e-4, &format!("glasses {name}"));
    }
}

#[test]
fn the_lens_blends_and_the_frame_is_opaque() {
    let b = built_scene("standard", "glasses", json!({ "style": "sunglasses" }), None).built;
    let lens = &b.materials[&"acc-lens".parse().unwrap()];
    assert_eq!(lens.alpha, AlphaMode::Blend);
    assert!(lens.base.a > 200, "sunglasses block most of the light: {}", lens.base.a);
    assert_ne!(b.materials[&"acc-frame".parse().unwrap()].alpha, AlphaMode::Blend);
    let clear = built_scene("standard", "glasses", json!({ "lens_coverage": 0.1 }), None).built;
    assert!(clear.materials[&"acc-lens".parse().unwrap()].base.a < 40);
}

#[test]
fn the_glasses_ride_the_head() {
    for template in TEMPLATES {
        let s = built_scene(template, "glasses", json!({ "style": "cat_eye" }), None);
        for turn in [Quat::from_rotation_y(45f32.to_radians()), Quat::from_rotation_x(20f32.to_radians())] {
            assert_rides_the_bone(&s.built, HumanoidBone::Head, turn, &format!("glasses on {template}"));
        }
    }
}

#[test]
fn a_palette_change_recolours_the_glasses_without_new_geometry() {
    let params = json!({ "frame": "@f", "lens": "@l" });
    assert_recolours_in_place(character("standard", "glasses", params, None), &["f", "l"]);
}

#[test]
fn the_bytes_are_deterministic_and_inside_the_budget() {
    for (name, params) in styles("glasses") {
        let (a, b) = (
            built_scene("tall", "glasses", params.clone(), None),
            built_scene("tall", "glasses", params.clone(), None),
        );
        for (x, y) in meshes(&a.built).iter().zip(meshes(&b.built)) {
            assert_eq!(x.geometry, y.geometry, "{name}");
        }
        let triangles: usize = meshes(&a.built).iter().map(|m| m.geometry.indices.len() / 3).sum();
        println!("glasses {name}: {triangles} triangles");
        assert!(triangles <= TRIANGLE_BUDGET, "glasses {name}: {triangles} triangles");
    }
}

#[test]
fn the_glasses_build_in_5_ms_or_less() {
    for (name, params) in styles("glasses") {
        let ms = build_ms(&character("standard", "glasses", params, None), 20);
        println!("glasses {name}: {ms:.3} ms");
        assert!(ms <= build_budget_ms(), "glasses {name} took {ms:.2} ms");
    }
}

#[test]
fn every_documented_preset_is_among_the_styles_the_sweeps_use() {
    accessory_common::assert_styles_found("glasses", &["round", "square", "cat_eye", "sunglasses"]);
}

#[test]
fn an_eye_line_override_keeps_the_brow_where_the_layout_puts_it() {
    // The default layout's brow is at -0.09 + 0.08 = -0.01 (face-layout units, scaled with the head). Moving the
    // glasses' eye line up to -0.05 leaves 0.04 under the brow, not the layout's 0.08: frames that would reach past it
    // are refused, and small ones stay under it.
    for template in ["soft-sd", "standard"] {
        let err = scene(&character(template, "glasses", json!({ "eye_y": -0.05 }), None))
            .err()
            .unwrap_or_else(|| panic!("{template}: full-size frames reach the brow"))
            .to_string();
        assert!(err.contains("brow"), "{template}: {err}");
        let s = built_scene(template, "glasses", json!({ "eye_y": -0.05, "size": 0.5 }), None);
        let (c, brow) = (s.head.centre(), -0.01 * face_scale(&s));
        let top = front(&s).iter().map(|p| p.y - c.y).fold(f32::MIN, f32::max);
        assert!(top < brow, "{template}: the frames reach {top}, the brow is at {brow}");
    }
}
