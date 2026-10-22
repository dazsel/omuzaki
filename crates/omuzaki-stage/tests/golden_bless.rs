//! `render.golden.bless` (#230, VF-03): the mock character's base and its `game` and `close-up` variants rendered in the
//! manifest's shots and recorded at `goldens/mock`. The mock renderer gives the same bytes every run.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "render/support.rs"]
mod support;

use omuzaki_char_api::CharId;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocRead;
use omuzaki_ops_api::{Actor, ErrorCode, Op, OpCtx, OpError};
use omuzaki_stage::ops::golden::bless::{GoldenBless, GoldenBlessResult};
use omuzaki_stage::ops::golden::manifest::{GoldenManifest, manifest_path};
use omuzaki_store_api::Store;
use serde_json::{Value, json};

use support::{Harness, mock_doc};

fn bless(harness: &mut Harness, args: Value) -> Result<GoldenBlessResult, OpError> {
    let op: GoldenBless = serde_json::from_value(args).expect("the arguments read");
    let (_, finished) = harness.run_to_end(&op)?;
    Ok(serde_json::from_value(finished.output.expect("a result")).expect("the result reads"))
}

fn small() -> Value {
    json!({
        "front": { "framing": "full", "size": [48, 64] },
        "face": { "framing": "face", "size": [32, 32], "tolerance": { "per_channel": 4, "max_changed": 0.01 } }
    })
}

fn manifest(harness: &Harness) -> GoldenManifest {
    GoldenManifest::read(harness.doc(), &CharId::new("mock").unwrap()).unwrap().expect("a manifest")
}

#[test]
fn blessing_records_every_variant_in_every_scene_and_twice_gives_the_same_manifest() {
    let mut harness = Harness::new(mock_doc());
    let first = bless(&mut harness, json!({ "character": "mock", "scenes": small() })).unwrap();
    assert_eq!(first.blessed, ["base", "close-up", "game"]);
    assert_eq!(first.renders, 6);
    let recorded = manifest(&harness);
    assert_eq!(recorded, first.manifest, "the manifest in the document is the one returned");
    assert_eq!(recorded.variants.keys().collect::<Vec<_>>(), ["base", "close-up", "game"]);
    // The game face (the variant for the widest shots) is pixel-exact; the others compare within tolerance.
    let exact: Vec<bool> = recorded.variants.values().map(|v| v.pixel_exact).collect();
    assert_eq!(exact, [false, false, true]);
    for (name, golden) in &recorded.variants {
        assert_eq!(golden.images.keys().collect::<Vec<_>>(), ["face", "front"], "{name}");
        for hash in golden.images.values() {
            assert!(harness.store.get(&hash.parse().unwrap()).is_ok(), "{name}: the golden is stored");
        }
    }
    // The close-up's bangs make it a different picture; the game face adds nothing to the base.
    let image = |variant: &str| recorded.variants[variant].images["front"].clone();
    assert_ne!(image("close-up"), image("game"));

    // Blessing again renders the same bytes: the same manifest, and nothing to change in the document.
    let op: GoldenBless = serde_json::from_value(json!({ "character": "mock" })).unwrap();
    let (_, again) = harness.run_to_end(&op).unwrap();
    assert!(again.patch.is_empty(), "a second bless changes nothing");
    assert_eq!(manifest(&harness), recorded);
}

#[test]
fn the_manifest_round_trips_through_the_document() {
    let mut harness = Harness::new(mock_doc());
    bless(&mut harness, json!({ "character": "mock", "scenes": small() })).unwrap();
    let node = harness.doc().get(&manifest_path(&CharId::new("mock").unwrap())).cloned().expect("stored");
    let read: GoldenManifest = serde_json::from_value(node.clone()).unwrap();
    assert_eq!(serde_json::to_value(&read).unwrap(), node);
    // The face shot keeps its own tolerance; the front one the default.
    assert_eq!(read.scenes["face"].tolerance.per_channel, 4);
    assert_eq!(read.scenes["front"].tolerance.per_channel, 2);
}

#[test]
fn blessing_some_variants_keeps_the_others_goldens() {
    let mut harness = Harness::new(mock_doc());
    bless(&mut harness, json!({ "character": "mock", "scenes": small() })).unwrap();
    let before = manifest(&harness);
    let only = bless(&mut harness, json!({ "character": "mock", "variants": ["close-up"] })).unwrap();
    assert_eq!((only.blessed.as_slice(), only.renders), (["close-up".to_owned()].as_slice(), 2));
    assert_eq!(manifest(&harness), before, "nothing changed, so the same goldens");
}

#[test]
fn bad_arguments_are_refused() {
    let mut harness = Harness::new(mock_doc());
    let mut refused = |args: Value| bless(&mut harness, args).expect_err("refused");
    let mut scenes = small();
    scenes["front"]["tolerance"] = json!({ "per_channel": 2, "max_changed": 1.5 });
    for (args, code, field) in [
        (json!({ "character": "mock", "scenes": scenes }), ErrorCode::Invalid, "scenes/front/tolerance/max_changed"),
        (json!({ "character": "mock", "scenes": { "Not A Name": {} } }), ErrorCode::Invalid, "scenes/Not A Name"),
        (json!({ "character": "mock", "scenes": {} }), ErrorCode::Invalid, "scenes"),
        (json!({ "character": "mock", "scenes": { "x": { "size": [8, 8] } } }), ErrorCode::Invalid, "scenes/x/size"),
        (
            json!({ "character": "mock", "scenes": { "x": { "light_rig": "nope" } } }),
            ErrorCode::Invalid,
            "scenes/x/light_rig",
        ),
        (json!({ "character": "mock", "variants": ["nope"] }), ErrorCode::NotFound, "variants/0"),
        (json!({ "character": "mock", "variants": ["game"], "scenes": small() }), ErrorCode::Invalid, "scenes"),
        (json!({ "character": "nobody" }), ErrorCode::NotFound, ""),
    ] {
        let error = refused(args.clone());
        assert_eq!(error.code, code, "{args}: {error:?}");
        if !field.is_empty() {
            assert_eq!(error.field.as_deref(), Some(field), "{args}: {error:?}");
        }
    }
}

#[test]
fn setting_a_scene_keeps_its_goldens_for_a_new_tolerance_and_drops_them_for_a_new_shot() {
    use omuzaki_stage::ops::golden::scene::GoldenSetScene;
    let mut harness = Harness::new(mock_doc());
    bless(&mut harness, json!({ "character": "mock", "scenes": small() })).unwrap();
    let set = |harness: &mut Harness, args: Value| {
        let op: GoldenSetScene = serde_json::from_value(args).unwrap();
        let (actor, clock) = (Actor::Person { name: "test".to_owned() }, FakeClock::new(Seconds(0.0)));
        let plan = op.plan(&OpCtx::new(harness.doc(), &actor, &clock))?;
        omuzaki_doc_api::DocWrite::apply(harness.doc_mut(), &plan.patch).unwrap();
        Ok::<_, OpError>(())
    };
    let looser = json!({ "framing": "full", "size": [48, 64], "tolerance": { "per_channel": 6, "max_changed": 0.02 } });
    set(&mut harness, json!({ "character": "mock", "scene": "front", "shot": looser })).unwrap();
    let kept = manifest(&harness);
    assert_eq!(kept.scenes["front"].tolerance.per_channel, 6);
    assert!(kept.variants.values().all(|v| v.images.contains_key("front")), "a tolerance keeps the goldens");
    set(
        &mut harness,
        json!({ "character": "mock", "scene": "front", "shot": { "framing": "bust", "size": [48, 64] } }),
    )
    .unwrap();
    let dropped = manifest(&harness);
    assert!(dropped.variants.values().all(|v| !v.images.contains_key("front") && v.images.contains_key("face")));
    // The check reports the shot as unblessed and renders only the face shots it has goldens for.
    let check: omuzaki_stage::ops::golden::check::GoldenCheck =
        serde_json::from_value(json!({ "character": "mock", "rerender": true })).unwrap();
    let (plan, value) = harness.run(&check).unwrap();
    assert_eq!(plan.output.unwrap()["renders"], 3);
    let result: omuzaki_stage::ops::golden::check::GoldenCheckResult = serde_json::from_value(value).unwrap();
    assert!(result.passed);
    let front = &result.variants[0].scenes.iter().find(|s| s.scene == "front").unwrap();
    assert_eq!(front.outcome, omuzaki_stage::ops::golden::check::Outcome::Unblessed);
    // Removing the last shot is refused; removing one of two is not.
    set(&mut harness, json!({ "character": "mock", "scene": "front" })).unwrap();
    let error = set(&mut harness, json!({ "character": "mock", "scene": "face" })).expect_err("the last shot");
    assert_eq!(error.code, ErrorCode::Invalid);
}
