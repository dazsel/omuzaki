//! How expressions play (FE-03): the player's fades, auto-stop, exclusive groups, layers and default expression on the
//! built-in library, and the ops that set them (`face.expression.set_playback`, `face.expression.set_default`), with
//! apply and undo exact and the default kept safe through delete and rename.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face::player::ExpressionPlayer;
use omuzaki_face_api::{ExpressionId, ExpressionSet, FaceProfile, PlaybackLayer, Validate};
use omuzaki_ops_api::{Actor, OpCtx, OpError, Plan, find};
use serde_json::{Value, json};

fn id(name: &str) -> ExpressionId {
    ExpressionId::new(name).unwrap()
}

/// Steps `player` for `seconds` at 60 Hz.
fn run_for(player: &mut ExpressionPlayer, seconds: f32) {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a short positive duration
    let frames = (seconds * 60.0).round() as u32;
    for _ in 0..frames {
        player.step(1.0 / 60.0);
    }
}

#[test]
fn the_library_plays_moods_one_at_a_time_and_accents_over_them() {
    let set = ExpressionSet::library().unwrap();
    let profile = FaceProfile { expressions: set.clone(), ..FaceProfile::default() };
    profile.validate().unwrap();
    assert_eq!(set.0[&id("happy")].playback.group.as_deref(), Some("mood"));
    assert_eq!(set.0[&id("wink")].playback.layer, PlaybackLayer::Momentary);

    let mut player = ExpressionPlayer::new(None);
    player.show(&set, &id("happy")).unwrap();
    run_for(&mut player, 0.06);
    let half = player.weight(&id("happy"));
    assert!(half > 0.3 && half < 0.7, "half way through the 0.12 s fade: {half}");
    run_for(&mut player, 0.2);
    assert!((player.weight(&id("happy")) - 1.0).abs() < 1e-6);

    // Another mood cross-fades: happy goes out over 0.2 s while angry comes in.
    player.show(&set, &id("angry")).unwrap();
    assert!(!player.is_shown(&id("happy")) && player.is_shown(&id("angry")));
    run_for(&mut player, 0.5);
    assert_eq!(player.weight(&id("happy")), 0.0, "the group is exclusive");
    assert!((player.weight(&id("angry")) - 1.0).abs() < 1e-6);

    // The wink plays over the mood: it takes the left eye and the left corner from a smile, and gives them back when it
    // stops on its own (0.6 s).
    player.show(&set, &id("happy")).unwrap();
    run_for(&mut player, 0.5);
    let base = ChannelBuf::NEUTRAL;
    let happy = player.blend(&set, &base);
    player.show(&set, &id("wink")).unwrap();
    run_for(&mut player, 0.3);
    let winking = player.blend(&set, &base);
    let wink = &set.0[&id("wink")].deltas.0;
    let shared: Vec<&Channel> = set.0[&id("happy")].deltas.0.keys().filter(|c| wink.contains_key(*c)).collect();
    assert!(!shared.is_empty(), "the wink and happy share channels");
    let untouched = set.0[&id("happy")].deltas.0.keys().find(|c| !wink.contains_key(*c)).copied().unwrap();
    assert_eq!(winking[untouched], happy[untouched], "a channel the wink leaves alone keeps the mood");
    for channel in &shared {
        let expected = (base[**channel] + omuzaki_face_api::narrow(wink[*channel])).clamp(0.0, 1.0);
        assert!((winking[**channel] - expected).abs() < 1e-5, "{} is the wink's while it shows", channel.name());
    }
    run_for(&mut player, 1.0);
    assert_eq!(player.weight(&id("wink")), 0.0, "the wink stopped on its own");
    assert_eq!(player.blend(&set, &base), happy, "and the mood is back as it was");
}

#[test]
fn the_default_expression_fills_in_where_nothing_held_shows() {
    let set = ExpressionSet::library().unwrap();
    let mut player = ExpressionPlayer::new(Some(id("relaxed")));
    let alone = set.blend(&ChannelBuf::NEUTRAL, &[(&id("relaxed"), 1.0)]).unwrap();
    assert_eq!(player.blend(&set, &ChannelBuf::NEUTRAL), alone, "nothing shows: the default does, in full");
    player.show(&set, &id("sad")).unwrap();
    run_for(&mut player, 0.5);
    assert_eq!(player.default_weight(), 0.0, "a held mood replaces it");
    let sad = set.blend(&ChannelBuf::NEUTRAL, &[(&id("sad"), 1.0)]).unwrap();
    assert_eq!(player.blend(&set, &ChannelBuf::NEUTRAL), sad);
    player.hide(&id("sad"));
    run_for(&mut player, 0.1);
    let w = player.default_weight();
    assert!(w > 0.3 && w < 0.7, "it comes back as the mood fades: {w}");
    run_for(&mut player, 0.2);
    assert_eq!(player.blend(&set, &ChannelBuf::NEUTRAL), alone);
}

#[test]
fn the_player_is_steady_on_odd_input() {
    let set = ExpressionSet::library().unwrap();
    let mut player = ExpressionPlayer::new(None);
    assert!(player.show(&set, &id("nope")).is_err());
    player.show(&set, &id("happy")).unwrap();
    player.step(f32::NAN);
    player.step(-1.0);
    assert_eq!(player.weight(&id("happy")), 0.0, "a bad step moves nothing");
    player.step(10.0);
    assert!(player.weight(&id("happy")) > 0.99, "a stalled frame steps at most 0.25 s, which ends a 0.12 s fade");
    for name in ["wink", "hearts", "stars", "sparkle", "sweat", "gloom", "dizzy", "bleh", "aa", "ih", "ou", "ee", "oh"]
    {
        player.show(&set, &id(name)).unwrap();
    }
    for name in ["blink", "laugh", "cry", "smug", "shy"] {
        player.show(&set, &id(name)).unwrap();
    }
    player.step(0.1);
    let out = player.blend(&set, &ChannelBuf::NEUTRAL);
    assert!(out.as_slice().iter().all(|v| v.is_finite()), "eighteen shown at once stay finite (the faintest drop)");
}

fn run(doc: &MemDoc, name: &str, args: Value) -> Result<Plan, OpError> {
    let op = (find(name).unwrap_or_else(|| panic!("{name} is registered")).decode)(args)?;
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    op.plan(&OpCtx::new(doc, &actor, &clock))
}

fn doc() -> MemDoc {
    let face = FaceProfile { expressions: ExpressionSet::library().unwrap(), ..FaceProfile::default() };
    let face = serde_json::to_value(face).unwrap();
    MemDoc::from_value(json!({ "characters": { "mira": { "name": "Mira", "face": face } } }))
}

fn face_of(doc: &MemDoc) -> FaceProfile {
    serde_json::from_value(doc.get(&DocPath::parse("characters/mira/face").unwrap()).cloned().unwrap()).unwrap()
}

/// Applies `name` with `args`, checks the face still validates, undoes it and checks the document is as it was.
fn apply_and_undo(name: &str, args: &Value, check: impl Fn(&FaceProfile)) {
    let mut document = doc();
    let before = document.root().clone();
    let plan = run(&document, name, args.clone()).unwrap_or_else(|e| panic!("{args}: {e}"));
    assert_eq!(plan, run(&document, name, args.clone()).unwrap(), "a dry run repeats");
    document.apply(&plan.patch).unwrap();
    let face = face_of(&document);
    face.validate().unwrap();
    check(&face);
    document.apply(&plan.patch.inverse()).unwrap();
    assert_eq!(document.root(), &before, "{args}: undo restores the document exactly");
}

#[test]
fn the_playback_and_default_ops_apply_and_undo_exactly() {
    apply_and_undo(
        "face.expression.set_playback",
        &json!({ "character": "mira", "id": "happy", "fade_in_s": 0.4, "auto_stop_s": 5 }),
        |face| {
            let p = &face.expressions.0[&id("happy")].playback;
            assert_eq!((p.fade_in_s, p.auto_stop_s, p.group.as_deref()), (0.4, Some(5.0), Some("mood")));
        },
    );
    apply_and_undo(
        "face.expression.set_playback",
        &json!({ "character": "mira", "id": "wink", "auto_stop_s": 0, "group": "", "layer": "held" }),
        |face| {
            let p = &face.expressions.0[&id("wink")].playback;
            assert_eq!((p.auto_stop_s, p.group.as_deref(), p.layer), (None, None, PlaybackLayer::Held));
        },
    );
    apply_and_undo("face.expression.set_default", &json!({ "character": "mira", "id": "relaxed" }), |face| {
        assert_eq!(face.default_expression, Some(id("relaxed")));
    });
    // Replacing an expression with face.expression.set keeps its playback unless one is given.
    apply_and_undo(
        "face.expression.set",
        &json!({ "character": "mira", "id": "happy", "kind": "emotion", "deltas": { "mouthSmileLeft": 1.0 } }),
        |face| assert_eq!(face.expressions.0[&id("happy")].playback.group.as_deref(), Some("mood")),
    );
}

#[test]
fn the_ops_refuse_what_would_break_the_face() {
    let document = doc();
    let refused = |name: &str, args: Value| run(&document, name, args.clone()).expect_err(&args.to_string());
    let e = refused("face.expression.set_playback", json!({ "character": "mira", "id": "happy", "fade_in_s": 11 }));
    assert_eq!(e.field.as_deref(), Some("fade_in_s"), "{e}");
    let e = refused("face.expression.set_playback", json!({ "character": "mira", "id": "happy", "group": "Mood!" }));
    assert_eq!(e.field.as_deref(), Some("group"), "{e}");
    let e = refused("face.expression.set_playback", json!({ "character": "mira", "id": "nope" }));
    assert!(e.message.contains("no expression 'nope'"), "{e}");
    let e = refused("face.expression.set_default", json!({ "character": "mira", "id": "aa" }));
    assert!(e.message.contains("viseme"), "{e}");

    // The default can't be deleted out from under the face, and follows a rename.
    let mut document = doc();
    let set = run(&document, "face.expression.set_default", json!({ "character": "mira", "id": "smug" })).unwrap();
    document.apply(&set.patch).unwrap();
    let e = run(&document, "face.expression.delete", json!({ "character": "mira", "id": "smug" })).unwrap_err();
    assert!(e.message.contains("default expression"), "{e}");
    let renamed =
        run(&document, "face.expression.rename", json!({ "character": "mira", "id": "smug", "to": "proud" })).unwrap();
    document.apply(&renamed.patch).unwrap();
    let face = face_of(&document);
    face.validate().unwrap();
    assert_eq!(face.default_expression, Some(id("proud")));
}
