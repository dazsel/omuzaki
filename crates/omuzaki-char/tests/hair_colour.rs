//! Hair colour (#183): streaks tint exactly their share of the clumps, the same ones for a seed and whatever unrelated
//! groups do; `hair.set_shading`, `hair.streak.*` and `hair.recolour` are undoable edits of the part's shading that
//! refuse a bad field or value by name; and the colours reach the built hair.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // colours in 0..=1 keyed to a 1e-3 grid

mod common;

use common::{assert_error, at, refused, round_trip};
use omuzaki_char_api::{BuiltCharacter, GroupId, HairShading, Streak};
use omuzaki_core::colour::Colour;
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::{ColourRef, Palette};
use omuzaki_ops_api::ErrorCode;
use serde_json::json;

/// The kit, whose hair part is `hair`.
fn kit() -> MemDoc {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    doc
}

const SHADING: &str = "characters/kit/parts/hair/params/shading";

fn group(name: &str) -> GroupId {
    GroupId::new(name).unwrap()
}

/// How many of `tints` a streak touched.
fn tinted(tints: &[Option<Colour>]) -> usize {
    tints.iter().filter(|t| t.is_some()).count()
}

#[test]
fn a_fifth_of_44_clumps_is_exactly_9_and_the_same_9_for_a_seed() {
    let a = group("a");
    let groups: Vec<&GroupId> = vec![&a; 44];
    let shading =
        HairShading { streaks: vec![Streak { fraction: 0.2, ..Streak::default() }], ..HairShading::default() };
    let tints = shading.streak_tints(&Palette::default(), &groups).unwrap();
    assert_eq!(tinted(&tints), 9, "round(0.2 x 44) clumps");
    assert_eq!(shading.streak_tints(&Palette::default(), &groups).unwrap(), tints, "the same clumps every time");
    let reseeded =
        HairShading { streaks: vec![Streak { fraction: 0.2, seed: 7, ..Streak::default() }], ..HairShading::default() };
    let other = reseeded.streak_tints(&Palette::default(), &groups).unwrap();
    assert_eq!(tinted(&other), 9);
    assert_ne!(other, tints, "another seed picks other clumps");
    for (fraction, expected) in [(0.0, 0), (1.0, 44), (0.5, 22), (0.01, 0), (0.02, 1)] {
        let s = HairShading { streaks: vec![Streak { fraction, ..Streak::default() }], ..HairShading::default() };
        assert_eq!(tinted(&s.streak_tints(&Palette::default(), &groups).unwrap()), expected, "fraction {fraction}");
    }
}

#[test]
fn a_streak_on_one_group_ignores_what_other_groups_do() {
    let (a, b) = (group("a"), group("b"));
    let streak = Streak { fraction: 0.3, groups: vec![a.clone()], ..Streak::default() };
    let shading = HairShading { streaks: vec![streak], ..HairShading::default() };
    let alone: Vec<&GroupId> = vec![&a; 20];
    let tints = shading.streak_tints(&Palette::default(), &alone).unwrap();
    // The same 20 clumps of `a` with clumps of `b` between them: the same `a` clumps take the streak, and no `b` one.
    let mut mixed = Vec::new();
    for k in 0..20 {
        mixed.push(&a);
        if k % 3 == 0 {
            mixed.extend([&b, &b]);
        }
    }
    let mixed_tints = shading.streak_tints(&Palette::default(), &mixed).unwrap();
    let of_a: Vec<Option<Colour>> =
        mixed.iter().zip(&mixed_tints).filter(|(g, _)| ***g == a).map(|(_, t)| *t).collect();
    assert_eq!(of_a, tints);
    assert!(mixed.iter().zip(&mixed_tints).all(|(g, t)| **g == a || t.is_none()), "no clump of `b` is tinted");
}

// ------------------------------------------------------------------------------------------------------- the ops

#[test]
fn set_shading_edits_one_field_undoably_and_refuses_by_name() {
    let mut doc = kit();
    for (field, value) in [
        ("ring_strength", json!(0.45)),
        ("root", json!("#3a2a4a")),
        ("keep_identity", json!(0.8)),
        ("brows_through", json!(false)),
        ("inner_darkening", json!(0.5)),
        ("outline_width", json!(0.002)),
    ] {
        round_trip(
            &mut doc,
            "hair.set_shading",
            json!({"character": "kit", "part": "hair", "field": field, "value": value}),
        );
        assert_eq!(at(&doc, &format!("{SHADING}/{field}")), Some(value), "{field}");
    }
    let e = refused(
        &doc,
        "hair.set_shading",
        json!({"character": "kit", "part": "hair", "field": "ring_strenght", "value": 0.4}),
    );
    assert_error(&e, ErrorCode::Invalid, "field", &["ring_strenght"], &["ring_strength"]);
    let e = refused(
        &doc,
        "hair.set_shading",
        json!({"character": "kit", "part": "hair", "field": "ring_strength", "value": 2.0}),
    );
    assert_error(&e, ErrorCode::Invalid, "value", &["ring_strength"], &[]);
    let e =
        refused(&doc, "hair.set_shading", json!({"character": "kit", "part": "hair", "field": "streaks", "value": []}));
    assert_error(&e, ErrorCode::Invalid, "field", &["streaks"], &[]);
    let e = refused(&doc, "hair.set_shading", json!({"character": "kit", "part": "hair", "field": "root", "value": 3}));
    assert_error(&e, ErrorCode::Invalid, "value", &["root"], &[]);
    let e = refused(
        &doc,
        "hair.set_shading",
        json!({"character": "kit", "part": "body", "field": "root", "value": "#000000"}),
    );
    assert_error(&e, ErrorCode::Invalid, "part", &["not hair"], &["hair"]);
}

#[test]
fn streaks_are_added_set_and_removed_undoably() {
    let mut doc = kit();
    let added = round_trip(
        &mut doc,
        "hair.streak.add",
        json!({"character": "kit", "part": "hair", "streak": {"fraction": 0.25, "colour": "#9b6ed6"}}),
    );
    assert_eq!(added.output.as_ref().unwrap()["index"], json!(0));
    let second = round_trip(&mut doc, "hair.streak.add", json!({"character": "kit", "part": "hair"}));
    assert_eq!(second.output.as_ref().unwrap()["index"], json!(1), "a streak of defaults");
    assert_eq!(at(&doc, &format!("{SHADING}/streaks/0/fraction")), Some(json!(0.25)));
    round_trip(
        &mut doc,
        "hair.streak.set",
        json!({"character": "kit", "part": "hair", "index": 1, "field": "blend", "value": "multiply"}),
    );
    assert_eq!(at(&doc, &format!("{SHADING}/streaks/1/blend")), Some(json!("multiply")));
    round_trip(&mut doc, "hair.streak.remove", json!({"character": "kit", "part": "hair", "index": 0}));
    assert_eq!(at(&doc, &format!("{SHADING}/streaks/0/blend")), Some(json!("multiply")), "the second moved down");

    let e = refused(
        &doc,
        "hair.streak.set",
        json!({"character": "kit", "part": "hair", "index": 4, "field": "seed", "value": 2}),
    );
    assert_error(&e, ErrorCode::NotFound, "index", &["1 streaks"], &["hair.streak.add"]);
    let e = refused(
        &doc,
        "hair.streak.set",
        json!({"character": "kit", "part": "hair", "index": 0, "field": "fraction", "value": 1.5}),
    );
    assert_error(&e, ErrorCode::Invalid, "value", &["fraction"], &[]);
    let e = refused(&doc, "hair.streak.add", json!({"character": "kit", "part": "hair", "streak": {"hue": 3}}));
    assert_error(&e, ErrorCode::Invalid, "streak", &["hue"], &["fraction"]);
}

/// The built hair's tint colours: every distinct vertex colour of its meshes.
fn hair_colours(built: &BuiltCharacter) -> Vec<[u32; 3]> {
    let mut out: Vec<[u32; 3]> = built
        .meshes
        .iter()
        .filter(|m| m.part.as_str() == "hair")
        .flat_map(|m| m.mesh.geometry.colours.iter())
        .map(|c| [c[0], c[1], c[2]].map(|v| (v * 1000.0).round() as u32))
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[test]
fn a_streak_reaches_the_built_hair() {
    let mut doc = kit();
    let plain = hair_colours(&common::build(&doc, "kit"));
    common::apply(
        &mut doc,
        "hair.streak.add",
        json!({"character": "kit", "part": "hair", "streak": {"fraction": 0.5, "colour": "#20c0a0", "strength": 1.0}}),
    );
    let streaked = hair_colours(&common::build(&doc, "kit"));
    assert!(
        streaked.len() > plain.len(),
        "the streak adds tinted clumps: {} colours before, {} after",
        plain.len(),
        streaked.len()
    );
}

#[test]
fn recolour_goes_through_the_palette_by_default_and_into_the_part_on_request() {
    let mut doc = kit();
    round_trip(
        &mut doc,
        "hair.recolour",
        json!({"character": "kit", "part": "hair", "root": "#3a2a4a", "mid": "#6a5a8a", "tip": "#a898c8"}),
    );
    for (field, slot, colour) in
        [("root", "hair_root", "#3a2a4a"), ("mid", "hair_mid", "#6a5a8a"), ("tip", "hair_tip", "#a898c8")]
    {
        assert_eq!(at(&doc, &format!("{SHADING}/{field}")), Some(json!(format!("@{slot}"))), "{field}");
        assert_eq!(at(&doc, &format!("characters/kit/palette/{slot}")), Some(json!(colour)), "{slot}");
    }
    // A colourway swap (here, the slot itself) now recolours the hair.
    common::apply(&mut doc, "palette.set", json!({"character": "kit", "slot": "hair_mid", "colour": "#204060"}));
    let def = common::definition(&doc, "kit");
    let mid = def.palette.resolve(&ColourRef::Slot(omuzaki_face_api::PaletteSlot::new("hair_mid").unwrap())).unwrap();
    assert_eq!(mid, "#204060".parse::<Colour>().unwrap());

    round_trip(
        &mut doc,
        "hair.recolour",
        json!({"character": "kit", "part": "hair", "tip": "#ffeedd", "slots": false}),
    );
    assert_eq!(at(&doc, &format!("{SHADING}/tip")), Some(json!("#ffeedd")));
    let e = refused(&doc, "hair.recolour", json!({"character": "kit", "part": "hair"}));
    assert_error(&e, ErrorCode::Invalid, "root", &["at least one"], &[]);
    let e = refused(&doc, "hair.recolour", json!({"character": "kit", "part": "hair", "mid": "purple"}));
    assert_error(&e, ErrorCode::Invalid, "mid", &["purple"], &[]);
}

#[test]
fn recolour_brings_the_shadow_lines_outline_ring_and_rim_with_the_new_colour() {
    // Platinum hair takes a light, near neutral shadow and pale lines, not the kit brown's dark ones.
    let mut doc = kit();
    let before = at(&doc, &format!("{SHADING}/shade"));
    round_trip(&mut doc, "hair.recolour", json!({"character": "kit", "part": "hair", "mid": "#f1f3f7"}));
    let colour = |doc: &MemDoc, field: &str| -> Colour {
        serde_json::from_value(at(doc, &format!("{SHADING}/{field}")).unwrap()).unwrap()
    };
    let companions = omuzaki_char::hair::colour::companions("#f1f3f7".parse().unwrap());
    for (field, want) in [
        ("shade", companions.shade),
        ("line", companions.line),
        ("outline", companions.outline),
        ("ring", companions.ring),
        ("rim", companions.rim),
    ] {
        assert_eq!(colour(&doc, field), want, "{field}");
    }
    assert_ne!(at(&doc, &format!("{SHADING}/shade")), before);
    let shade = colour(&doc, "shade");
    assert!(shade.r > 200 && shade.r.abs_diff(shade.b) < 12, "a light, neutral shadow: {shade:?}");
    // `derive: false` leaves them as they were.
    round_trip(
        &mut doc,
        "hair.recolour",
        json!({"character": "kit", "part": "hair", "mid": "#d04080", "derive": false}),
    );
    assert_eq!(colour(&doc, "shade"), companions.shade);
}
