//! The mock kinds' `recolour` against their `build` (decision Q6): for every part of the mock character, recolouring the
//! output built under one palette gives exactly the output built under another, and the hair's vertex tints and the
//! garments' materials are the only things that changed. The conformance check every kind that overrides
//! `PartKind::recolour` owes.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{
    Assembler, BuildOptions, BuildServices, CharId, CharacterDef, PartId, PartOutput, Quality, Recoloured, build_part,
    plan, recolour_part,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::colour::Colour;
use omuzaki_face_api::PaletteSlot;
use serde_json::{Value, json};

fn char_id() -> CharId {
    CharId::new("mock").unwrap()
}

/// Every part of `def` built the way a builder builds it: in plan order, each against what the parts before it published.
fn build_all(def: &CharacterDef, options: &BuildOptions) -> Vec<(PartId, PartOutput)> {
    let (id, services) = (char_id(), BuildServices::default());
    let mut assembler = Assembler::new(&id, def, options);
    let mut outputs = Vec::new();
    for part in plan(def).unwrap() {
        // A part's output can't be copied (its simulation), but building is pure: build it once to keep, once to merge.
        let kept = build_part(def, &part, options, &services, assembler.published()).unwrap();
        let merged = build_part(def, &part, options, &services, assembler.published()).unwrap();
        assembler.add(&part, merged).unwrap();
        outputs.push((part, kept));
    }
    outputs
}

/// All of an output that a recolour could touch or must leave alone, as exact text (floats print round-trippably).
fn exact(out: &PartOutput) -> String {
    let runtime = out.runtime.as_ref().map(|r| {
        let meshes: Vec<_> = (0..r.dynamic_count()).map(|i| r.dynamic_mesh(i)).collect();
        format!("{meshes:?} {:?}", r.cost())
    });
    format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{runtime:?}",
        out.meshes, out.materials, out.textures, out.publishes, out.rig, out.warnings
    )
}

fn recolour_between(before: &CharacterDef, after: &CharacterDef, options: &BuildOptions) {
    let services = BuildServices::default();
    let want = build_all(after, options);
    for ((part, mut out), (_, want)) in build_all(before, options).into_iter().zip(want) {
        let done = recolour_part(after, &part, &services, &mut out);
        assert_eq!(done, Ok(Recoloured::Done), "{part}");
        assert_eq!(exact(&out), exact(&want), "{part}: recolour(build(a), b) == build(b)");
    }
}

fn colourway(def: &mut CharacterDef, n: u8) {
    for (i, name) in ["skin", "skin_shade", "skin_line", "outfit", "streak"].into_iter().enumerate() {
        let k = n.wrapping_add(u8::try_from(i).unwrap() * 40);
        def.palette.0.insert(PaletteSlot::new(name).unwrap(), Colour::rgb(k, k.wrapping_mul(5), 255 - k));
    }
}

fn set_hair(def: &mut CharacterDef, params: Value) {
    def.parts.get_mut(&PartId::new("hair").unwrap()).unwrap().params = params;
}

/// A hairstyle with every group the mock builds, streaks from the palette over some of them, and the chosen simulation.
fn streaked(simulate: bool) -> Value {
    json!({
        "groups": {
            "a-cap": { "generator": { "type": "cap" } },
            "b-bangs": { "generator": { "type": "bangs" } },
            "c-curtain": { "generator": { "type": "curtain" }, "dynamics": { "simulate": simulate, "follow": "body" } },
            "d-tail": { "generator": { "type": "curtain", "count": 7 }, "dynamics": { "simulate": !simulate } },
        },
        "shading": {
            "mid": "@skin",
            "line": "@skin_line",
            "streaks": [
                { "fraction": 0.5, "colour": "@streak", "strength": 0.8, "seed": 3, "blend": "replace" },
                { "fraction": 0.4, "colour": "@outfit", "strength": 0.6, "seed": 9, "blend": "multiply",
                  "groups": ["c-curtain", "b-bangs"] },
            ],
        },
    })
}

#[test]
fn recolouring_the_mock_character_is_building_it_with_the_new_palette() {
    let options = BuildOptions::default();
    let mut before = mock_character().unwrap();
    before.palette.0.insert(PaletteSlot::new("streak").unwrap(), Colour::rgb(1, 2, 3));
    let mut after = before.clone();
    colourway(&mut after, 90);
    recolour_between(&before, &after, &options);
}

#[test]
fn recolouring_streaked_hair_rewrites_the_tints_of_the_clumps_the_streaks_reach() {
    for simulate in [true, false] {
        for quality in [Quality::Live, Quality::Offline] {
            let options = BuildOptions { quality, ..BuildOptions::default() };
            let mut before = mock_character().unwrap();
            set_hair(&mut before, streaked(simulate));
            colourway(&mut before, 5);
            let mut after = before.clone();
            colourway(&mut after, 120);
            recolour_between(&before, &after, &options);

            // The tints really are what moved: the vertex colours differ between the two palettes.
            let colours = |def: &CharacterDef| {
                let hair = build_all(def, &options).into_iter().find(|(p, _)| p.as_str() == "hair").unwrap().1;
                hair.meshes.iter().map(|m| m.geometry.colours.clone()).collect::<Vec<_>>()
            };
            assert_ne!(colours(&before), colours(&after), "simulate {simulate}: the streaks changed colour");
        }
    }
}

#[test]
fn hair_whose_vertices_do_not_match_its_clumps_is_declined_not_guessed() {
    let options = BuildOptions::default();
    let mut before = mock_character().unwrap();
    set_hair(&mut before, streaked(true));
    colourway(&mut before, 5);
    let mut after = before.clone();
    colourway(&mut after, 60);
    let hair = PartId::new("hair").unwrap();
    let (_, mut out) = build_all(&before, &options).into_iter().find(|(p, _)| *p == hair).unwrap();
    out.meshes[0].geometry.colours.pop();
    assert_eq!(
        recolour_part(&after, &hair, &BuildServices::default(), &mut out),
        Ok(Recoloured::NeedsRebuild),
        "a mesh the build didn't make is rebuilt"
    );
}
