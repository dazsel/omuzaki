//! The 3D eye's paint layer: a mark painted on the skin, the white or the iris lands in its texture exactly where its
//! points say, over the generated detail, and `face.eye3d.paint` adds marks (or replaces them) as one undoable edit,
//! refusing a bad mark by its path.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::eye3d::canvas::Canvas;
use omuzaki_face::eye3d::geometry::{SKIN_HALF, SKIN_TOP};
use omuzaki_face::eye3d::textures::{DISC, SCLERA_REACH};
use omuzaki_face::eye3d::{EyeDesign, skin, textures};
use omuzaki_face_api::FaceProfile;
use omuzaki_face_api::eye3d::{EyeMark, EyeSurface};
use omuzaki_ops_api::{Actor, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

fn run(doc: &MemDoc, name: &str, args: Value) -> Result<Plan, OpError> {
    omuzaki_face::link();
    let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
    let op = (registration.decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

fn dot(on: EyeSurface, at: [f32; 2], size: f32) -> EyeMark {
    EyeMark { on, points: vec![at], size, colour: Colour::hex(0x0000_ff00), opacity: 1.0, softness: 0.0, taper: 0.0 }
}

/// How green the canvas is at pixel `(x, y)`: the marks are pure green, which nothing generated is.
fn green(canvas: &Canvas, x: f32, y: f32) -> f32 {
    let p: Vec3 = canvas.pixels[y as usize * canvas.width as usize + x as usize];
    p.y - p.x.max(p.z)
}

/// The iris half and the white half of the atlas, as canvases of linear colour.
fn halves(design: &EyeDesign, size: u32) -> (Canvas, Canvas) {
    let atlas = textures::atlas(design, size);
    let half = |offset: u32| {
        let mut canvas = Canvas::new(size, size, Vec3::ZERO);
        for y in 0..size {
            for x in 0..size {
                let i = ((y * atlas.width + x + offset) * 4) as usize;
                let c = |k: usize| omuzaki_core::colour::srgb_decode(f32::from(atlas.rgba[i + k]) / 255.0);
                canvas.pixels[(y * size + x) as usize] = Vec3::new(c(0), c(1), c(2));
            }
        }
        canvas
    };
    (half(0), half(size))
}

#[test]
fn a_mark_lands_where_its_points_say_on_each_surface() {
    let design = EyeDesign {
        paint: vec![
            dot(EyeSurface::Skin, [0.016, -0.009], 0.001),
            dot(EyeSurface::Iris, [0.003, 0.001], 0.0006),
            dot(EyeSurface::Sclera, [0.008, -0.002], 0.0006),
        ],
        ..EyeDesign::default()
    };
    let plain = EyeDesign::default();

    let skin = skin::texture(&design, 1024);
    let px_per_m = skin.width as f32 / (2.0 * SKIN_HALF.x);
    let (x, y) = ((0.016 + SKIN_HALF.x) * px_per_m, (SKIN_TOP + 0.009) * px_per_m);
    assert!(green(&skin, x, y) > 0.5, "the skin's dot is at its place");
    assert!(green(&skin, x + 0.002 * px_per_m, y) < 0.1, "and only there");
    assert!(green(&skin::texture(&plain, 1024), x, y) < 0.1, "nothing is painted without a mark");

    let size = 512;
    let (iris, sclera) = halves(&design, size);
    let half = 0.5 * DISC * size as f32;
    let c = 0.5 * size as f32;
    let per_m = half / design.ball.iris_radius;
    assert!(green(&iris, c + 0.003 * per_m, c - 0.001 * per_m) > 0.5, "the iris's dot is at its place");
    assert!(green(&iris, c - 0.003 * per_m, c - 0.001 * per_m) < 0.1, "and not mirrored");
    let off = (0.008_f32.hypot(0.002) / design.ball.radius).asin() / SCLERA_REACH * half;
    let dir = omuzaki_core::glam::Vec2::new(0.008, -0.002).normalize();
    assert!(green(&sclera, c + dir.x * off, c - dir.y * off) > 0.5, "the white's dot is at its place, round the ball");
}

#[test]
fn an_agent_paints_marks_adds_more_replaces_them_and_undo_takes_them_away() {
    let face = serde_json::to_value(FaceProfile::default()).unwrap();
    let mut doc = MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }));
    let before = doc.root().clone();
    let path = DocPath::parse("characters/mira/face").unwrap();
    let marks = |doc: &MemDoc| {
        let face: FaceProfile = serde_json::from_value(doc.get(&path).cloned().unwrap()).unwrap();
        face.eye3d.map_or(0, |e| e.paint.len())
    };
    let freckle = json!({ "on": "skin", "points": [[0.016, -0.009]], "size": 0.0006, "colour": "#6a3a24",
                          "opacity": 0.7, "softness": 0.6 });
    let vessel = json!({ "on": "sclera", "points": [[0.011, 0.0], [0.009, 0.0004], [0.0075, 0.0003]],
                         "size": 0.00008, "colour": "#a03a3a", "opacity": 0.5, "taper": 0.8 });

    let plan = run(&doc, "face.eye3d.paint", json!({ "character": "mira", "marks": [freckle] })).expect("paints");
    assert_eq!(plan.output.as_ref().unwrap()["marks"], json!(1));
    doc.apply(&plan.patch).expect("applies");
    let first = plan.patch;
    let plan = run(&doc, "face.eye3d.paint", json!({ "character": "mira", "marks": [vessel] })).expect("paints");
    doc.apply(&plan.patch).expect("applies");
    assert_eq!(marks(&doc), 2, "a second call adds to the first");
    let second = plan.patch;

    let plan = run(&doc, "face.eye3d.paint", json!({ "character": "mira", "marks": [], "replace": true })).unwrap();
    let mut cleared = doc.clone();
    cleared.apply(&plan.patch).expect("applies");
    assert_eq!(marks(&cleared), 0, "replacing with nothing clears them");

    doc.apply(&second.inverse()).expect("undoes");
    assert_eq!(marks(&doc), 1);
    doc.apply(&first.inverse()).expect("undoes");
    assert_eq!(doc.root(), &before, "undo leaves the face as it was");
}

#[test]
fn a_bad_mark_is_refused_by_its_path() {
    let face = serde_json::to_value(FaceProfile::default()).unwrap();
    let doc = MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }));
    let huge = json!({ "on": "iris", "points": [[0.0, 0.0]], "size": 0.5, "colour": "#000000" });
    let err = run(&doc, "face.eye3d.paint", json!({ "character": "mira", "marks": [huge] })).unwrap_err();
    assert!(err.to_string().contains("paint/0/size"), "{err}");
    let far = json!({ "on": "skin", "points": [[0.3, 0.0]], "size": 0.001, "colour": "#000000" });
    let err = run(&doc, "face.eye3d.paint", json!({ "character": "mira", "marks": [far] })).unwrap_err();
    assert!(err.to_string().contains("paint/0/points"), "{err}");
    let err = run(&doc, "face.eye3d.paint", json!({ "character": "mira", "marks": [] })).unwrap_err();
    assert!(err.to_string().contains("at least one mark"), "{err}");
}
