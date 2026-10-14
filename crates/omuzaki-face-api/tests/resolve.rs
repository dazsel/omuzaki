//! Palette resolution (LK-03, gap G1): every `@slot` colour becomes the palette's fixed colour, and nothing else changes.

// Test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_core::colour::Colour;
use omuzaki_face_api::{ColourRef, FaceMark, FaceProfile, Palette, PaletteSlot, ResolveError, Validate};
use proptest::prelude::*;
use serde_json::Value;

/// The slots the property test's palette defines, and so every slot a random profile can use.
const SLOTS: [&str; 6] = ["skin", "eyes", "hair", "blush", "lip", "trim"];

fn slot(name: &str) -> PaletteSlot {
    PaletteSlot::new(name).unwrap()
}

fn palette(entries: &[(&str, Colour)]) -> Palette {
    Palette(entries.iter().map(|(name, c)| (slot(name), *c)).collect())
}

/// The default profile with the iris top on `@eyes` and the blush on `@blush`.
fn eyes_and_blush() -> FaceProfile {
    let mut p = FaceProfile::default();
    p.eyes.iris.top = ColourRef::parse("@eyes").unwrap();
    p.cheeks.colour = ColourRef::parse("@blush").unwrap();
    p
}

/// Visits every string in a JSON tree, mutably.
fn for_each_string(node: &mut Value, visit: &mut impl FnMut(&mut String)) {
    match node {
        Value::String(text) => visit(text),
        Value::Array(items) => items.iter_mut().for_each(|item| for_each_string(item, visit)),
        Value::Object(fields) => fields.values_mut().for_each(|value| for_each_string(value, visit)),
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn tree(profile: &FaceProfile) -> Value {
    serde_json::to_value(profile).unwrap()
}

#[test]
fn slots_become_the_palette_colours() {
    let pal = palette(&[("eyes", Colour::rgb(1, 2, 3)), ("blush", Colour::rgba(10, 20, 30, 128))]);
    let resolved = eyes_and_blush().resolve_colours(&pal).unwrap();
    assert_eq!(resolved.eyes.iris.top, ColourRef::rgb(1, 2, 3));
    assert_eq!(resolved.cheeks.colour, ColourRef::rgba(10, 20, 30, 128));
    assert!(resolved.slots_used().is_empty(), "a resolved profile names no slot");
}

#[test]
fn an_unknown_slot_names_its_json_path() {
    let err = eyes_and_blush().resolve_colours(&palette(&[("blush", Colour::BLACK)])).unwrap_err();
    assert_eq!(err, ResolveError::UnknownSlot { path: "eyes/iris/top".to_owned(), slot: slot("eyes") });
    assert_eq!(err.to_string(), "eyes/iris/top: the palette has no slot 'eyes'");
}

#[test]
fn the_first_unknown_slot_in_key_order_is_the_one_reported() {
    // Both slots are missing; `cheeks` sorts before `eyes`, so the error names it every time.
    let err = eyes_and_blush().resolve_colours(&Palette::default()).unwrap_err();
    assert!(matches!(&err, ResolveError::UnknownSlot { path, .. } if path == "cheeks/colour"), "{err}");
}

#[test]
fn a_slot_inside_a_list_gets_an_index_in_its_path() {
    let mut p = FaceProfile::default();
    p.marks.push(FaceMark { colour: ColourRef::parse("@trim").unwrap(), ..FaceMark::default() });
    let err = p.resolve_colours(&Palette::default()).unwrap_err();
    assert!(matches!(&err, ResolveError::UnknownSlot { path, .. } if path == "marks/0/colour"), "{err}");
}

#[test]
fn resolving_twice_equals_resolving_once() {
    let pal = palette(&[("eyes", Colour::rgb(1, 2, 3)), ("blush", Colour::rgb(4, 5, 6))]);
    let once = eyes_and_blush().resolve_colours(&pal).unwrap();
    assert_eq!(once.resolve_colours(&pal).unwrap(), once);
}

#[test]
fn the_default_profile_names_no_slot_and_resolves_to_itself() {
    let default = FaceProfile::default();
    assert!(default.slots_used().is_empty());
    assert_eq!(default.resolve_colours(&Palette::default()).unwrap(), default);
}

#[test]
fn slots_used_lists_each_named_slot_once_in_order() {
    let mut p = eyes_and_blush();
    p.marks.push(FaceMark { colour: ColourRef::parse("@blush").unwrap(), ..FaceMark::default() });
    assert_eq!(p.slots_used(), BTreeSet::from([slot("blush"), slot("eyes")]));
}

/// The default profile's colour leaves (the `#rrggbb` strings) are the places a random profile may put a slot.
fn colour_leaf_count() -> usize {
    let mut n = 0;
    for_each_string(&mut tree(&FaceProfile::default()), &mut |text| {
        if text.starts_with('#') {
            n += 1;
        }
    });
    n
}

/// A profile with a random slot (or none) on each colour leaf, and a palette that defines every slot.
fn random_case() -> impl Strategy<Value = (FaceProfile, Palette)> {
    let choices = proptest::collection::vec(proptest::option::of(0..SLOTS.len()), colour_leaf_count());
    let colours = proptest::collection::vec(any::<(u8, u8, u8, u8)>(), SLOTS.len());
    (choices, colours).prop_map(|(choices, colours)| {
        let mut json = tree(&FaceProfile::default());
        let mut choices = choices.into_iter();
        for_each_string(&mut json, &mut |text| {
            if !text.starts_with('#') {
                return;
            }
            if let Some(Some(i)) = choices.next() {
                *text = format!("@{}", SLOTS[i]);
            }
        });
        let profile = serde_json::from_value(json).unwrap();
        let entries = SLOTS.iter().zip(colours).map(|(name, (r, g, b, a))| (slot(name), Colour::rgba(r, g, b, a)));
        (profile, Palette(entries.collect::<BTreeMap<_, _>>()))
    })
}

/// The expected JSON: the input with every `@slot` replaced by its palette colour, and nothing else touched.
fn expected(profile: &FaceProfile, palette: &Palette) -> Value {
    let mut json = tree(profile);
    for_each_string(&mut json, &mut |text| {
        if let Some(name) = text.strip_prefix('@') {
            *text = palette.0[&slot(name)].to_string();
        }
    });
    json
}

proptest! {
    #[test]
    fn any_palette_that_defines_the_used_slots_resolves_to_a_valid_slot_free_profile((profile, palette) in random_case()) {
        let resolved = profile.resolve_colours(&palette).unwrap();
        resolved.validate().unwrap();
        prop_assert!(resolved.slots_used().is_empty());
        let json = serde_json::to_string(&resolved).unwrap();
        prop_assert!(!json.contains('@'), "a slot survived: {json}");
        prop_assert_eq!(tree(&resolved), expected(&profile, &palette));
    }
}
