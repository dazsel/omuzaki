//! `ExpressionEvaluator` against VRM 1.0's rules and `three-vrm`'s values: overrides, `isBinary`, inputs, the gaze
//! routes, colour and UV binds.
//!
//! The expected values in [`THREE_VRM`] are what `three-vrm`'s `VRMExpressionManager.update` (MIT) computes for the same
//! expressions and weights: `outputWeight` steps a binary expression, each class's multiplier is
//! `max(0, 1 − Σ overrideAmount)` with a blocking expression counting 1 while its output is above 0 and a blending one
//! its output, and `applyWeight` multiplies a class target by it and zeroes a binary target left below 1. They are
//! worked by hand from that code, and the same steps are the VRM 1.0 spec's own examples.

mod common;

use common::{Run, avatar, ix};
use omuzaki_anim_api::Channel;
use omuzaki_avatar::buffers::MOVE_THRESHOLD;
use omuzaki_avatar::evaluator::ExpressionEvaluator;
use omuzaki_avatar_api::avatar::ImportedAvatar;
use omuzaki_avatar_api::expression::{ColourBind, ExprIx, Overrides, UvBind};
use omuzaki_avatar_api::gaze::{AvatarGaze, AvatarLookAt};
use omuzaki_core::glam::{Vec2, Vec4};
use omuzaki_io_api::{ColorBindKind, LookAtKind, Override, RangeMap};

/// `happy` on `custom00`, `blink` on `eyeBlinkLeft`, `aa` on `jawOpen`, `lookUp` on `custom01`: the spec's examples.
fn face() -> ImportedAvatar {
    let mut a = avatar(false, &["happy", "blink", "aa", "lookUp"]);
    for (channel, n) in [
        (Channel::CUSTOM_00, "happy"),
        (Channel::EYE_BLINK_LEFT, "blink"),
        (Channel::JAW_OPEN, "aa"),
        (Channel::CUSTOM_01, "lookUp"),
    ] {
        let e = ix(&a, n);
        a.drivers.set(channel, Some(e));
    }
    a
}

/// One row of the comparison: how `happy` and `blink` are set up, the inputs, and the weights `three-vrm` applies.
struct Case {
    what: &'static str,
    happy_binary: bool,
    blink_binary: bool,
    happy: Overrides,
    inputs: [f32; 4],
    /// `happy`, `blink`, `aa`, `lookUp` as `three-vrm` applies them.
    expected: [f32; 4],
}

const NONE: Overrides = Overrides::new(Override::None, Override::None, Override::None);
const BLOCK_ALL: Overrides = Overrides::new(Override::Block, Override::Block, Override::Block);
const BLEND_ALL: Overrides = Overrides::new(Override::Blend, Override::Blend, Override::Blend);
const BLEND_BLINK: Overrides = Overrides::new(Override::Blend, Override::None, Override::None);
const BLOCK_MOUTH: Overrides = Overrides::new(Override::None, Override::None, Override::Block);

/// The comparison table (#865 evidence). Inputs and outputs are `happy`, `blink`, `aa`, `lookUp`.
const THREE_VRM: &[Case] = &[
    Case {
        what: "none: nothing is suppressed",
        happy_binary: false,
        blink_binary: false,
        happy: NONE,
        inputs: [1.0, 1.0, 1.0, 1.0],
        expected: [1.0, 1.0, 1.0, 1.0],
    },
    Case {
        what: "block: any weight above 0 zeroes the classes",
        happy_binary: false,
        blink_binary: false,
        happy: BLOCK_ALL,
        inputs: [0.01, 1.0, 0.8, 0.6],
        expected: [0.01, 0.0, 0.0, 0.0],
    },
    Case {
        what: "block at weight 0 does nothing",
        happy_binary: false,
        blink_binary: false,
        happy: BLOCK_ALL,
        inputs: [0.0, 1.0, 0.8, 0.6],
        expected: [0.0, 1.0, 0.8, 0.6],
    },
    Case {
        what: "blend: targets scale by 1 - weight",
        happy_binary: false,
        blink_binary: false,
        happy: BLEND_ALL,
        inputs: [0.3, 1.0, 0.8, 0.5],
        expected: [0.3, 0.7, 0.56, 0.35],
    },
    Case {
        what: "blend only the class it names",
        happy_binary: false,
        blink_binary: false,
        happy: BLEND_BLINK,
        inputs: [0.3, 1.0, 0.8, 0.5],
        expected: [0.3, 0.7, 0.8, 0.5],
    },
    Case {
        what: "block only the class it names",
        happy_binary: false,
        blink_binary: false,
        happy: BLOCK_MOUTH,
        inputs: [0.5, 1.0, 0.8, 0.5],
        expected: [0.5, 1.0, 0.0, 0.5],
    },
    Case {
        what: "spec: binary blink 1 under blend happy 0.2 is 0, not 0.8",
        happy_binary: false,
        blink_binary: true,
        happy: BLEND_BLINK,
        inputs: [0.2, 1.0, 0.0, 0.0],
        expected: [0.2, 0.0, 0.0, 0.0],
    },
    Case {
        what: "binary blink with happy at 0 stays 1",
        happy_binary: false,
        blink_binary: true,
        happy: BLEND_BLINK,
        inputs: [0.0, 1.0, 0.0, 0.0],
        expected: [0.0, 1.0, 0.0, 0.0],
    },
    Case {
        what: "binary blink at 0.5 steps to 0",
        happy_binary: false,
        blink_binary: true,
        happy: NONE,
        inputs: [0.0, 0.5, 0.0, 0.0],
        expected: [0.0, 0.0, 0.0, 0.0],
    },
    Case {
        what: "binary blink 0.51 steps to 1",
        happy_binary: false,
        blink_binary: true,
        happy: NONE,
        inputs: [0.0, 0.51, 0.0, 0.0],
        expected: [0.0, 1.0, 0.0, 0.0],
    },
    Case {
        what: "binary happy 0.2 steps to 0 and suppresses nothing",
        happy_binary: true,
        blink_binary: false,
        happy: BLEND_ALL,
        inputs: [0.2, 1.0, 1.0, 1.0],
        expected: [0.0, 1.0, 1.0, 1.0],
    },
    Case {
        what: "binary happy 0.6 steps to 1 and suppresses fully",
        happy_binary: true,
        blink_binary: false,
        happy: BLEND_ALL,
        inputs: [0.6, 1.0, 1.0, 1.0],
        expected: [1.0, 0.0, 0.0, 0.0],
    },
    Case {
        what: "inputs clamp to 0..1",
        happy_binary: false,
        blink_binary: false,
        happy: NONE,
        inputs: [1.7, -0.4, 2.0, 0.25],
        expected: [1.0, 0.0, 1.0, 0.25],
    },
];

fn configured(case: &Case) -> ImportedAvatar {
    let mut a = face();
    let (happy, blink) = (ix(&a, "happy").index(), ix(&a, "blink").index());
    a.expressions[happy].binary = case.happy_binary;
    a.expressions[happy].overrides = case.happy;
    a.expressions[blink].binary = case.blink_binary;
    a
}

#[test]
fn the_override_and_binary_rules_match_three_vrm() {
    println!("| case | happy, blink, aa, lookUp in | three-vrm applies | ours |");
    println!("|---|---|---|---|");
    for case in THREE_VRM {
        let a = configured(case);
        let mut run = Run::new(&a);
        let [h, b, aa, look] = case.inputs;
        run.eval(&[
            (Channel::CUSTOM_00, h),
            (Channel::EYE_BLINK_LEFT, b),
            (Channel::JAW_OPEN, aa),
            (Channel::CUSTOM_01, look),
        ]);
        let ours = ["happy", "blink", "aa", "lookUp"].map(|n| run.weights.expression(ix(&a, n)));
        println!("| {} | {:?} | {:?} | {:?} |", case.what, case.inputs, case.expected, ours);
        for (got, want) in ours.iter().zip(case.expected) {
            assert!((got - want).abs() < 1e-6, "{}: {ours:?}, want {:?}", case.what, case.expected);
        }
        let targets = ["happy", "blink", "aa", "lookUp"].map(|n| run.target(ix(&a, n)));
        assert_eq!(targets, ours, "{}: each expression's own target carries its weight", case.what);
    }
}

#[test]
fn blends_add_up_and_saturate_and_a_block_counts_one() {
    let mut a = avatar(false, &["happy", "sad", "aa"]);
    for (n, o) in [("happy", Override::Blend), ("sad", Override::Blend)] {
        let e = ix(&a, n).index();
        a.expressions[e].overrides = Overrides::new(Override::None, Override::None, o);
    }
    for (c, n) in [(Channel::CUSTOM_00, "happy"), (Channel::CUSTOM_01, "sad"), (Channel::JAW_OPEN, "aa")] {
        let e = ix(&a, n);
        a.drivers.set(c, Some(e));
    }
    let aa = ix(&a, "aa");
    let weight = |a: &ImportedAvatar, h: f32, s: f32| {
        let mut run = Run::new(a);
        run.eval(&[(Channel::CUSTOM_00, h), (Channel::CUSTOM_01, s), (Channel::JAW_OPEN, 1.0)]).weights.expression(aa)
    };
    assert!((weight(&a, 0.2, 0.3) - 0.5).abs() < 1e-6, "1 - (0.2 + 0.3)");
    assert_eq!(weight(&a, 0.6, 0.7), 0.0, "the sum saturates");

    let sad = ix(&a, "sad").index();
    a.expressions[sad].overrides = Overrides::new(Override::None, Override::None, Override::Block);
    assert_eq!(weight(&a, 0.2, 0.1), 0.0, "a block at 0.1 counts 1, whatever blends beside it");
    assert!((weight(&a, 0.2, 0.0) - 0.8).abs() < 1e-6, "a block at 0 counts nothing");
}

#[test]
fn several_channels_take_the_largest_and_an_inert_channel_changes_nothing() {
    let mut a = avatar(false, &["happy", "jawOpen"]);
    let happy = ix(&a, "happy");
    a.drivers.set(Channel::CUSTOM_00, Some(happy));
    a.drivers.set(Channel::CUSTOM_05, Some(happy));
    let mut run = Run::new(&a);
    run.eval(&[(Channel::CUSTOM_00, 0.3), (Channel::CUSTOM_05, 0.8)]);
    assert_eq!(run.weights.expression(happy), 0.8, "the largest wins, never the sum");
    run.eval(&[(Channel::CUSTOM_00, 0.9), (Channel::CUSTOM_05, 0.2)]);
    assert_eq!(run.weights.expression(happy), 0.9);

    let quiet = run.eval(&[]).weights.clone();
    let inert = run
        .eval(&[(Channel::CHEEK_PUFF, 1.0), (Channel::TONGUE_OUT, 1.0), (Channel::EYE_LOOK_UP_LEFT, 1.0)])
        .weights
        .clone();
    assert_eq!(inert, quiet, "channels that drive nothing move nothing");
}

/// Gaze straight up, down or sideways (toward the character's left), as full-weight `eyeLook*` channels.
fn up() -> [(Channel, f32); 2] {
    [(Channel::EYE_LOOK_UP_LEFT, 1.0), (Channel::EYE_LOOK_UP_RIGHT, 1.0)]
}
fn down() -> [(Channel, f32); 2] {
    [(Channel::EYE_LOOK_DOWN_LEFT, 1.0), (Channel::EYE_LOOK_DOWN_RIGHT, 1.0)]
}
fn left() -> [(Channel, f32); 2] {
    [(Channel::EYE_LOOK_OUT_LEFT, 1.0), (Channel::EYE_LOOK_IN_RIGHT, 1.0)]
}
fn all_gaze() -> Vec<(Channel, f32)> {
    [up(), down(), left()]
        .concat()
        .into_iter()
        .chain([(Channel::EYE_LOOK_IN_LEFT, 1.0), (Channel::EYE_LOOK_OUT_RIGHT, 1.0)])
        .collect()
}

/// A look-at whose range maps reach full output at a full look (40°), so a full channel gives weight 1.
fn full_at_one(kind: LookAtKind) -> AvatarLookAt {
    let mut l = AvatarLookAt::default_for(kind);
    let map = RangeMap { input_max_value: 40.0, output_scale: 1.0 };
    (l.vertical_up, l.vertical_down, l.horizontal_inner, l.horizontal_outer) = (map, map, map, map);
    l
}

fn all_quiet(run: &Run) -> bool {
    run.weights.as_slice().iter().all(|w| *w == 0.0)
}

#[test]
fn the_bone_route_writes_no_gaze_weight() {
    let mut a = avatar(true, &["lookUp", "lookDown", "eyeLookUpLeft"]);
    a.gaze = AvatarGaze::Bone;
    let mut run = Run::new(&a);
    run.eval(&all_gaze());
    assert!(all_quiet(&run), "the IK stage turns the eyes; no expression moves: {:?}", run.weights.as_slice());
}

#[test]
fn the_native_route_drives_the_models_own_gaze_morphs() {
    let mut a = avatar(false, &["eyeLookUpLeft", "eyeLookUpRight", "eyeLookOutLeft"]);
    a.gaze = AvatarGaze::NativeArkit;
    for n in ["eyeLookUpLeft", "eyeLookUpRight", "eyeLookOutLeft"] {
        let e = ix(&a, n);
        a.drivers.set(Channel::from_name(n).unwrap(), Some(e));
    }
    let mut run = Run::new(&a);
    run.eval(&up());
    assert_eq!(run.target(ix(&a, "eyeLookUpLeft")), 1.0);
    assert_eq!(run.target(ix(&a, "eyeLookUpRight")), 1.0);
    assert_eq!(run.target(ix(&a, "eyeLookOutLeft")), 0.0);
}

#[test]
fn the_presets_route_counts_a_symmetric_gaze_once() {
    let mut a = avatar(false, &["lookUp", "lookDown", "lookLeft", "lookRight"]);
    a.gaze = AvatarGaze::Presets;
    a.look_at = Some(full_at_one(LookAtKind::Expression));
    let mut run = Run::new(&a);
    run.eval(&up());
    assert_eq!(run.weights.expression(ix(&a, "lookUp")), 1.0, "both eyeLookUp* at 1 give lookUp 1, not 2");
    assert_eq!(run.weights.expression(ix(&a, "lookDown")), 0.0);
    run.eval(&left());
    assert_eq!(run.weights.expression(ix(&a, "lookLeft")), 1.0);
    assert_eq!(run.weights.expression(ix(&a, "lookRight")), 0.0);
    run.eval(&[(Channel::EYE_LOOK_UP_LEFT, 0.5), (Channel::EYE_LOOK_UP_RIGHT, 0.25)]);
    assert!((run.weights.expression(ix(&a, "lookUp")) - 0.5).abs() < 1e-5, "the larger of the pair");

    a.look_at = None;
    let mut run = Run::new(&a);
    run.eval(&up());
    let default = run.weights.expression(ix(&a, "lookUp"));
    assert!((default - 40.0 / 90.0).abs() < 1e-5, "the default map: a full look is 40° of 90°, got {default}");
}

#[test]
fn the_inert_route_moves_nothing() {
    let mut a = avatar(true, &["lookUp", "lookDown", "lookLeft", "lookRight"]);
    a.gaze = AvatarGaze::Inert;
    let mut run = Run::new(&a);
    run.eval(&all_gaze());
    assert!(all_quiet(&run));
}

#[test]
fn a_look_up_only_avatar_moves_for_up_and_stays_still_otherwise() {
    let mut a = avatar(false, &["lookUp"]);
    a.gaze = AvatarGaze::Presets;
    a.look_at = Some(full_at_one(LookAtKind::Expression));
    let mut run = Run::new(&a);
    assert!(run.eval(&up()).target(ExprIx(0)) > 0.0, "up moves it");
    assert!(all_quiet(run.eval(&down())), "down does not");
    assert!(all_quiet(run.eval(&left())), "sideways does not");
}

#[test]
fn a_native_avatar_moves_up_even_with_a_flat_vertical_map() {
    let mut a = avatar(false, &["eyeLookUpLeft"]);
    a.gaze = AvatarGaze::NativeArkit;
    a.drivers.set(Channel::EYE_LOOK_UP_LEFT, Some(ExprIx(0)));
    let mut look = AvatarLookAt::default_for(LookAtKind::Bone);
    look.vertical_up.output_scale = 0.0;
    a.look_at = Some(look);
    let mut run = Run::new(&a);
    assert_eq!(run.eval(&up()).target(ExprIx(0)), 1.0);
}

/// `happy` on `custom00` moving the face material's base colour and UV.
fn binds() -> ImportedAvatar {
    let mut a = avatar(false, &["happy"]);
    let happy = ix(&a, "happy");
    a.drivers.set(Channel::CUSTOM_00, Some(happy));
    let e = &mut a.expressions[happy.index()];
    e.colour.push(ColourBind::new(
        common::material(),
        ColorBindKind::Color,
        Vec4::new(1.0, 1.0, 1.0, 1.0),
        Vec4::new(0.0, 0.5, 1.0, 1.0),
    ));
    e.colour.push(ColourBind::new(
        common::material(),
        ColorBindKind::ShadeColor,
        Vec4::ZERO,
        Vec4::new(0.2, 0.0, 0.0, 1.0),
    ));
    e.uv.push(UvBind::new(common::material(), Vec2::new(0.5, 1.0), Vec2::new(0.5, 0.0)));
    a
}

#[test]
fn colour_and_uv_binds_move_half_way_at_half_weight() {
    let a = binds();
    let mut run = Run::new(&a);
    run.eval(&[(Channel::CUSTOM_00, 0.5)]);
    let d = run.binds.get(&common::material()).unwrap();
    assert_eq!(
        d.colour(ColorBindKind::Color),
        Vec4::new(-0.5, -0.25, 0.0, 0.0),
        "base + delta is half way to the target"
    );
    assert_eq!(d.colour(ColorBindKind::ShadeColor), Vec4::new(0.1, 0.0, 0.0, 0.5));
    assert_eq!(d.colour(ColorBindKind::RimColor), Vec4::ZERO);
    assert_eq!((d.uv_scale(), d.uv_offset()), (Vec2::new(-0.25, 0.0), Vec2::new(0.25, 0.0)));
    assert!(d.moved());
    assert_eq!(run.binds.len(), 1);
}

#[test]
fn a_material_is_marked_moved_above_the_threshold_and_not_below() {
    let a = binds();
    let mut run = Run::new(&a);
    // The largest delta is base colour red: weight · 1.0.
    let step_below = MOVE_THRESHOLD * 0.9;
    let step_above = MOVE_THRESHOLD * 1.1;
    assert!(!run.eval(&[(Channel::CUSTOM_00, step_below)]).binds.get(&common::material()).unwrap().moved(), "below");
    assert!(run.eval(&[(Channel::CUSTOM_00, step_above)]).binds.get(&common::material()).unwrap().moved(), "above");
    assert!(
        !run.eval(&[(Channel::CUSTOM_00, step_above)]).binds.get(&common::material()).unwrap().moved(),
        "unchanged"
    );
    let creep = step_above + step_below;
    assert!(!run.eval(&[(Channel::CUSTOM_00, creep)]).binds.get(&common::material()).unwrap().moved(), "a small step");
    let more = step_above + 2.0 * step_below;
    assert!(
        run.eval(&[(Channel::CUSTOM_00, more)]).binds.get(&common::material()).unwrap().moved(),
        "small steps add up"
    );
}

#[test]
fn evaluation_is_deterministic_and_nan_safe() {
    let a = binds();
    let mut first = Run::new(&a);
    let mut second = Run::new(&a);
    first.eval(&[(Channel::CUSTOM_00, 0.37)]);
    second.eval(&[(Channel::CUSTOM_00, 0.37)]);
    assert_eq!((&first.weights, &first.binds), (&second.weights, &second.binds));
    first.eval(&[(Channel::CUSTOM_00, f32::NAN)]);
    assert!(first.weights.as_slice().iter().all(|w| *w == 0.0), "a NaN channel reads as 0");
}

#[test]
fn buffers_are_laid_out_mesh_after_mesh() {
    let mut a = avatar(false, &["happy", "sad"]);
    a.meshes.push(common::mesh(0));
    a.meshes.push(common::mesh(3));
    let run = Run::new(&a);
    assert_eq!(run.weights.meshes(), 3);
    assert_eq!(run.weights.range(0), Some(0..2));
    assert_eq!(run.weights.range(1), Some(2..2));
    assert_eq!(run.weights.range(2), Some(2..5));
    assert_eq!(run.weights.range(3), None);
    assert_eq!(run.weights.as_slice().len(), 5);
    assert_eq!(run.weights.expressions().len(), 2);
    assert!(run.binds.is_empty());
}

#[test]
fn an_evaluator_from_the_body_view_matches_one_from_the_avatar() {
    let mut a = binds();
    a.gaze = AvatarGaze::Presets;
    a.expressions.push(omuzaki_avatar_api::expression::AvatarExpression::new(common::name("lookUp")));
    let mut from_avatar = Run::new(&a);
    let body = omuzaki_avatar_api::body::Body::Imported(Box::new(a.clone()));
    let evaluator = ExpressionEvaluator::from_view(&body.view()).expect("an imported body has expressions");
    let mut from_view = Run { weights: evaluator.morph_weights(), binds: evaluator.bind_deltas(), evaluator };
    let inputs = [(Channel::CUSTOM_00, 0.6), (Channel::EYE_LOOK_UP_LEFT, 0.5)];
    from_avatar.eval(&inputs);
    from_view.eval(&inputs);
    assert_eq!((&from_view.weights, &from_view.binds), (&from_avatar.weights, &from_avatar.binds));
}

#[test]
fn a_bad_gaze_channel_cannot_cancel_or_swamp_a_good_one() {
    let mut a = avatar(false, &["lookUp", "lookDown", "lookLeft", "lookRight"]);
    a.gaze = AvatarGaze::Presets;
    a.look_at = Some(full_at_one(LookAtKind::Expression));
    let mut run = Run::new(&a);
    let (look_up, look_down) = (ix(&a, "lookUp"), ix(&a, "lookDown"));

    let nan_up = [(Channel::EYE_LOOK_UP_LEFT, f32::NAN), (Channel::EYE_LOOK_UP_RIGHT, f32::NAN)];
    run.eval(&[nan_up.as_slice(), &down()].concat());
    assert_eq!(run.weights.expression(look_down), 1.0, "a NaN upward pair reads as 0 and leaves the downward gaze");
    assert_eq!(run.weights.expression(look_up), 0.0);

    run.eval(&[(Channel::EYE_LOOK_UP_LEFT, 3.0), (Channel::EYE_LOOK_DOWN_LEFT, 0.5)]);
    assert!((run.weights.expression(look_up) - 0.5).abs() < 1e-5, "3.0 clamps to 1, so up 1 against down 0.5 nets 0.5");
    run.eval(&[(Channel::EYE_LOOK_DOWN_LEFT, -2.0), (Channel::EYE_LOOK_UP_LEFT, 0.25)]);
    assert!((run.weights.expression(look_up) - 0.25).abs() < 1e-5, "a negative channel reads as 0");
    assert_eq!(run.weights.expression(look_down), 0.0);
}
