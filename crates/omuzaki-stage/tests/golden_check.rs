//! `render.golden.check` (#231, VF-03): after an edit, the mock character's blessed variants are rendered again and
//! held to their goldens. An edit to the close-up's own overrides leaves the game face as it was; an edit to a part
//! every variant shares fails the game face, with a heatmap of where it changed, unless the edit says it meant to.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "render/support.rs"]
mod support;

use omuzaki_doc_api::{DocPath, DocRead, DocWrite, Patch};
use omuzaki_ops_api::ErrorCode;
use omuzaki_stage::ops::golden::bless::GoldenBless;
use omuzaki_stage::ops::golden::check::{GoldenCheck, GoldenCheckResult, Outcome, VariantReport};
use omuzaki_store_api::Store;
use serde_json::{Value, json};

use support::{Harness, mock_doc};

/// The mock character blessed in one small full-body shot.
fn blessed() -> Harness {
    let mut harness = Harness::new(mock_doc());
    let op: GoldenBless = serde_json::from_value(json!({
        "character": "mock",
        "scenes": { "front": { "framing": "full", "size": [64, 96] } }
    }))
    .unwrap();
    harness.run_to_end(&op).expect("blessed");
    harness
}

/// Sets `path` to `value` in the document, as an edit op would.
fn edit(harness: &mut Harness, path: &str, value: Value) {
    let path = DocPath::parse(path).unwrap();
    let old = harness.doc().get(&path).cloned();
    harness.doc_mut().apply(&Patch::new().with(path, old, Some(value))).unwrap();
}

fn check(harness: &Harness, args: Value) -> (Value, GoldenCheckResult) {
    let op: GoldenCheck = serde_json::from_value(args).unwrap();
    let (plan, value) = harness.run(&op).expect("checks");
    (plan.output.expect("the dry run says what it renders"), serde_json::from_value(value).unwrap())
}

fn variant<'a>(result: &'a GoldenCheckResult, name: &str) -> &'a VariantReport {
    result.variants.iter().find(|v| v.variant == name).expect("reported")
}

fn outcome(result: &GoldenCheckResult, name: &str) -> Outcome {
    variant(result, name).scenes[0].outcome
}

#[test]
fn with_nothing_edited_nothing_is_rendered_and_forcing_it_renders_the_same_bytes() {
    let harness = blessed();
    let (dry, quiet) = check(&harness, json!({ "character": "mock" }));
    assert_eq!(dry["renders"], 0);
    assert!(quiet.passed && quiet.renders == 0, "{quiet:?}");
    assert!(quiet.variants.iter().all(|v| v.scenes[0].outcome == Outcome::Unchanged));
    let (dry, forced) = check(&harness, json!({ "character": "mock", "rerender": true }));
    assert_eq!(dry["renders"], 3, "the dry run counts the renders");
    assert!(forced.passed && forced.renders == 3, "{forced:?}");
    assert!(forced.variants.iter().all(|v| v.scenes[0].outcome == Outcome::Same), "{forced:?}");
}

#[test]
fn an_edit_to_the_close_up_alone_leaves_the_game_face_byte_identical() {
    let mut harness = blessed();
    // The close-up loses its bangs: an override only it has.
    let overrides = "characters/mock/variants/close-up/overrides";
    let mut value = harness.doc().get(&DocPath::parse(overrides).unwrap()).cloned().unwrap();
    value.as_object_mut().unwrap().remove("parts/hair/params/groups/b-bangs");
    edit(&mut harness, overrides, value);

    let (dry, result) = check(&harness, json!({ "character": "mock", "intended": ["close-up"] }));
    assert_eq!((dry["renders"].clone(), dry["variants"].clone()), (json!(1), json!(["close-up"])));
    assert!(result.passed, "{result:?}");
    assert_eq!(outcome(&result, "game"), Outcome::Unchanged, "the game face's definition is untouched");
    assert_eq!(outcome(&result, "base"), Outcome::Unchanged);
    assert_eq!(outcome(&result, "close-up"), Outcome::Intended, "the close-up changed, as meant");
    // Rendered anyway, the game face is the golden byte for byte.
    let (_, forced) = check(&harness, json!({ "character": "mock", "intended": ["close-up"], "rerender": true }));
    assert_eq!(outcome(&forced, "game"), Outcome::Same);
    assert!(forced.passed);
    // Without saying so, the close-up's change fails the check.
    let (_, unsaid) = check(&harness, json!({ "character": "mock" }));
    assert!(!unsaid.passed);
    assert_eq!(outcome(&unsaid, "close-up"), Outcome::Changed);
}

#[test]
fn an_edit_every_variant_shares_fails_the_game_face_with_a_heatmap_unless_it_is_intended() {
    let mut harness = blessed();
    // The vest every variant wears hangs looser.
    edit(&mut harness, "characters/mock/parts/vest/params/ease", json!(0.05));
    let (dry, result) = check(&harness, json!({ "character": "mock" }));
    assert_eq!(dry["renders"], 3, "every variant's definition changed");
    assert!(!result.passed, "{result:?}");
    let game = &variant(&result, "game").scenes[0];
    assert_eq!(game.outcome, Outcome::Changed);
    assert!(game.changed > 0.0 && game.max_difference > 0, "{game:?}");
    let bounds = game.bounds.expect("the change is boxed");
    // The heatmap is stored, the golden's size, red where the change is.
    let heat = game.heatmap.as_deref().expect("a heatmap");
    let hash = &heat["omuzaki://render/".len()..heat.len() - 4];
    let image = image::load_from_memory(&harness.store.get(&hash.parse().unwrap()).unwrap()).unwrap().to_rgba8();
    assert_eq!(image.dimensions(), (64, 96));
    let red = |x: u32, y: u32| {
        let p = image.get_pixel(x, y).0;
        p[0] == 255 && p[2] == 0 && p[1] < 230
    };
    let [x, y, w, h] = bounds;
    assert!((x..x + w).any(|x| (y..y + h).any(|y| red(x, y))), "the changed pixels are coloured in the heatmap");
    assert!(!red(0, 0) || (x == 0 && y == 0), "and an unchanged corner is not");
    assert!(game.diff.is_some() && game.render.is_some());

    // Said to be meant for the game face, the same change is reported and passes.
    let (_, meant) = check(&harness, json!({ "character": "mock", "intended": ["game", "base", "close-up"] }));
    assert!(meant.passed, "{meant:?}");
    assert_eq!(outcome(&meant, "game"), Outcome::Intended);
}

#[test]
fn a_check_without_goldens_or_with_an_unknown_variant_is_refused() {
    let harness = Harness::new(mock_doc());
    let op: GoldenCheck = serde_json::from_value(json!({ "character": "mock" })).unwrap();
    assert_eq!(harness.run(&op).expect_err("no goldens").code, ErrorCode::NotFound);
    let harness = blessed();
    let op: GoldenCheck = serde_json::from_value(json!({ "character": "mock", "intended": ["nope"] })).unwrap();
    let error = harness.run(&op).expect_err("refused");
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::NotFound, Some("intended/0")));
}

/// The shared-part edit on lavapipe: the game face fails with its heatmap, written beside the goldens as evidence.
#[cfg(feature = "wgpu")]
#[test]
#[allow(clippy::disallowed_methods, reason = "the evidence images are written beside the test, not by an op")]
fn on_the_gpu_a_shared_edit_fails_the_game_face_where_it_changed() {
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    let renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let mut harness = Harness::on(mock_doc(), Box::new(renderer));
    let op: GoldenBless = serde_json::from_value(json!({
        "character": "mock",
        "scenes": { "front": { "framing": "bust", "size": [192, 192] } }
    }))
    .unwrap();
    harness.run_to_end(&op).expect("blessed");
    let (_, steady) = check(&harness, json!({ "character": "mock", "rerender": true }));
    assert!(steady.passed, "a re-render on the GPU holds: {steady:?}");
    edit(&mut harness, "characters/mock/parts/vest/params/ease", json!(0.05));
    let (_, result) = check(&harness, json!({ "character": "mock" }));
    let game = &variant(&result, "game").scenes[0];
    println!("game: {:.3} of the pixels changed, at most {}, in {:?}", game.changed, game.max_difference, game.bounds);
    assert_eq!(game.outcome, Outcome::Changed);
    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        let dir = std::path::PathBuf::from(dir);
        for (name, uri) in [
            ("before", game.golden.as_ref().unwrap()),
            ("after", game.render.as_ref().unwrap()),
            ("heatmap", game.heatmap.as_ref().unwrap()),
        ] {
            let hash = &uri["omuzaki://render/".len()..uri.len() - 4];
            std::fs::write(
                dir.join(format!("golden-check-{name}.png")),
                harness.store.get(&hash.parse().unwrap()).unwrap(),
            )
            .unwrap();
        }
    }
}
