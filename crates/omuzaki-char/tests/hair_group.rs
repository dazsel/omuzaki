//! Hair groups added and removed by op (#2452): a braid, a tail and a bun join the kit's own hair part as groups of it,
//! with their generators' defaults or the fields given, apply and undo exactly and build; a taken id, a missing type
//! and the last group are refused with what to do instead.

#![allow(clippy::expect_used, clippy::unwrap_used)] // test helpers: a panic is a clear failure

mod common;

use common::{assert_error, refused, round_trip};
use omuzaki_char_api::{Generator, GroupId, HairParams, PartId};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::json;

fn kit() -> MemDoc {
    let mut doc = MemDoc::new();
    common::apply(&mut doc, "character.create", json!({"id": "kit", "name": "Kit", "template": "tall"}));
    doc
}

fn hair(doc: &MemDoc) -> HairParams {
    let def = common::definition(doc, "kit");
    let stored = &def.parts[&PartId::new("hair").unwrap()].params;
    let kind = omuzaki_char_api::find_kind("hair").unwrap();
    serde_json::from_value((kind.check)(stored).unwrap()).unwrap()
}

#[test]
fn a_braid_a_tail_and_a_bun_join_the_hair_part_and_build() {
    let mut doc = kit();
    let before = hair(&doc).groups.len();
    round_trip(
        &mut doc,
        "hair.group.add",
        json!({"character": "kit", "part": "hair", "group": "e-braid", "generator": {"type": "braid", "strands": 4}}),
    );
    round_trip(
        &mut doc,
        "hair.group.add",
        json!({"character": "kit", "part": "hair", "group": "f-tail", "generator": {"type": "tail"}, "simulate": true}),
    );
    round_trip(
        &mut doc,
        "hair.group.add",
        json!({"character": "kit", "part": "hair", "group": "g-bun", "generator": {"type": "bun"}}),
    );

    let groups = hair(&doc).groups;
    assert_eq!(groups.len(), before + 3);
    match &groups[&GroupId::new("e-braid").unwrap()].generator {
        Generator::Braid(braid) => assert_eq!(braid.strands, 4, "the field given is kept"),
        other => panic!("e-braid is not a braid: {other:?}"),
    }
    assert!(groups[&GroupId::new("f-tail").unwrap()].dynamics.simulate, "simulate is set");
    assert!(!groups[&GroupId::new("g-bun").unwrap()].dynamics.simulate, "rigid unless asked");
    let built = common::build(&doc, "kit");
    assert!(!built.meshes.is_empty(), "the character still builds");
}

#[test]
fn removing_a_group_undoes_exactly_and_the_last_one_stays() {
    let mut doc = kit();
    let ids: Vec<String> = hair(&doc).groups.keys().map(ToString::to_string).collect();
    let (last, rest) = ids.split_last().unwrap();
    for id in rest {
        round_trip(&mut doc, "hair.group.remove", json!({"character": "kit", "part": "hair", "group": id}));
    }
    let e = refused(&doc, "hair.group.remove", json!({"character": "kit", "part": "hair", "group": last}));
    assert_error(&e, ErrorCode::Invalid, "group", &["only group"], &["part.remove"]);
    let e = refused(&doc, "hair.group.remove", json!({"character": "kit", "part": "hair", "group": "z-none"}));
    assert_error(&e, ErrorCode::NotFound, "group", &["z-none"], &[last.as_str()]);
}

#[test]
fn a_taken_id_and_a_generator_without_a_type_are_refused() {
    let doc = kit();
    let taken = hair(&doc).groups.keys().next().unwrap().to_string();
    let e = refused(
        &doc,
        "hair.group.add",
        json!({"character": "kit", "part": "hair", "group": taken, "generator": {"type": "bun"}}),
    );
    assert_error(&e, ErrorCode::Invalid, "group", &["already has"], &["free id"]);
    let e =
        refused(&doc, "hair.group.add", json!({"character": "kit", "part": "hair", "group": "e-x", "generator": {}}));
    assert_error(&e, ErrorCode::Invalid, "generator", &["type"], &["braid"]);
    let e = refused(
        &doc,
        "hair.group.add",
        json!({"character": "kit", "part": "hair", "group": "e-x", "generator": {"type": "plait"}}),
    );
    assert_error(&e, ErrorCode::Invalid, "generator", &["plait"], &["braid"]);
}
