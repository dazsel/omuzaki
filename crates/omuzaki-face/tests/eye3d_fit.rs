//! Fitting the 3D eye to landmarks (`omuzaki_face::eye3d::fit`, the op `face.eye3d.fit`): landmarks projected from a
//! known eye and shot are fitted back to within a few pixels from a different start, the op writes the fitted design
//! as one undoable edit and returns the shot for `render.eye`, and bad landmarks or shots are refused with their field.
//! `face.eye3d.preset` gives the eye a named shape and iris colour in one undoable edit.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::glam::Vec2;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::eye3d::EyeDesign;
use omuzaki_face::eye3d::closeup::{Lamp, Shot, View};
use omuzaki_face::eye3d::fit::{Circle, Landmarks, fit, project, residuals};
use omuzaki_face_api::FaceProfile;
use omuzaki_face_api::eye3d::{EyeShape, IrisColour};
use omuzaki_ops_api::{Actor, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

const SIZE: [u32; 2] = [1024, 744];

fn run(doc: &MemDoc, name: &str, args: Value) -> Result<Plan, OpError> {
    omuzaki_face::link();
    let registration = find(name).unwrap_or_else(|| panic!("{name} is not registered"));
    let op = (registration.decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

/// An eye a little more open than the default, its outer corner lifted, shot from the temple's side further round than
/// the macro view: what the fit should find. Its lids rest on the eyeball, as a real eye's do.
fn target() -> (EyeDesign, Shot) {
    let mut design = EyeDesign::default();
    design.aperture.outer_y = 0.000_6;
    design.aperture.upper = 0.006_2;
    design.aperture.lower = 0.004_2;
    design.ball.iris_radius = 0.006_1;
    design.ball.pupil = 0.3;
    let mut shot = View::Macro.shot();
    shot.camera = [-0.033, -0.011, 0.051];
    shot.roll = -16.0;
    shot.gaze = [-0.12, 0.1, 0.95];
    (design, shot)
}

/// The eyeball's edge on the nose's side, where a view from the temple's side sees past the lids to it: the nine
/// points of its outline furthest toward the picture's right.
fn nose_side(ball: &[Vec2]) -> Vec<[f32; 2]> {
    let mut points: Vec<Vec2> = ball.to_vec();
    points.sort_by(|a, b| b.x.total_cmp(&a.x));
    points.iter().take(9).map(|q| [q.x, q.y]).collect()
}

/// The landmarks a person would measure on a picture of `design` taken as `shot` takes it.
fn landmarks(design: &EyeDesign, shot: &Shot) -> Landmarks {
    let p = project(design, shot, SIZE);
    Landmarks {
        image: SIZE,
        outline: p.outline.iter().step_by(2).map(|v| [v.x, v.y]).collect(),
        iris: Circle { centre: p.iris.0.into(), radius: p.iris.1 },
        pupil: Some(Circle { centre: p.pupil.0.into(), radius: p.pupil.1 }),
        ball_edge: nose_side(&p.ball),
    }
}

#[test]
fn the_fit_finds_a_known_eye_from_its_landmarks() {
    let (design, shot) = target();
    let marks = landmarks(&design, &shot);
    let exact = residuals(&design, &shot, &marks);
    assert!(exact.outline < 3.0, "the eye that made the landmarks fits them: {exact:?}");
    assert!(exact.off_ball < 1.0, "and rests on its eyeball: {exact:?}");
    assert!(
        !marks.ball_edge.is_empty() && exact.ball_edge < 0.5,
        "its eyeball's edge is where it was measured: {} points, {exact:?}",
        marks.ball_edge.len()
    );

    let fitted = fit(&EyeDesign::default(), &View::Macro.shot(), &marks, 300);
    let (before, after) = (&fitted.before, &fitted.residuals);
    assert!(before.outline > 10.0, "the start is well off: {before:?}");
    assert!(after.outline < 3.0, "the outline fits within a few pixels: {after:?}");
    assert!(after.iris_centre < 4.0 && after.iris_radius < 3.0, "the iris fits: {after:?}");
    assert!(after.pupil_radius < 3.0, "the pupil fits: {after:?}");
    assert!(after.ball_edge < 2.0, "the eyeball's edge fits: {after:?}");
    assert!(fitted.shot.check().is_ok(), "the fitted shot is one render.eye takes");
}

#[test]
fn the_op_fits_a_characters_eye_as_one_undoable_edit() {
    let face = serde_json::to_value(FaceProfile::default()).unwrap();
    let mut doc = MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }));
    let before = doc.root().clone();
    let (design, shot) = target();
    let marks = serde_json::to_value(landmarks(&design, &shot)).unwrap();
    assert!(marks["ball_edge"].as_array().is_some_and(|edge| !edge.is_empty()), "an agent sends the ball's edge too");

    let plan = run(&doc, "face.eye3d.fit", json!({ "character": "mira", "landmarks": marks })).expect("fits");
    let output = plan.output.clone().expect("the fit answers");
    assert_eq!(output["changed"], json!(["characters/mira/face/eye3d"]), "one edit, of the 3D eye");
    let outline = output["residuals"]["outline"].as_f64().unwrap();
    assert!(outline < 3.0, "the outline fits within a few pixels: {output}");
    let fitted: Shot = serde_json::from_value(output["shot"].clone()).expect("a shot");
    assert!(fitted.check().is_ok());

    doc.apply(&plan.patch).expect("applies");
    let path = DocPath::parse("characters/mira/face").unwrap();
    let face: FaceProfile = serde_json::from_value(doc.get(&path).cloned().unwrap()).unwrap();
    let eye = face.eye3d.expect("the face now has a fitted 3D eye");
    // One picture can't tell a larger eye from a nearer camera, so the sizes are only as good as the picture's
    // outlines; what holds is an eye that builds, and outlines that land.
    assert!(eye.ball.iris_radius < 0.7 * eye.ball.radius, "{:?}", eye.ball);
    assert!(output["residuals"]["ball_edge"].as_f64().unwrap() < 2.0, "the eyeball's edge lands: {output}");
    assert_eq!(eye.iris, EyeDesign::default().iris, "only the opening, the iris's size and the pupil change");

    doc.apply(&plan.patch.inverse()).expect("undoes");
    assert_eq!(doc.root(), &before, "undo leaves the face as it was");
}

#[test]
fn bad_landmarks_and_shots_are_refused_with_their_field() {
    let face = serde_json::to_value(FaceProfile::default()).unwrap();
    let doc = MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }));
    let (design, shot) = target();
    let mut marks = landmarks(&design, &shot);
    marks.outline.truncate(5);
    let err = run(&doc, "face.eye3d.fit", json!({ "character": "mira", "landmarks": marks })).unwrap_err();
    assert!(err.to_string().contains("outline"), "{err}");

    let marks = landmarks(&design, &shot);
    let mut start = shot.clone();
    start.fov = 170.0;
    let err = run(&doc, "face.eye3d.fit", json!({ "character": "mira", "landmarks": marks, "start": start }));
    assert!(err.unwrap_err().to_string().contains("fov"));

    let mut bad = shot;
    bad.gaze = [0.0, 0.0, -1.0];
    assert!(bad.check().is_err(), "an eye can't look into its own head");
    assert!(View::Macro.shot().check().is_ok() && View::Front.shot().check().is_ok());

    // The light rides on the shot, and its numbers are checked with it.
    let mut lit = View::Macro.shot();
    lit.light.lamp = Some(Lamp { position: [0.03, -0.03, 0.02], intensity: 1.0, reach: 0.06 });
    assert!(lit.check().is_ok());
    let json = serde_json::to_value(&lit).unwrap();
    assert_eq!(serde_json::from_value::<Shot>(json).unwrap(), lit, "a lit shot reads back as it was written");
    lit.light.lamp = Some(Lamp { position: [0.03, -0.03, 0.02], intensity: 12.0, reach: 0.06 });
    assert!(lit.check().unwrap_err().contains("lamp"));
    lit.light.lamp = None;
    lit.light.key = [0.0; 3];
    assert!(lit.check().unwrap_err().contains("key"));
}

#[test]
fn a_named_shape_and_iris_colour_set_the_eye_in_one_undoable_edit() {
    let face = serde_json::to_value(FaceProfile::default()).unwrap();
    let mut doc = MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }));
    let before = doc.root().clone();
    let plan = run(&doc, "face.eye3d.preset", json!({ "character": "mira", "shape": "wide", "iris": "green" }))
        .expect("plans");
    doc.apply(&plan.patch).expect("applies");
    let path = DocPath::parse("characters/mira/face").unwrap();
    let face: FaceProfile = serde_json::from_value(doc.get(&path).cloned().unwrap()).unwrap();
    let eye = face.eye3d.expect("a 3D eye");
    let mut want = EyeDesign::default();
    EyeShape::Wide.apply(&mut want);
    IrisColour::Green.apply(&mut want);
    assert_eq!(eye, want, "the shape and the colour, every other field its default");
    assert!(eye.aperture.upper > EyeDesign::default().aperture.upper, "a wide eye opens further");

    doc.apply(&plan.patch.inverse()).expect("undoes");
    assert_eq!(doc.root(), &before);
    let err = run(&doc, "face.eye3d.preset", json!({ "character": "mira" })).unwrap_err();
    assert!(err.to_string().contains("shape"), "{err}");
    assert!(run(&doc, "face.eye3d.preset", json!({ "character": "mira", "shape": "huge" })).is_err());
}
