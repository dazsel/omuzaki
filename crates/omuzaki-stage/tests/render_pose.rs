//! The `pose` argument of the render ops (#224, RG-06): a pose of the pose library by id, mirrored or blended, reaches
//! the picture of `render.preview`, `render.turnaround` and `render.contact_sheet`, and `render.contact_sheet {poses}`
//! lays out a pose library sheet. The mock renderer draws the skinned mock character, so a pose moves its pixels.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "render/support.rs"]
mod support;

use omuzaki_anim::hands::{HandLibrary, HandRest};
use omuzaki_anim::poses::PoseLibrary;
use omuzaki_char_api::{BuildOptions, CharId, CharacterBuilder, Quality as BuildQuality, RegistryBuilder};
use omuzaki_ops_api::{ErrorCode, Op, OpError};
use omuzaki_stage::ops::render::contact_sheet::{ContactSheet, ContactSheetResult};
use omuzaki_stage::ops::render::pose::PoseArg;
use omuzaki_stage::ops::render::preview::{Preview, PreviewResult};
use omuzaki_stage::ops::render::turnaround::{Turnaround, TurnaroundResult};
use omuzaki_store_api::Store;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use support::{Harness, mock_doc};

fn harness() -> Harness {
    Harness::new(mock_doc())
}

fn run<O: Op + DeserializeOwned, R: DeserializeOwned>(harness: &Harness, args: Value) -> Result<R, OpError> {
    let op: O = serde_json::from_value(args).expect("the arguments read");
    let (plan, value) = harness.run(&op)?;
    assert!(plan.patch.is_empty(), "a render op changes nothing in the document");
    Ok(serde_json::from_value(value).expect("the result reads"))
}

fn preview(harness: &Harness, pose: Option<Value>) -> PreviewResult {
    let mut args = json!({ "character": "mock", "size": [64, 96], "framing": "full" });
    if let Some(pose) = pose {
        args["pose"] = pose;
    }
    run::<Preview, _>(harness, args).expect("previews")
}

fn pixels(harness: &Harness, hash: &str) -> Vec<u8> {
    let png = harness.store.get(&hash.parse().unwrap()).unwrap();
    image::load_from_memory(&png).unwrap().to_rgba8().into_raw()
}

#[test]
fn a_mirrored_pose_is_the_library_pose_reflected_and_reaches_the_picture() {
    // The pose the argument resolves to is the library's own mirror of it, worked out here from the files: the
    // authored pose reflected (`PoseDef::mirrored`) and its hand shapes put on the mock's thumbs.
    let built = RegistryBuilder
        .build(
            &CharId::new("mock").unwrap(),
            &omuzaki_char_mock::mock_character().unwrap(),
            &BuildOptions { variant: None, quality: BuildQuality::Offline, rig_export: false },
            &omuzaki_char_api::BuildServices::default(),
        )
        .expect("the mock builds");
    let skeleton = &built.rig.skeleton;
    let rest = HandRest::of(skeleton).unwrap_or_else(|_| HandRest::nominal());
    let library = PoseLibrary::builtin().unwrap();
    let by_hand = library
        .get("wave-right")
        .unwrap()
        .mirrored()
        .unwrap()
        .posture(HandLibrary::builtin().unwrap())
        .unwrap()
        .to_humanoid(&rest);
    let resolved = PoseArg { mirror: true, ..PoseArg::new("wave-right") }.resolve_on("pose", skeleton).unwrap();
    for bone in omuzaki_anim_api::HumanoidBone::ALL {
        let (a, b) = (resolved.get(bone), by_hand.get(bone));
        assert!(a.abs_diff_eq(b, 1e-6), "{bone:?}: {a} vs {b}");
    }

    let harness = harness();
    let sculpted = preview(&harness, None);
    let right = preview(&harness, Some(json!({ "id": "wave-right" })));
    let mirrored = preview(&harness, Some(json!({ "id": "wave-right", "mirror": true })));
    let left = preview(&harness, Some(json!({ "id": "wave-left" })));
    assert_ne!(sculpted.hash, right.hash, "the pose reaches the picture");
    assert_ne!(right.hash, mirrored.hash, "and so does the mirroring");
    // `wave-left` is authored by hand as the mirror of `wave-right`: the mirrored picture is that pose's, to the last
    // few pixels where the authored degrees round differently from the reflected quaternions.
    let (a, b) = (pixels(&harness, &mirrored.hash), pixels(&harness, &left.hash));
    let differ = a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0).filter(|(p, q)| p != q).count();
    assert!(differ * 100 <= a.len() / 4, "{differ} of {} pixels differ", a.len() / 4);
    assert_eq!(
        preview(&harness, Some(json!({ "id": "wave-right", "mirror": true }))),
        mirrored,
        "the same bytes again"
    );
}

#[test]
fn an_unknown_pose_is_refused_with_the_nearest_id_and_a_bad_weight_is_refused() {
    let harness = harness();
    let refused = |pose: Value| {
        run::<Preview, PreviewResult>(&harness, json!({ "character": "mock", "size": [32, 32], "pose": pose }))
            .expect_err("refused")
    };
    let unknown = refused(json!({ "id": "wave-rite" }));
    assert_eq!((unknown.code, unknown.field.as_deref()), (ErrorCode::NotFound, Some("pose/id")));
    assert!(unknown.hint.as_deref().is_some_and(|h| h.contains("did you mean 'wave-right'")), "{unknown:?}");
    let blend = refused(json!({ "id": "wave-right", "blend": { "with": "wave-lft", "weight": 0.5 } }));
    assert_eq!(blend.field.as_deref(), Some("pose/blend/with"));
    let weight = refused(json!({ "id": "wave-right", "blend": { "with": "wave-left", "weight": -0.1 } }));
    assert_eq!((weight.code, weight.field.as_deref()), (ErrorCode::Invalid, Some("pose/blend/weight")));
    let sheet = run::<ContactSheet, ContactSheetResult>(
        &harness,
        json!({ "character": "mock", "poses": [{ "id": "wave-right" }, { "id": "nope" }], "cell": [32, 32] }),
    )
    .expect_err("refused");
    assert_eq!(sheet.field.as_deref(), Some("poses/1/id"));
}

#[test]
fn the_pose_sheet_renders_one_labelled_cell_per_pose_the_same_every_run() {
    let harness = harness();
    let args = json!({
        "character": "mock",
        "poses": [
            { "id": "wave-right" },
            { "id": "wave-right", "mirror": true },
            { "id": "wave-right", "blend": { "with": "wave-left", "weight": 0.5 } }
        ],
        "cell": [48, 64],
        "labels": false
    });
    let sheet: ContactSheetResult = run::<ContactSheet, _>(&harness, args.clone()).unwrap();
    assert_eq!(sheet.cells, ["wave-right", "wave-right mirrored", "wave-right + wave-left 0.50"]);
    assert_eq!((sheet.columns, sheet.rows), (2, 2));
    let image = image::load_from_memory(&harness.store.get(&sheet.hash.parse().unwrap()).unwrap()).unwrap().to_rgba8();
    let cell = |i: u32| {
        let (x, y) = (8 + (i % 2) * (48 + 8), 8 + (i / 2) * (64 + 8));
        image::imageops::crop_imm(&image, x, y, 48, 64).to_image().into_raw()
    };
    assert_ne!(cell(0), cell(1));
    assert_ne!(cell(0), cell(2));
    assert_ne!(cell(1), cell(2));
    assert_eq!(run::<ContactSheet, ContactSheetResult>(&harness, args).unwrap(), sheet, "the same bytes every run");
    // A shared pose on an expression or variant sheet, and none where each cell has its own.
    let posed: ContactSheetResult = run::<ContactSheet, _>(
        &harness,
        json!({ "character": "mock", "variants": ["game", "close-up"], "pose": { "id": "wave-left" }, "cell": [48, 64] }),
    )
    .unwrap();
    let plain: ContactSheetResult = run::<ContactSheet, _>(
        &harness,
        json!({ "character": "mock", "variants": ["game", "close-up"], "cell": [48, 64] }),
    )
    .unwrap();
    assert_ne!(posed.hash, plain.hash);
    let both = run::<ContactSheet, ContactSheetResult>(
        &harness,
        json!({ "character": "mock", "poses": [{ "id": "wave-left" }], "pose": { "id": "wave-left" } }),
    )
    .expect_err("refused");
    assert_eq!(both.field.as_deref(), Some("pose"));
}

#[test]
fn a_turnaround_shows_the_pose_on_every_view() {
    let harness = harness();
    let row = |pose: Value| -> TurnaroundResult {
        let mut args = json!({ "character": "mock", "size": [48, 80], "quality": "live" });
        if !pose.is_null() {
            args["pose"] = pose;
        }
        run::<Turnaround, _>(&harness, args).unwrap()
    };
    let (plain, posed) = (row(Value::Null), row(json!({ "id": "wave-right" })));
    let (a, b) = (pixels(&harness, &plain.hash), pixels(&harness, &posed.hash));
    let width = (plain.size[0] * 4) as usize;
    for view in 0..4 {
        let columns = view * (48 + 8) * 4..(view * (48 + 8) + 48) * 4;
        let differs =
            a.chunks_exact(width).zip(b.chunks_exact(width)).any(|(p, q)| p[columns.clone()] != q[columns.clone()]);
        assert!(differs, "view {view} shows the pose");
    }
}
