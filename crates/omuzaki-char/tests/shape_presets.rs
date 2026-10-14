//! The five face-shape presets (CB-10): each is in the library, valid, inside the plausible space, builds a
//! closed head, and measures its stated targets; blends between them are continuous.
//!
//! **The targets** ([`TARGETS`]) were written from each preset's geometric description before it was fitted. They are
//! measured on the head built from the preset on the template's default cranium
//! ([`omuzaki_char::head::presets::measure`]), not on the raw superellipse, and the layout ratios are CB-12's
//! (`FaceLayout::ratios`). Two ignored tests produce evidence: `report` prints the measured table as Markdown, and
//! `dump` writes the five meshes and their target curves as JSON for the contact sheet (`SHAPE_PRESETS_OUT`).

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // loop counters and metres to millimetres
#![allow(clippy::print_stdout, reason = "the `report` test prints the evidence table")]
#![allow(
    clippy::disallowed_methods,
    reason = "the `dump` test writes evidence to a folder the developer names; the fence is for ops"
)]

use std::collections::HashMap;
use std::fmt::Write as _;

use omuzaki_char::head::mesh::{Resolution, build};
use omuzaki_char::head::outline::{half_width_at, plausible};
use omuzaki_char::head::presets::measure::{Silhouette, SilhouetteMetrics};
use omuzaki_char::head::presets::{ShapePreset, ShapeProfileError, ShapeProfiles};
use omuzaki_char::template::registry;
use omuzaki_char_api::{HeadOutline, HeadParams, PRESET_IDS};
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::Validate;

/// What a preset is designed to measure, written from its description before it was fitted.
///
/// The head's own height and width come from the cranium, which a face shape does not change, so the targets are
/// the ratios of the lower face (eye line to chin) that the outline does set. `chin` is the front angle at the chin in
/// degrees; `fullness` the width half way down the lower face over the width at the eye line (a superellipse of
/// exponent p has `(1 - 0.5^p)^(1/p)`); the last four are CB-12's `FaceRatios`.
struct Target {
    id: &'static str,
    /// Lower-face length over its width at the eye line.
    length_to_width: f64,
    fullness: f64,
    chin: f64,
    /// The jaw's width a quarter of the face's length above the chin, metres.
    jaw_width: f64,
    eye_width_to_iod: f64,
    nose: f64,
    mouth: f64,
    drop_to_iod: f64,
}

#[rustfmt::skip]
const TARGETS: [Target; 5] = [
    // Between mochi and the egg: a short lower face with soft cheeks, round and no chin point.
    Target { id: "round-u", length_to_width: 0.319, fullness: 0.881, chin: 170.9, jaw_width: 0.220, eye_width_to_iod: 0.29, nose: 0.30, mouth: 0.58, drop_to_iod: 0.57 },
    // Between round-u and heart: widest at the temples, tapering to a soft chin.
    Target { id: "egg-oval", length_to_width: 0.336, fullness: 0.849, chin: 167.5, jaw_width: 0.198, eye_width_to_iod: 0.28, nose: 0.31, mouth: 0.60, drop_to_iod: 0.61 },
    // The edge of the plausible space: the highest chin, the fullest exponent, the fullest and softest cheeks, the widest jaw.
    Target { id: "mochi", length_to_width: 0.299, fullness: 0.916, chin: 173.8, jaw_width: 0.251, eye_width_to_iod: 0.31, nose: 0.27, mouth: 0.56, drop_to_iod: 0.45 },
    // The edge: the widest cheeks the eye line allows, the lowest exponent (1.6, where the crease limit binds), the deepest chin, a narrow jaw.
    Target { id: "heart", length_to_width: 0.398, fullness: 0.779, chin: 155.3, jaw_width: 0.180, eye_width_to_iod: 0.27, nose: 0.30, mouth: 0.62, drop_to_iod: 0.84 },
    // The edge: the deepest chin on the narrowest jaw, the eye line high, almost as pointed as the heart.
    Target { id: "long", length_to_width: 0.403, fullness: 0.792, chin: 157.0, jaw_width: 0.174, eye_width_to_iod: 0.26, nose: 0.32, mouth: 0.64, drop_to_iod: 0.77 },
];

/// The acceptance tolerance: measured within 5% of the target.
const TOLERANCE: f64 = 0.05;

fn preset(id: &str) -> &'static ShapePreset {
    ShapeProfiles::get(id).unwrap()
}

fn params_of(outline: HeadOutline) -> HeadParams {
    HeadParams { outline, ..HeadParams::default() }
}

fn live(outline: HeadOutline) -> MeshData {
    build(&params_of(outline), Resolution::LIVE, None).unwrap()
}

/// The silhouette numbers of the head built from `p` on the template's default cranium, finely meshed.
fn measure(p: &ShapePreset) -> SilhouetteMetrics {
    let params = params_of(p.profile.outline);
    let mesh = build(&params, Resolution::OFFLINE, None).unwrap();
    Silhouette::of(&mesh, params.cranium.centre[1]).unwrap().metrics(p.profile.outline.top)
}

fn within(what: &str, id: &str, measured: f64, target: f64) {
    let error = (measured - target).abs() / target.abs();
    assert!(error <= TOLERANCE, "{id}: {what} {measured:.4} is {:.1}% from its target {target}", error * 100.0);
}

// ------------------------------------------------------------------------------------------------ the library

#[test]
fn the_library_has_the_five_reserved_presets_in_order() {
    let ids: Vec<&str> = ShapeProfiles::list().unwrap().iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, PRESET_IDS);
    assert_eq!(TARGETS.map(|t| t.id), PRESET_IDS, "every preset has its targets, in order");
    for p in ShapeProfiles::list().unwrap() {
        assert!(!p.summary.is_empty() && p.summary.len() < 120, "{}: one line for pickers", p.id.as_str());
        assert_eq!(ShapeProfiles::get(p.id.as_str()).unwrap(), p);
    }
}

#[test]
fn an_unknown_preset_is_an_error_that_names_the_five() {
    let e = ShapeProfiles::get("sheet").unwrap_err();
    assert_eq!(e, ShapeProfileError::Unknown("sheet".into()));
    assert!(e.to_string().contains("round-u"), "{e}");
}

#[test]
fn every_preset_validates_and_states_its_chin() {
    for p in ShapeProfiles::list().unwrap() {
        p.profile.outline.validate().unwrap();
        p.profile.layout.validate().unwrap();
        assert!(p.profile.outline.chin.is_some(), "{}: blends never switch a chin in half way", p.id.as_str());
        let o = p.profile.outline;
        assert!((o.height - (o.top - o.chin_y())).abs() < 1e-9, "{}: height is top minus the chin", p.id.as_str());
    }
}

#[test]
fn every_preset_is_inside_the_plausible_space_so_what_it_names_is_what_is_built() {
    let cranium = HeadParams::default().cranium;
    for p in ShapeProfiles::list().unwrap() {
        let (given, built) = (p.profile.outline, plausible(&p.profile.outline, &cranium));
        let (gc, bc) = (given.chin.unwrap(), built.chin.unwrap());
        for (what, a, b) in [
            ("half_width", given.half_width, built.half_width),
            ("top", given.top, built.top),
            ("height", given.height, built.height),
            ("exponent", given.exponent, built.exponent),
            ("cheek_puff", given.cheek_puff, built.cheek_puff),
            ("chin/y", gc.y, bc.y),
            ("chin/stretch_top", gc.stretch_top, bc.stretch_top),
            ("chin/min_ratio", gc.min_ratio, bc.min_ratio),
        ] {
            assert!((a - b).abs() < 5e-5, "{}: {what} {a} is clamped to {b}", p.id.as_str());
        }
    }
}

// ------------------------------------------------------------------------------------------------ the targets

#[test]
fn each_preset_measures_its_silhouette_targets_within_5_percent() {
    for t in &TARGETS {
        let p = preset(t.id);
        let m = measure(p);
        within("lower-face length / width", t.id, m.face_length_to_width(), t.length_to_width);
        within("mid-face fullness", t.id, m.mid_face_fullness, t.fullness);
        within("chin angle", t.id, m.chin_angle_deg, t.chin);
        within("jaw width", t.id, m.jaw_width, t.jaw_width);
    }
}

#[test]
fn each_preset_lays_out_the_features_by_its_cb12_ratios() {
    for t in &TARGETS {
        let p = preset(t.id);
        let r = p.profile.layout.ratios(p.profile.outline.chin_y()).unwrap();
        within("eye width / iod", t.id, r.eye_width_to_iod, t.eye_width_to_iod);
        within("nose", t.id, r.nose, t.nose);
        within("mouth", t.id, r.mouth, t.mouth);
        within("drop / iod", t.id, r.drop_to_iod, t.drop_to_iod);
    }
}

#[test]
fn the_presets_differ_the_way_their_descriptions_say() {
    let m = |id| measure(preset(id));
    let (round_u, egg, mochi, heart, long) = (m("round-u"), m("egg-oval"), m("mochi"), m("heart"), m("long"));
    // Mochi is the widest and shortest head, then round-u; the egg, heart and long share the template's chin depth.
    assert!(mochi.width_to_height() > round_u.width_to_height() && round_u.width_to_height() > egg.width_to_height());
    // Lower-face length grows mochi < round-u < egg < heart and long, which are as long as each other (both at the
    // deepest chin the plausible space allows); the chin sinks in the same order.
    let lengths = [mochi, round_u, egg, heart].map(|x| x.face_length_to_width());
    assert!(lengths.windows(2).all(|w| w[0] < w[1]), "{lengths:?}");
    assert!((heart.face_length_to_width() / long.face_length_to_width() - 1.0).abs() < 0.02);
    // Mochi and round-u differ in their cheeks, not only in the chin: mochi is fuller and its jaw wider.
    assert!(mochi.mid_face_fullness > round_u.mid_face_fullness + 0.03);
    assert!(mochi.jaw_width > round_u.jaw_width * 1.1);
    let chins = [mochi, round_u, egg, heart].map(|x| x.chin_y);
    assert!(chins.windows(2).all(|w| w[0] > w[1]) && (heart.chin_y - long.chin_y).abs() < 1e-6, "{chins:?}");
    // The heart is the pointiest, mochi and round-u have no chin point, and the low-set mochi eye line is lowest.
    assert!([round_u, egg, mochi, long].iter().all(|x| heart.chin_angle_deg < x.chin_angle_deg - 1.0));
    assert!(round_u.chin_angle_deg > 170.0 && mochi.chin_angle_deg > 170.0);
    assert!(
        mochi.eye_line_fraction() < heart.eye_line_fraction() && mochi.eye_line_fraction() < long.eye_line_fraction()
    );
    // The long face has the narrowest jaw; mochi's is the fullest.
    assert!([round_u, egg, mochi, heart].iter().all(|x| long.jaw_width < x.jaw_width));
    assert!([round_u, egg, heart, long].iter().all(|x| mochi.jaw_width > x.jaw_width));
}

#[test]
fn the_widest_point_of_the_head_is_the_craniums_and_the_face_is_widest_at_the_eye_line() {
    let cranium = HeadParams::default().cranium;
    for p in ShapeProfiles::list().unwrap() {
        let m = measure(p);
        assert!((m.head_width - 2.0 * cranium.half_width).abs() < 1e-3, "{}: {}", p.id.as_str(), m.head_width);
        assert!(m.widest_y.abs() < 0.01, "{}: the cranium is widest at its centre line", p.id.as_str());
        // Below the eye line the outline only narrows: its width at the top is its half-width.
        let o = p.profile.outline;
        assert!((half_width_at(&o, o.top) - o.half_width).abs() < 1e-12);
        assert!(half_width_at(&o, o.top - 0.01) < o.half_width);
    }
}

// ------------------------------------------------------------------------------------------------ closed heads

/// Asserts that `mesh` is a closed surface wound counter-clockwise from outside: welding coincident vertices, every
/// directed edge of a real triangle has exactly one opposite edge, and the enclosed volume is positive.
fn assert_closed_and_wound_outward(what: &str, mesh: &MeshData) {
    let key = |p: Vec3| [p.x, p.y, p.z].map(|v| (f64::from(v) * 1e5).round() as i64);
    let mut ids: HashMap<[i64; 3], u32> = HashMap::new();
    let welded: Vec<u32> = mesh
        .positions
        .iter()
        .map(|p| {
            let next = ids.len() as u32;
            *ids.entry(key(*p)).or_insert(next)
        })
        .collect();
    let mut edges: HashMap<(u32, u32), u32> = HashMap::new();
    let mut volume = 0.0_f64;
    for tri in mesh.indices.as_chunks::<3>().0 {
        let p = tri.map(|i| mesh.positions[i as usize].as_dvec3());
        volume += p[0].dot(p[1].cross(p[2])) / 6.0;
        let w = tri.map(|i| welded[i as usize]);
        if w[0] == w[1] || w[1] == w[2] || w[2] == w[0] {
            continue; // the collapsed cells at the poles
        }
        for (a, b) in [(w[0], w[1]), (w[1], w[2]), (w[2], w[0])] {
            *edges.entry((a, b)).or_default() += 1;
        }
    }
    for (&(a, b), &count) in &edges {
        assert_eq!(count, 1, "{what}: edge {a}->{b} is used {count} times in the same direction");
        assert_eq!(edges.get(&(b, a)), Some(&1), "{what}: edge {a}->{b} has no opposite: the surface has a hole");
    }
    assert!(volume > 0.0, "{what}: enclosed volume {volume} is not positive");
}

#[test]
fn every_preset_builds_a_watertight_head_on_every_template() {
    for template in registry::templates() {
        let head = template.defaults().head;
        for p in ShapeProfiles::list().unwrap() {
            let params = HeadParams { outline: p.profile.outline, ..head.clone() };
            let mesh = build(&params, Resolution::LIVE, None).unwrap();
            assert_closed_and_wound_outward(&format!("{} on {}", p.id.as_str(), template.id()), &mesh);
        }
    }
}

// ------------------------------------------------------------------------------------------------ blending

#[test]
fn blend_runs_from_one_preset_to_the_other_and_checks_its_input() {
    let (a, b) = (preset("mochi").profile, preset("long").profile);
    assert_eq!(ShapeProfiles::blend("mochi", "long", 0.0).unwrap(), a);
    assert_eq!(ShapeProfiles::blend("mochi", "long", 1.0).unwrap(), b);
    assert_eq!(ShapeProfiles::blend("mochi", "long", 0.25).unwrap(), a.lerp(&b, 0.25));
    for t in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
        assert!(matches!(ShapeProfiles::blend("mochi", "long", t), Err(ShapeProfileError::Blend(_))), "{t}");
    }
    assert_eq!(ShapeProfiles::blend("mochi", "nope", 0.5).unwrap_err(), ShapeProfileError::Unknown("nope".into()));
}

#[test]
fn mix_is_the_normalised_weighted_mean() {
    let (a, b, c) = (preset("round-u").profile, preset("heart").profile, preset("long").profile);
    assert_eq!(ShapeProfiles::mix(&[("heart", 3.0)]).unwrap(), b, "one preset mixes to itself, whatever its weight");
    let two = ShapeProfiles::mix(&[("round-u", 0.75), ("heart", 0.25)]).unwrap();
    let expected = a.lerp(&b, 0.25);
    assert!((two.outline.exponent - expected.outline.exponent).abs() < 1e-12);
    assert!((two.layout.mouth_y - expected.layout.mouth_y).abs() < 1e-12);
    // Three equal weights are the plain mean of the three, in any order.
    let third = ShapeProfiles::mix(&[("round-u", 1.0), ("heart", 1.0), ("long", 1.0)]).unwrap();
    let swapped = ShapeProfiles::mix(&[("long", 2.0), ("round-u", 2.0), ("heart", 2.0)]).unwrap();
    let mean = (a.outline.half_width + b.outline.half_width + c.outline.half_width) / 3.0;
    assert!((third.outline.half_width - mean).abs() < 1e-12 && (swapped.outline.half_width - mean).abs() < 1e-12);
    // A preset listed twice adds its weights; a zero weight changes nothing.
    assert_eq!(
        ShapeProfiles::mix(&[("heart", 0.5), ("heart", 0.5)]).unwrap().outline,
        ShapeProfiles::mix(&[("heart", 1.0), ("long", 0.0)]).unwrap().outline
    );
}

#[test]
fn mix_refuses_bad_weights() {
    for weights in [vec![], vec![("heart", 0.0)], vec![("heart", -1.0)], vec![("heart", f64::NAN)]] {
        assert!(matches!(ShapeProfiles::mix(&weights), Err(ShapeProfileError::Blend(_))), "{weights:?}");
    }
    assert_eq!(ShapeProfiles::mix(&[("heart", 0.0), ("x", 1.0)]).unwrap_err(), ShapeProfileError::Unknown("x".into()));
}

#[test]
fn a_blend_of_any_two_presets_is_valid_and_inside_the_plausible_space() {
    let cranium = HeadParams::default().cranium;
    for a in PRESET_IDS {
        for b in PRESET_IDS {
            for t in [0.25, 0.5, 0.75] {
                let blend = ShapeProfiles::blend(a, b, t).unwrap();
                blend.outline.validate().unwrap();
                blend.layout.validate().unwrap();
                let built = plausible(&blend.outline, &cranium);
                // The plausible limits are not convex, so a blend of two outlines inside them may sit a hair outside
                // (here by under a millimetre of width): the head builder then clamps it, and nothing jumps.
                assert!((built.half_width - blend.outline.half_width).abs() < 1e-3, "{a}/{b} at {t}");
                assert!((built.exponent - blend.outline.exponent).abs() < 0.02, "{a}/{b} at {t}");
            }
        }
    }
}

/// The largest distance any vertex moves between two meshes of the same topology, metres.
fn max_step(a: &MeshData, b: &MeshData) -> f64 {
    a.positions.iter().zip(&b.positions).map(|(p, q)| f64::from((*p - *q).length())).fold(0.0, f64::max)
}

#[test]
fn blends_move_the_head_in_proportion_to_the_blend_step() {
    const STEPS: u32 = 8;
    for (i, a) in PRESET_IDS.iter().enumerate() {
        for b in &PRESET_IDS[i + 1..] {
            let meshes: Vec<MeshData> = (0..=STEPS)
                .map(|k| live(ShapeProfiles::blend(a, b, f64::from(k) / f64::from(STEPS)).unwrap().outline))
                .collect();
            let whole = max_step(&meshes[0], &meshes[STEPS as usize]);
            let worst = meshes.windows(2).map(|w| max_step(&w[0], &w[1])).fold(0.0, f64::max);
            // Linear would be whole / STEPS; allow a factor of two for the chin stretch's nonlinearity (the bound
            // `tests/head_shape.rs` holds any two outlines to) plus a micrometre of float noise.
            assert!(worst <= 2.0 * whole / f64::from(STEPS) + 1e-6, "{a}/{b}: step {worst} vs whole {whole}");
        }
    }
}

// ------------------------------------------------------------------------------------------------ evidence

/// The numbers a preset is held to as a table row, for the PR.
fn row(t: &Target) -> String {
    let shape = preset(t.id);
    let (metrics, outline) = (measure(shape), shape.profile.outline);
    let ratios = shape.profile.layout.ratios(outline.chin_y()).unwrap();
    let pct =
        |measured: f64, target: f64| format!("{measured:.3} / {target} ({:+.1}%)", (measured / target - 1.0) * 100.0);
    format!(
        "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.3} |",
        t.id,
        pct(metrics.face_length_to_width(), t.length_to_width),
        pct(metrics.mid_face_fullness, t.fullness),
        pct(metrics.chin_angle_deg, t.chin),
        pct(metrics.jaw_width, t.jaw_width),
        pct(ratios.eye_width_to_iod, t.eye_width_to_iod),
        pct(ratios.nose, t.nose),
        pct(ratios.mouth, t.mouth),
        pct(ratios.drop_to_iod, t.drop_to_iod),
        metrics.width_to_height(),
    )
}

/// `cargo test -p omuzaki-char --test shape_presets -- --ignored --nocapture report`
#[test]
#[ignore = "prints the evidence table"]
fn report() {
    let mut out = String::from(
        "| preset | length/width | mid-face fullness | chin angle | jaw width (m) | eye width/iod | nose | mouth | drop/iod | head W/H |\n|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for t in &TARGETS {
        writeln!(out, "{}", row(t)).unwrap();
    }
    println!("{out}");
}

/// The target outline of a preset in the front view: the superellipse through the measured eye-line width, the target
/// length and the target fullness, as `[x, y]` head-local points down the right side (the sheet mirrors it).
fn target_curve(t: &Target, face_width: f64, eye_line: f64) -> Vec<[f64; 2]> {
    // The exponent whose mid-point width is the target fullness (monotone, so bisect).
    let fullness = |p: f64| (1.0 - 0.5_f64.powf(p)).powf(1.0 / p);
    let (mut low, mut high) = (1.0_f64, 6.0_f64);
    for _ in 0..60 {
        let mid = f64::midpoint(low, high);
        if fullness(mid) < t.fullness { low = mid } else { high = mid }
    }
    let (half, length) = (face_width / 2.0, t.length_to_width * face_width);
    (0..=60)
        .map(|k| {
            let u = f64::from(k) / 60.0;
            [half * (1.0 - u.powf(low)).max(0.0).powf(1.0 / low), eye_line - u * length]
        })
        .collect()
}

/// The outline from the eye line down to the chin as `[x, y]` head-local points down the right side: what the sheet
/// plots the five presets against each other with.
fn outline_curve(outline: &HeadOutline) -> Vec<[f64; 2]> {
    (0..=80)
        .map(|k| {
            let y = outline.top + (outline.chin_y() - outline.top) * f64::from(k) / 80.0;
            [half_width_at(outline, y), y]
        })
        .collect()
}

/// `SHAPE_PRESETS_OUT=<dir> cargo test -p omuzaki-char --test shape_presets -- --ignored dump`: each preset's
/// mesh (head-local metres), its target curve and chin and eye-line heights, for the contact sheet.
#[test]
#[ignore = "writes the contact-sheet data"]
fn dump() {
    let dir = std::path::PathBuf::from(std::env::var("SHAPE_PRESETS_OUT").expect("SHAPE_PRESETS_OUT names the folder"));
    std::fs::create_dir_all(&dir).unwrap();
    let origin = HeadParams::default().cranium.centre;
    let heads: Vec<_> = TARGETS
        .iter()
        .map(|t| {
            let p = preset(t.id);
            let params = params_of(p.profile.outline);
            let mesh = build(&params, Resolution::LIVE, None).unwrap();
            let m = measure(p);
            let local = |v: Vec3| {
                [v.x, v.y - origin[1] as f32, v.z - origin[2] as f32].map(|c| (f64::from(c) * 1e5).round() / 1e5)
            };
            serde_json::json!({
                "id": t.id,
                "positions": mesh.positions.iter().map(|v| local(*v)).collect::<Vec<_>>(),
                "indices": mesh.indices,
                "target_curve": target_curve(t, m.face_width, p.profile.outline.top),
                "outline_curve": outline_curve(&p.profile.outline),
                "eye_line": p.profile.outline.top,
                "target_chin": p.profile.outline.top - t.length_to_width * m.face_width,
                "chin": m.chin_y,
                "measured": {
                    "length_to_width": m.face_length_to_width(), "fullness": m.mid_face_fullness,
                    "chin_angle": m.chin_angle_deg, "width_to_height": m.width_to_height(),
                },
                "target": { "length_to_width": t.length_to_width, "fullness": t.fullness, "chin_angle": t.chin },
            })
        })
        .collect();
    std::fs::write(dir.join("heads.json"), serde_json::to_string(&heads).unwrap()).unwrap();
}
