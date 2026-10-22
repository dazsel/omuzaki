//! `render.preview`'s face landmarks and named render presets (#376, `docs/design/likeness-measures.md` D10): a kit
//! character's eyes, mouth and chin are reported in pixels of the picture, and `preset: "measure"` gives the measurement
//! look from library data. On the mock renderer the geometry of the points is checked (front-on, level, in order, large
//! enough, the same bytes every run); on lavapipe the points are checked against the painted face itself.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

#[path = "render/support.rs"]
mod support;

use omuzaki_char::ops::character::create::CharacterCreate;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError};
use omuzaki_stage::ops::render::preview::{Preview, PreviewResult, RenderedFace};
use serde_json::{Value, json};

use support::{Harness, TestDoc};

/// The kit's two templates, each as a character of its own name.
const TEMPLATES: [&str; 2] = ["standard", "mini"];

/// A project with one character per kit template.
fn kit_doc() -> MemDoc {
    let clock = FakeClock::new(Seconds(0.0));
    let actor = Actor::Person { name: "test".to_owned() };
    let mut doc = MemDoc::from_value(json!({}));
    for template in TEMPLATES {
        let op: CharacterCreate =
            serde_json::from_value(json!({ "id": template, "name": template, "template": template, "seed": 7 }))
                .unwrap();
        let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap();
        doc.apply(&plan.patch).unwrap();
    }
    doc
}

fn run(harness: &Harness, args: Value) -> Result<PreviewResult, OpError> {
    let op: Preview = serde_json::from_value(args).expect("the arguments read");
    let (_, value) = harness.run(&op)?;
    Ok(serde_json::from_value(value).expect("the result reads"))
}

fn iod(face: &RenderedFace) -> f64 {
    (face.eye_left[0] - face.eye_right[0]).hypot(face.eye_left[1] - face.eye_right[1])
}

/// The one face of a preview, checked to be a front view: the right eye on the picture's left, the eyes level, the
/// mouth between them and below, the chin below the mouth.
fn front_face(result: &PreviewResult, character: &str) -> RenderedFace {
    let [face] = result.face_landmarks.as_slice() else {
        panic!("{character}: one face in the picture, got {:?}", result.face_landmarks)
    };
    assert_eq!((face.character.as_str(), face.source.as_str()), (character, "render"));
    let iod = iod(face);
    assert!(face.eye_right[0] < face.eye_left[0], "{character}: the right eye is on the picture's left: {face:?}");
    assert!((face.eye_right[1] - face.eye_left[1]).abs() < 0.01 * iod, "{character}: the eyes are level: {face:?}");
    let centre = face.eye_right[0].midpoint(face.eye_left[0]);
    assert!((face.mouth[0] - centre).abs() < 0.01 * iod, "{character}: the mouth is under the eyes' middle: {face:?}");
    assert!((face.chin[0] - centre).abs() < 0.01 * iod, "{character}: and so is the chin: {face:?}");
    assert!(face.eye_right[1] < face.mouth[1] && face.mouth[1] < face.chin[1], "{character}: top to bottom: {face:?}");
    face.clone()
}

#[test]
fn the_measure_preset_reports_a_front_face_large_enough_to_measure_and_the_same_bytes_every_run() {
    let harness = Harness::new(TestDoc(kit_doc()));
    for character in TEMPLATES {
        let args = json!({ "character": character, "preset": "measure", "size": [768, 768] });
        let first = run(&harness, args.clone()).unwrap_or_else(|e| panic!("{character}: {e}"));
        let face = front_face(&first, character);
        println!("{character}: {face:?}, IOD {:.1} px", iod(&face));
        assert!(iod(&face) >= 160.0, "{character}: D10 asks for at least 160 px between the eyes: {face:?}");
        let again = run(&harness, args).unwrap();
        assert_eq!(first, again, "{character}: the same document and arguments give the same picture and points");
    }
}

#[test]
fn a_preset_is_refused_when_unknown_when_an_argument_it_fixes_is_given_or_when_the_face_is_too_small() {
    let harness = Harness::new(TestDoc(kit_doc()));
    let refused = |args: Value| run(&harness, args).expect_err("refused");
    let unknown = refused(json!({ "character": "standard", "preset": "glamour" }));
    assert_eq!((unknown.code, unknown.field.as_deref()), (ErrorCode::Invalid, Some("preset")));
    assert!(unknown.hint.as_deref().is_some_and(|h| h.contains("measure")), "it lists the presets: {unknown:?}");
    for (field, value) in [("framing", json!("bust")), ("light_rig", json!("dev-studio")), ("yaw", json!(30.0))] {
        let mut args = json!({ "character": "standard", "preset": "measure" });
        args[field] = value;
        let error = refused(args);
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some(field)), "{error:?}");
    }
    let small = refused(json!({ "character": "standard", "preset": "measure", "size": [256, 256] }));
    assert_eq!((small.code, small.field.as_deref()), (ErrorCode::Invalid, Some("size")), "{small:?}");
}

#[test]
fn a_face_turned_away_from_the_camera_reports_no_landmarks() {
    let harness = Harness::new(TestDoc(kit_doc()));
    let shot = |yaw: f32| {
        run(&harness, json!({ "character": "standard", "framing": "face", "yaw": yaw, "size": [256, 256] })).unwrap()
    };
    front_face(&shot(0.0), "standard");
    assert_eq!(shot(180.0).face_landmarks, Vec::new(), "the back of the head has no face to report");
    let turned = shot(30.0);
    assert_eq!(turned.face_landmarks.len(), 1, "a three-quarter view still shows the face");
}

/// The head-shape profiles blended from `round-u` toward `egg-oval` in five steps (outline and layout together, as
/// `ShapeProfiles::blend` gives them): the rendered eye-to-chin drop over the IOD moves one way at every step.
#[test]
fn blending_round_u_toward_egg_oval_moves_the_rendered_drop_one_way() {
    use omuzaki_char::head::presets::ShapeProfiles;

    let mut drops = Vec::new();
    for step in 0..5 {
        let profile = ShapeProfiles::blend("round-u", "egg-oval", f64::from(step) / 4.0).unwrap();
        let mut root = kit_doc().root().clone();
        let node = &mut root["characters"]["standard"];
        node["parts"]["head"]["params"]["outline"] = serde_json::to_value(profile.outline).unwrap();
        node["face"]["layout"] = serde_json::to_value(profile.layout).unwrap();
        let harness = Harness::new(TestDoc(MemDoc::from_value(root)));
        let result =
            run(&harness, json!({ "character": "standard", "preset": "measure", "size": [768, 768] })).unwrap();
        let face = front_face(&result, "standard");
        drops.push((face.chin[1] - face.eye_right[1].midpoint(face.eye_left[1])) / iod(&face));
    }
    println!("drop over IOD, round-u to egg-oval: {drops:.4?}");
    let steps: Vec<f64> = drops.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        steps.iter().all(|s| *s > 1e-4) || steps.iter().all(|s| *s < -1e-4),
        "the drop moves one way at every step: {drops:?}"
    );
}

/// On lavapipe, for both templates under the `measure` preset at 768 px: the eye points are on the painted eyes (far
/// from the skin between them), the mouth point is on the mouth line, and the chin point is at the bottom of the dark
/// line the preset draws around the jaw. Set `OMUZAKI_EVIDENCE_DIR` to keep the renders with the points marked.
#[cfg(feature = "wgpu")]
#[test]
#[allow(clippy::disallowed_methods, reason = "the evidence images are written beside the test, not by an op")]
fn on_the_gpu_the_points_land_on_the_painted_face() {
    use omuzaki_measure::colour::lab;
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    use omuzaki_store_api::Store;

    const SIDE: u32 = 768;
    let renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let harness = Harness::on(TestDoc(kit_doc()), Box::new(renderer));
    for character in TEMPLATES {
        let result =
            run(&harness, json!({ "character": character, "preset": "measure", "size": [SIDE, SIDE] })).unwrap();
        let face = front_face(&result, character);
        let png = harness.store.get(&result.hash.parse().unwrap()).unwrap();
        let mut picture = image::load_from_memory(&png).unwrap().to_rgba8();
        let at = |x: f64, y: f64| {
            let px = picture.get_pixel(x.floor() as u32, y.floor() as u32);
            lab([px[0], px[1], px[2]])
        };
        let distance =
            |a: [f64; 3], b: [f64; 3]| ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt();
        let iod = iod(&face);
        // The skin between the eyes, on the eye line.
        let skin = at(face.eye_right[0].midpoint(face.eye_left[0]), face.eye_right[1]);
        let eyes = [face.eye_right, face.eye_left].map(|eye| distance(at(eye[0], eye[1]), skin));
        // The darkest pixel within 5% of the IOD above or below a point, and how far from skin the farthest one is.
        let column = |point: [f64; 2]| {
            let reach = (0.05 * iod).ceil() as i32;
            (-reach..=reach)
                .map(|dy| at(point[0], point[1] + f64::from(dy)))
                .fold((f64::MAX, 0.0_f64), |(dark, far), c| (dark.min(c[0]), far.max(distance(c, skin))))
        };
        let (_, mouth) = column(face.mouth);
        // The jaw's line ends at the chin from both sides: under the chin itself the neck hides it (the flat rig casts
        // no shadow there), so its lowest dark pixel near the chin is a little above the chin point. The window stops
        // short of the shoulders, which the mini template brings up close under the chin.
        let reach = (0.2 * iod) as i32;
        let (top, bottom) = ((face.chin[1] - 0.1 * iod) as i32, (face.chin[1] + 0.02 * iod) as i32);
        let jaw_bottom = (-reach..=reach)
            .flat_map(|dx| (top..=bottom).filter(move |y| at(face.chin[0] + f64::from(dx), f64::from(*y))[0] < 60.0))
            .max()
            .map(f64::from);
        println!(
            "{character}: skin L* {:.1}; eye centres {:.1} and {:.1} from it; the mouth reaches {mouth:.1}; the jaw \
             line's lowest pixel at y {jaw_bottom:?}",
            skin[0], eyes[0], eyes[1]
        );
        if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
            for point in [face.eye_right, face.eye_left, face.mouth, face.chin] {
                for d in -6_i32..=6 {
                    let (x, y) = (point[0] as i32, point[1] as i32);
                    for (px, py) in [(x + d, y), (x, y + d)] {
                        if (0..SIDE.cast_signed()).contains(&px) && (0..SIDE.cast_signed()).contains(&py) {
                            picture.put_pixel(px as u32, py as u32, image::Rgba([255, 0, 255, 255]));
                        }
                    }
                }
            }
            picture.save(std::path::Path::new(&dir).join(format!("render-landmarks-{character}.png"))).unwrap();
        }
        assert!(eyes.iter().all(|d| *d > 20.0), "{character}: the eye points are on the eyes, not skin: {eyes:?}");
        assert!(mouth > 20.0, "{character}: the mouth point is on the mouth line ({mouth:.1} from skin)");
        let above = face.chin[1] - jaw_bottom.expect("the jaw's line is drawn") - 0.5;
        assert!(
            (-0.02 * iod..0.04 * iod).contains(&above),
            "{character}: the chin point is where the jaw's line ends ({above:.1} px below its lowest pixel)"
        );
    }
}
