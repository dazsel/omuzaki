//! A garment that paints its texture (the default character's plaid skirt) builds with the services every surface passes by default: the
//! character build paints with the rasteriser `omuzaki-face` links (`register_rasterizer!`), so the studio, the CLI,
//! MCP and export need no rasteriser wired in.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, RegistryBuilder};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, find};
use serde_json::json;

fn run(doc: &mut MemDoc, op: &str, args: serde_json::Value) {
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    let op_ = (find(op).expect("registered").decode)(args).expect("the arguments decode");
    let plan = op_.plan(&OpCtx::new(doc, &actor, &clock)).unwrap_or_else(|e| panic!("{op}: {e:?}"));
    doc.apply(&plan.patch).expect("the patch applies");
}

#[test]
fn a_plaid_skirt_builds_with_the_default_services_painted_by_the_linked_rasteriser() {
    omuzaki_stage::link();
    assert!(omuzaki_char_api::linked_rasterizer().is_some(), "omuzaki-face registers its rasteriser");
    let mut doc = MemDoc::new();
    run(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "soft-sd"}));
    let node = doc.get(&DocPath::parse("characters/kit").unwrap()).expect("stored").clone();
    let def = CharacterDef::from_node(&node).expect("reads back");
    let built = RegistryBuilder
        .build(&CharId::new("kit").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap_or_else(|e| panic!("the skirt doesn't build with the default services: {e}"));
    assert!(built.meshes.iter().any(|m| m.part.as_str() == "skirt"), "the skirt built");
    let (_, texture) = built
        .textures
        .iter()
        .find(|(name, _)| name.as_str().contains("skirt"))
        .expect("the plaid is a painted texture");
    let pixels = &texture.rgba8;
    assert!(pixels.chunks(4).any(|p| p != &pixels[..4]), "the plaid is painted, not a flat fill");
}
