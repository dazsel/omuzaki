//! A colourway swap on the kit's default character (#644): body, head, hair, shirt, skirt, legwear and boots. Every
//! part a colourway reaches recolours from the cache (no generator runs), the result is byte for byte a fresh build
//! under the new palette, and the swap fits its budget.
//!
//! Measured optimised on a 4-core cloud VM, median of five fresh colourways: 3.0 ms for the default character (its
//! palette is the skin, so the body and head recolour and the rest is served); about 30 ms when every colour of every
//! part is a palette slot, of which the skirt's plaid, repainted through the test rasteriser, is 22 ms and every other
//! part at most 1.5 ms. Tinting the plaid in the shader instead of repainting it is the follow-up (#2371).

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

mod common;

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use omuzaki_char::builder::CachingBuilder;
use omuzaki_char::builder::conformance::{PALETTES, alternatives};
use omuzaki_char_api::{
    BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, RegistryBuilder, find_kind,
};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::{ColourRef, Palette, PaletteSlot};
use omuzaki_face_mock::MockRasterizer;
use serde_json::{Value, json};

/// The budget of a colourway swap of the default character: 5 ms optimised (the issue's bar); a debug build is several
/// times slower, so it gets a ceiling that still catches a part rebuilding its geometry.
const BUDGET: Duration = if cfg!(debug_assertions) { Duration::from_millis(120) } else { Duration::from_millis(5) };
/// The ceiling of a swap that reaches every part: the plaid repaint dominates it (see the module doc). A rebuild of the
/// garments costs several times more.
const EVERY_PART_CEILING: Duration =
    if cfg!(debug_assertions) { Duration::from_millis(600) } else { Duration::from_millis(60) };

fn services() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

/// The default character with every fixed colour in its parts' parameters moved into a palette slot of its own (same
/// colour, so it builds the same): a colourway then reaches every garment and the hair, not only the skin.
fn every_colour_a_slot() -> CharacterDef {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    let mut def = common::definition(&doc, "kit");
    let mut n = 0;
    for part in def.parts.values_mut() {
        let kind = find_kind(part.kind.as_str()).unwrap();
        let mut params = (kind.check)(&part.params).unwrap();
        slot_every_colour(&mut params, &mut def.palette, &mut n);
        part.params = params;
    }
    def
}

fn slot_every_colour(value: &mut Value, palette: &mut Palette, n: &mut usize) {
    match value {
        Value::String(text) => {
            if let Ok(ColourRef::Fixed(colour)) = ColourRef::parse(text) {
                *n += 1;
                let slot = PaletteSlot::new(format!("c{n}")).unwrap();
                *text = format!("@{slot}");
                palette.0.insert(slot, colour);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| slot_every_colour(v, palette, n)),
        Value::Object(fields) => fields.values_mut().for_each(|v| slot_every_colour(v, palette, n)),
        _ => {}
    }
}

#[test]
fn a_colourway_swap_on_the_default_character_recolours_every_part_within_budget() {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    swap_within_budget(&common::definition(&doc, "kit"), BUDGET);
}

#[test]
fn a_colourway_swap_that_reaches_every_garment_recolours_every_part_within_budget() {
    swap_within_budget(&every_colour_a_slot(), EVERY_PART_CEILING);
}

fn swap_within_budget(def: &CharacterDef, budget: Duration) {
    let (id, options, services) = (CharId::new("kit").unwrap(), BuildOptions::default(), services());
    let builder = CachingBuilder::new();
    let (mut character, _) = builder.build_timed(&id, def, &options, &services).unwrap();

    // Five colourways, each differing from the character's in every slot and from each other: every swap is to a palette
    // the cache has not seen, so every part that names a slot is recoloured, never served.
    let colourways: Vec<Palette> = alternatives(&def.palette, &BTreeSet::new()).into_iter().take(PALETTES).collect();
    let mut took = Vec::new();
    for (round, colourway) in colourways.iter().enumerate() {
        let started = Instant::now();
        let timings = builder.rebuild_materials(&mut character, colourway, &options, &services).unwrap();
        took.push(started.elapsed());
        let per_part: Vec<String> =
            timings.parts.iter().map(|t| format!("{} {:?} {:?}", t.part, t.source, t.elapsed)).collect();
        eprintln!("swap {round}: {:?}\n  {}", took[round], per_part.join("\n  "));
        let built: Vec<String> = timings.built().map(ToString::to_string).collect();
        assert!(built.is_empty(), "every part recolours, none is built: {built:?}");
    }
    took.sort();
    let median = took[took.len() / 2];
    eprintln!("median swap: {median:?} (budget {budget:?})");

    let mut want = def.clone();
    want.palette = colourways[PALETTES - 1].clone();
    let fresh = RegistryBuilder.build(&id, &want, &options, &services).unwrap();
    assert_eq!(character.meshes, fresh.meshes);
    assert_eq!(character.materials, fresh.materials);
    assert_eq!(character.textures, fresh.textures);
    assert!(median <= budget, "a colourway swap took {median:?}, over {budget:?}");
}
