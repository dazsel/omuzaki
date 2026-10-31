//! `character.create_from_description` on the kit: the words it reads, every option planning on a fresh character,
//! the 20 judged descriptions (`fixtures/descriptions.toml`) reading as they should and building with a self-check
//! that holds on several seeds, and the same words and seed always making the same character.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;

use omuzaki_brief::lexicon::Lexicon;
use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, paths};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocRead, DocWrite, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Grants, OpCtx, OpError, find};
use serde::Deserialize;
use serde_json::json;

const FIXTURES: &str = include_str!("../fixtures/descriptions.toml");

#[derive(Deserialize)]
struct Fixtures {
    case: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    description: String,
    expect: BTreeMap<String, Value>,
}

/// Plans `op` with `args` on `doc` as a person, and applies it; the plan's output.
fn run(doc: &mut MemDoc, op: &str, args: Value) -> Result<Value, OpError> {
    omuzaki_brief::link();
    let (actor, clock, grants) = (Actor::Person { name: "test".into() }, FakeClock::default(), Grants::person());
    let registration = find(op).unwrap_or_else(|| panic!("no op {op}"));
    let op = (registration.decode)(args)?;
    let plan = op.plan(&OpCtx::new(doc, &actor, &clock).with_grants(&grants))?;
    doc.apply(&plan.patch).expect("the plan applies");
    Ok(plan.output.unwrap_or_default())
}

fn create(doc: &mut MemDoc, args: Value) -> Value {
    let shown = args.to_string();
    run(doc, "character.create_from_description", args).unwrap_or_else(|e| panic!("{shown}: {e}"))
}

/// The finished character the answer names, read back.
fn character(doc: &MemDoc, answer: &Value) -> CharacterDef {
    let id = CharId::new(answer["character"].as_str().unwrap()).unwrap();
    CharacterDef::from_node(doc.get(&paths::character(&id)).expect("stored")).expect("reads back")
}

/// The choice for `facet` in the answer, as its value.
fn choice<'a>(answer: &'a Value, facet: &str) -> Vec<&'a str> {
    answer["choices"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["facet"] == facet)
        .filter_map(|c| c["value"].as_str())
        .collect()
}

/// Builds the answer's character with the kit's builder, as the viewport would: every part's generator accepts what
/// the words made (a tie hangs on its shirt's collar, a garment finds its body).
fn assert_builds(doc: &MemDoc, answer: &Value, what: &str) {
    let def = character(doc, answer);
    let id = CharId::new(answer["character"].as_str().unwrap()).unwrap();
    let services = BuildServices::default().with_rasterizer(|| Box::new(omuzaki_face::raster::SkiaRasterizer::new()));
    let built = CachingBuilder::new().build(&id, &def, &BuildOptions::default(), &services);
    assert!(
        built.is_ok(),
        "{what}: the character doesn't build: {}",
        built.err().map(|e| e.to_string()).unwrap_or_default()
    );
}

fn assert_holds(answer: &Value, what: &str) {
    let check = &answer["self_check"];
    assert_eq!(check["holds"], json!(true), "{what}: the self-check failed: {:#}", check["checks"]);
}

#[test]
fn the_lexicon_reads_and_every_phrase_means_one_thing() {
    let lexicon = Lexicon::builtin().unwrap_or_else(|e| panic!("{e}"));
    assert!(lexicon.option.len() > 100, "{} options", lexicon.option.len());
    assert!(lexicon.phrases().count() > 300, "{} phrases", lexicon.phrases().count());
}

#[test]
fn every_option_plans_on_a_fresh_character_and_holds() {
    let lexicon = Lexicon::builtin().unwrap();
    for option in &lexicon.option {
        // The mature style needs a build that fits it.
        let mut facets = json!({ option.facet.clone(): [option.id.clone()] });
        if let Some(fits) = option.fits.first() {
            facets["build"] = json!([if fits == "tall" { "tall" } else { "average" }]);
        }
        let mut doc = MemDoc::new();
        let what = format!("{}.{}", option.facet, option.id);
        let answer =
            run(&mut doc, "character.create_from_description", json!({"description": "someone", "facets": facets}))
                .unwrap_or_else(|e| panic!("{what}: {e}"));
        assert_holds(&answer, &what);
        assert_builds(&doc, &answer, &what);
        let def = character(&doc, &answer);
        for part in &option.parts {
            let found = def.parts.iter().find(|(id, _)| id.as_str() == part.id);
            assert!(found.is_some_and(|(_, p)| p.kind.as_str() == part.kind), "{what}: no {} part", part.kind);
        }
    }
}

#[test]
fn the_twenty_descriptions_read_as_written_and_build_on_three_seeds() {
    let fixtures: Fixtures = toml::from_str(FIXTURES).unwrap();
    assert_eq!(fixtures.case.len(), 20);
    let mut missed = Vec::new();
    for case in &fixtures.case {
        for seed in [0, 1, 2] {
            let mut doc = MemDoc::new();
            let answer = create(&mut doc, json!({"description": case.description, "seed": seed}));
            assert_holds(&answer, &case.description);
            assert_builds(&doc, &answer, &case.description);
            if seed > 0 {
                continue;
            }
            for (facet, want) in &case.expect {
                let got = choice(&answer, facet);
                let wanted: Vec<&str> = match want {
                    Value::Array(all) => all.iter().filter_map(Value::as_str).collect(),
                    one => vec![one.as_str().unwrap()],
                };
                if got != wanted {
                    missed.push(format!("\"{}\": {facet} is {got:?}, wanted {wanted:?}", case.description));
                }
            }
        }
    }
    assert!(missed.is_empty(), "{} readings differ:\n{}", missed.len(), missed.join("\n"));
}

#[test]
fn a_description_reads_into_its_choices_with_the_words_that_made_them() {
    let mut doc = MemDoc::new();
    let answer =
        create(&mut doc, json!({"description": "a sleepy librarian with silver twin tails and round glasses"}));
    assert_eq!(answer["name"], "Sleepy Librarian");
    assert_eq!(answer["character"], "sleepy-librarian");
    let twin = answer["choices"].as_array().unwrap().iter().find(|c| c["facet"] == "hair_style").unwrap();
    assert_eq!(
        (twin["value"].as_str(), twin["because"].as_str(), twin["from"].as_str()),
        (Some("twintails"), Some("twin tails"), Some("description"))
    );
    let hair = answer["choices"].as_array().unwrap().iter().find(|c| c["facet"] == "hair_colour").unwrap();
    assert_eq!((hair["value"].as_str(), hair["because"].as_str()), (Some("silver"), Some("silver twin tails")));
    let def = character(&doc, &answer);
    assert!(def.parts.iter().any(|(id, p)| id.as_str() == "glasses" && p.kind.as_str() == "glasses"));
    assert_eq!(answer["ops"][0]["op"], "character.create");
    assert_eq!(answer["next"][0]["op"], "render.preview");
}

#[test]
fn a_second_character_from_the_same_words_gets_a_free_id() {
    let mut doc = MemDoc::new();
    let first = create(&mut doc, json!({"description": "a knight"}));
    let second = create(&mut doc, json!({"description": "a knight", "seed": 4}));
    assert_eq!((first["character"].as_str(), second["character"].as_str()), (Some("knight"), Some("knight-2")));
    let named = create(&mut doc, json!({"description": "a girl named Ember with red hair"}));
    assert_eq!(named["name"], "Ember");
}

#[test]
fn the_same_words_and_seed_make_the_same_character_and_another_seed_another() {
    let make = |seed: u32| {
        let mut doc = MemDoc::new();
        let answer = create(&mut doc, json!({"description": "a cheerful girl", "seed": seed}));
        serde_json::to_value(character(&doc, &answer)).unwrap()
    };
    assert_eq!(make(5), make(5));
    assert!((0..6).map(make).collect::<Vec<_>>().windows(2).any(|w| w[0] != w[1]), "the priors never vary");
}

#[test]
fn what_is_not_understood_or_set_aside_is_said() {
    let mut doc = MemDoc::new();
    let answer =
        create(&mut doc, json!({"description": "a wizard in a long dress, no glasses, with a zorbly scarf and teal"}));
    let unknown: Vec<&str> = answer["not_understood"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(unknown.contains(&"zorbly") && unknown.contains(&"scarf"), "{unknown:?}");
    let aside: Vec<&str> = answer["set_aside"].as_array().unwrap().iter().filter_map(|a| a["words"].as_str()).collect();
    assert!(aside.contains(&"glasses"), "a negated choice is set aside: {aside:?}");
    assert!(aside.contains(&"long"), "`long` before a dress is not hair: {aside:?}");
    assert!(aside.contains(&"teal"), "a colour with nothing to colour is set aside: {aside:?}");
    // The garment beats the hint: `wizard` would be a robe, but a dress is named.
    assert_eq!(choice(&answer, "outfit"), ["dress"]);
}

#[test]
fn facets_named_directly_win_over_the_words_and_are_checked() {
    let mut doc = MemDoc::new();
    let answer = create(
        &mut doc,
        json!({"description": "a girl with blue hair", "facets": {"hair_colour": ["#c0c8d8"], "outfit": ["kimono"]}}),
    );
    assert_eq!(choice(&answer, "hair_colour"), ["silver"]);
    assert_eq!(choice(&answer, "outfit"), ["kimono"]);
    let refused = run(
        &mut doc,
        "character.create_from_description",
        json!({"description": "x", "facets": {"outfit": ["spacesuit"]}}),
    );
    let e = refused.expect_err("an unknown option is refused");
    assert!(e.message.contains("spacesuit") && e.message.contains("kimono"), "{}", e.message);
}

#[test]
fn a_gradient_and_a_modified_colour_reach_the_hair() {
    let mut doc = MemDoc::new();
    let answer = create(&mut doc, json!({"description": "a girl with pink hair fading to blonde"}));
    assert_eq!(choice(&answer, "hair_colour"), ["pink to blonde"]);
    let answer = create(&mut doc, json!({"description": "a girl with dark brown hair", "id": "dark"}));
    let value = choice(&answer, "hair_colour");
    assert!(value[0].starts_with('#'), "dark brown is a shade of brown, not a named colour: {value:?}");
}

#[test]
fn the_vocabulary_lists_every_facet_and_the_colours() {
    let mut doc = MemDoc::new();
    let all = run(&mut doc, "character.brief_vocabulary", json!({})).unwrap();
    let lexicon = Lexicon::builtin().unwrap();
    assert_eq!(all["facets"].as_array().unwrap().len(), lexicon.facet.len());
    assert!(all["colours"].as_array().unwrap().len() >= 15);
    let hair = run(&mut doc, "character.brief_vocabulary", json!({"facet": "hair_style"})).unwrap();
    assert_eq!(hair["facets"].as_array().unwrap().len(), 1);
    assert!(hair.get("colours").is_none());
    assert!(run(&mut doc, "character.brief_vocabulary", json!({"facet": "nope"})).is_err());
}

#[test]
fn a_prior_never_contradicts_the_words_and_other_words_draw_other_priors() {
    let trousers = ["cloak", "blazer", "vest", "robe", "kimono", "bodysuit"];
    for seed in 0..12 {
        let mut doc = MemDoc::new();
        let answer = create(&mut doc, json!({"description": "a fairy with bare legs", "seed": seed}));
        let outfit = choice(&answer, "outfit");
        assert!(outfit.iter().all(|o| !trousers.contains(o)), "seed {seed}: bare legs under {outfit:?}");
    }
    // A mood picks the outfit's colours: a gothic vampire never draws a white dress.
    for seed in 0..6 {
        let mut doc = MemDoc::new();
        let answer = create(&mut doc, json!({"description": "a gothic vampire in a dress", "seed": seed}));
        let colour = choice(&answer, "outfit_colour");
        assert!(colour.iter().all(|c| ["black", "maroon", "purple"].contains(c)), "seed {seed}: {colour:?}");
    }
    // Unnamed choices follow the words as well as the seed: twenty descriptions on one seed don't all wear one outfit.
    let fixtures: Fixtures = toml::from_str(FIXTURES).unwrap();
    let outfits: std::collections::BTreeSet<String> = fixtures
        .case
        .iter()
        .filter(|case| !case.expect.contains_key("outfit"))
        .map(|case| {
            let mut doc = MemDoc::new();
            let answer = create(&mut doc, json!({"description": case.description}));
            choice(&answer, "outfit").join(",")
        })
        .collect();
    assert!(outfits.len() > 1, "every unnamed outfit is {outfits:?}");
}
