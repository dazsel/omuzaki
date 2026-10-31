//! `character.create` and the part ops (`part.add`, `part.remove`, `part.set_enabled`, `part.reorder`,
//! `part.duplicate`) against the in-memory document.
//!
//! Each op is decoded from JSON and planned exactly as the executor does (`omuzaki-ops` is an implementation crate, so
//! it is not a dependency here). The round trip the executor gives every edit is spelled out in `common::round_trip`: a
//! dry-run (the plan alone) changes nothing, applying the patch changes the document, its inverse restores the
//! document exactly, and applying it again gives the same result as the first time.
//!
//! The parts come from the mock kinds (`mock-body`, `mock-head`, `mock-hair`, `mock-garment`), which take the real
//! parameter types. `character.create` makes the kit's default character (`library/starter.toml`): parts of the kinds
//! `body`, `head`, `hair`, `legwear`, `shirt`, `skirt` and `boots`, which are the real generators this crate registers.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{apply, assert_error, at, build, definition, mock_node, plan, project, refused, round_trip};
use omuzaki_char::template::registry;
use omuzaki_char_api::{HairParams, find_kind};
use omuzaki_char_mock::VARIANTS_JSON;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{ErrorCode, OpKind, Scope, Undo, find};
use serde_json::{Value, json};

const OPS: [&str; 7] = [
    "character.create",
    "character.rename",
    "part.add",
    "part.remove",
    "part.set_enabled",
    "part.reorder",
    "part.duplicate",
];

// ------------------------------------------------------------------------------------------------ harness

/// `project()` after `edit` changed the character's node.
fn project_with(edit: impl FnOnce(&mut Value)) -> MemDoc {
    let mut node = mock_node();
    edit(&mut node);
    MemDoc::from_value(json!({ "characters": { "mock": node } }))
}

fn built_parts(doc: &MemDoc, id: &str) -> Vec<String> {
    build(doc, id).report.parts.iter().map(|p| p.part.to_string()).collect()
}

fn names(value: &Value) -> Vec<&str> {
    value.as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect()
}

const BRUSH: &str = r#"{"centre":[0.0,1.0,0.0],"radius":0.1,"joint":{"humanoid":"head"},"strength":0.5}"#;

// ------------------------------------------------------------------------------------------------ the ops as an agent sees them

#[test]
fn every_op_is_a_registered_undoable_character_edit_with_a_closed_documented_schema() {
    for name in OPS {
        let registration = find(name).unwrap_or_else(|| panic!("{name} isn't registered"));
        let meta = (registration.meta)();
        assert_eq!((meta.kind, meta.scope, meta.undo), (OpKind::Edit, Scope::EditCharacter, Undo::Auto), "{name}");

        let schema = serde_json::to_value((registration.schema)()).unwrap();
        assert_eq!(schema["additionalProperties"], json!(false), "{name}: a typo'd argument must be refused");
        assert!(!schema.to_string().contains("$ref"), "{name}: MCP schemas are inlined");
        assert!(schema["description"].as_str().unwrap().len() > 100, "{name}: the struct docs are the agent's manual");
        for (field, property) in schema["properties"].as_object().unwrap() {
            assert!(property["description"].is_string(), "{name}.{field} has no description");
        }
    }
    let required = |name: &str| {
        let schema = serde_json::to_value((find(name).unwrap().schema)()).unwrap();
        let mut required: Vec<String> =
            schema["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect();
        required.sort();
        required
    };
    assert_eq!(required("character.create"), ["id", "name", "template"]);
    assert_eq!(required("part.add"), ["character", "id", "kind"]);
    assert_eq!(required("part.remove"), ["character", "part"]);
    assert_eq!(required("part.set_enabled"), ["character", "enabled", "part"]);
    assert_eq!(required("part.reorder"), ["character", "part"]);
    assert_eq!(required("part.duplicate"), ["character", "new_id", "part"]);
}

#[test]
fn a_misspelt_argument_or_a_bad_id_is_refused_before_anything_is_planned() {
    let doc = project();
    let typo = refused(&doc, "part.remove", json!({"character": "mock", "part": "vest", "dry_run": true}));
    assert_eq!(typo.code, ErrorCode::Invalid);
    assert!(typo.message.contains("unknown field `dry_run`"), "{}", typo.message);
    assert!(typo.message.contains("expected `character` or `part`"), "it lists the valid arguments: {}", typo.message);

    let bad_id = refused(&doc, "part.add", json!({"character": "mock", "kind": "mock-garment", "id": "Hair B"}));
    assert_eq!(bad_id.code, ErrorCode::Invalid);
    assert!(bad_id.message.contains("isn't allowed at position 0"), "{}", bad_id.message);
}

// ------------------------------------------------------------------------------------------------ character.create

#[test]
fn create_round_trips_and_builds_for_every_registered_template() {
    let mut built_one = false;
    for template in ["soft-sd", "mini", "standard", "tall"] {
        let Some(registered) = registry::find(template) else {
            eprintln!("skipping template `{template}`: it isn't registered yet");
            continue;
        };
        let mut doc = MemDoc::new();
        let planned = round_trip(
            &mut doc,
            "character.create",
            json!({"id": "mira", "name": " Mira ", "template": template, "seed": 7}),
        );
        assert_eq!(planned.patch.changes.len(), 2, "the first character also creates the `characters` table");
        // In build order: the garments by their layers, the tights under the shirt, the skirt over it and the boots
        // over everything.
        let dressed = ["body", "head", "hair", "legwear", "shirt", "skirt", "boots"];
        assert_eq!(names(&planned.output.as_ref().unwrap()["parts"]), dressed);

        let def = definition(&doc, "mira");
        assert_eq!((def.name.as_str(), def.template.as_str(), def.seed), ("Mira", template, 7));
        assert_eq!(built_parts(&doc, "mira"), dressed);

        // The body and head take the template's own numbers, the body with the starter's five-finger hands; the hair
        // is the starter's style; the outfit takes its kinds' defaults.
        let defaults = registered.defaults();
        let stored = |part: &str| def.parts[&part.parse().unwrap()].params.clone();
        let body = omuzaki_char_api::BodyParams { hands: omuzaki_char_api::HandStyle::FiveFinger, ..defaults.body };
        assert_eq!(serde_json::from_value::<omuzaki_char_api::BodyParams>(stored("body")).unwrap(), body);
        assert_eq!(serde_json::from_value::<omuzaki_char_api::HeadParams>(stored("head")).unwrap(), defaults.head);
        let hair = serde_json::from_value::<HairParams>(stored("hair")).unwrap();
        assert!(hair.groups.len() == 4 && hair != HairParams::default(), "the starter's bob with framing locks");
        for part in ["legwear", "shirt", "skirt", "boots"] {
            assert_eq!(stored(part), (find_kind(part).unwrap().defaults)(), "{part}");
        }
        assert!(!doc.root().to_string().contains("null"), "project files can't hold null: {}", doc.root());
        built_one = true;
    }
    assert!(built_one, "soft-sd is registered, so at least one template is tested");
}

#[test]
fn every_part_kinds_defaults_are_storable_and_valid() {
    omuzaki_char::link();
    let mut seen = 0;
    for kind in omuzaki_char_api::kinds().filter(|k| !(k.meta)().id.starts_with("mock-")) {
        let id = (kind.meta)().id;
        let defaults = (kind.defaults)();
        // `part.add` and `character.create` store a kind's defaults as they are, and a project file has no null.
        assert!(!defaults.to_string().contains("null"), "`{id}`'s defaults hold a null: {defaults}");
        assert_eq!((kind.check)(&defaults).as_ref().ok(), Some(&defaults), "`{id}`'s defaults pass its own check");
        seen += 1;
    }
    assert!(seen >= 10, "the kit's kinds are linked ({seen})");
}

#[test]
fn create_next_to_an_existing_character_adds_one_change_and_empty_means_no_parts() {
    let mut doc = project();
    let planned = round_trip(
        &mut doc,
        "character.create",
        json!({"id": "bare", "name": "Bare", "template": "soft-sd", "parts": "empty"}),
    );
    assert_eq!(planned.patch.changes.len(), 1, "the `characters` table already exists");
    let def = definition(&doc, "bare");
    assert!(def.parts.is_empty());
    assert_eq!(def.palette.0.len(), 3, "the skin slots are there for the parts added later");
    assert_eq!(at(&doc, "characters/mock/name"), Some(json!("Mock")), "the other character is untouched");
}

#[test]
fn create_refuses_bad_input_naming_the_field_and_the_fix() {
    let doc = project();
    let make = |extra: Value| {
        let mut args = json!({"id": "new", "name": "New", "template": "soft-sd"});
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        args
    };

    let taken = refused(&doc, "character.create", make(json!({"id": "mock"})));
    assert_error(&taken, ErrorCode::Invalid, "id", &["`mock` already exists"], &["choose another id"]);

    let template = refused(&doc, "character.create", make(json!({"template": "gigantic"})));
    assert_error(&template, ErrorCode::Invalid, "template", &["no body template `gigantic`"], &["`soft-sd`"]);

    let name = refused(&doc, "character.create", make(json!({"name": "   "})));
    assert_error(&name, ErrorCode::Invalid, "name", &["empty"], &["display name"]);

    let parts = refused(&doc, "character.create", make(json!({"parts": "all"})));
    assert_eq!(parts.code, ErrorCode::Invalid);
    assert!(parts.message.contains("unknown variant `all`, expected `defaults` or `empty`"), "{}", parts.message);
}

// ------------------------------------------------------------------------------------------------ character.rename

#[test]
fn rename_changes_only_the_display_name() {
    let mut doc = project();
    let planned = round_trip(&mut doc, "character.rename", json!({"character": "mock", "name": "  Mock Two "}));
    assert_eq!(planned.patch.changes.len(), 1);
    assert_eq!(at(&doc, "characters/mock/name"), Some(json!("Mock Two")));

    let same = plan(&doc, "character.rename", json!({"character": "mock", "name": "Mock Two"})).unwrap();
    assert!(same.patch.is_empty(), "renaming to the current name changes nothing");
    assert_eq!(same.output.unwrap()["changed"], json!(false));

    let empty = refused(&doc, "character.rename", json!({"character": "mock", "name": ""}));
    assert_error(&empty, ErrorCode::Invalid, "name", &["empty"], &["display name"]);
    let missing = refused(&doc, "character.rename", json!({"character": "ghost", "name": "X"}));
    assert_error(&missing, ErrorCode::NotFound, "character", &["no character `ghost`"], &["`mock`"]);
    let none = refused(&MemDoc::new(), "character.rename", json!({"character": "ghost", "name": "X"}));
    assert_error(&none, ErrorCode::NotFound, "character", &["no character `ghost`"], &["character.create"]);
}

// ------------------------------------------------------------------------------------------------ part.add

#[test]
fn add_puts_a_normalised_part_in_at_the_kinds_usual_layer_and_the_character_still_builds() {
    let mut doc = project();
    let planned = round_trip(
        &mut doc,
        "part.add",
        json!({"character": "mock", "kind": "mock-garment", "id": "jacket", "params": {"ease": 0.01}}),
    );
    let stored = at(&doc, "characters/mock/parts/jacket").unwrap();
    assert_eq!(stored["kind"], "mock-garment");
    assert_eq!((stored["layer"].clone(), stored["enabled"].clone()), (json!(20), json!(true)), "garments start at 20");
    assert_eq!(stored["params"]["ease"], json!(0.01));
    assert!(stored["params"]["colour"].is_string(), "the parameters you leave out are filled in: {stored}");
    assert!(stored.get("parent").is_none() && stored.get("attach").is_none(), "no nulls in the project");
    assert_eq!(
        names(&planned.output.as_ref().unwrap()["build_order"]),
        ["body", "head", "hair", "jacket", "shirt", "vest"]
    );
    assert_eq!(built_parts(&doc, "mock"), ["body", "head", "hair", "jacket", "shirt", "vest"]);

    let outer = round_trip(
        &mut doc,
        "part.add",
        json!({"character": "mock", "kind": "mock-garment", "id": "coat", "layer": 40, "parent": "vest"}),
    );
    assert_eq!(at(&doc, "characters/mock/parts/coat/parent"), Some(json!("vest")));
    assert_eq!(outer.output.unwrap()["layer"], json!(40));
    assert_eq!(built_parts(&doc, "mock").last().map(String::as_str), Some("coat"));
}

#[test]
fn add_fixes_the_part_where_attach_says_and_refuses_an_anchor_on_a_missing_part() {
    let mut doc = project();
    let attach = json!({"to": "anchor", "part": "shirt", "anchor": "collar_ring"});
    round_trip(
        &mut doc,
        "part.add",
        json!({"character": "mock", "kind": "mock-garment", "id": "pin", "attach": attach}),
    );
    let stored = at(&doc, "characters/mock/parts/pin/attach").unwrap();
    assert_eq!((stored["to"].as_str(), stored["part"].as_str()), (Some("anchor"), Some("shirt")), "{stored}");

    let attach = json!({"to": "anchor", "part": "nope", "anchor": "collar_ring"});
    let missing =
        refused(&doc, "part.add", json!({"character": "mock", "kind": "mock-garment", "id": "pin2", "attach": attach}));
    assert_error(&missing, ErrorCode::NotFound, "attach/part", &["no part `nope`"], &["`shirt`"]);
}

#[test]
fn add_without_a_parameters_object_uses_the_kinds_defaults() {
    let mut doc = project();
    round_trip(&mut doc, "part.add", json!({"character": "mock", "kind": "mock-garment", "id": "plain"}));
    let kind = find_kind("mock-garment").unwrap();
    let defaults = (kind.check)(&(kind.defaults)()).unwrap();
    assert_eq!(at(&doc, "characters/mock/parts/plain/params"), Some(defaults));
}

#[test]
fn add_refuses_bad_input_naming_the_field_and_the_fix() {
    let doc = project();
    let add = |extra: Value| {
        let mut args = json!({"character": "mock", "kind": "mock-garment", "id": "jacket"});
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        args
    };

    let kind = refused(&doc, "part.add", add(json!({"kind": "mock-hare"})));
    assert_error(&kind, ErrorCode::Invalid, "kind", &["no part kind `mock-hare`"], &["`mock-hair`", "`mock-garment`"]);

    // The hint's kind list is `part.list_kinds`' list, in the same order.
    let listed: Vec<String> = plan(&doc, "part.list_kinds", json!({})).unwrap().output.unwrap()["kinds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| format!("`{}`", k["id"].as_str().unwrap()))
        .collect();
    let hint = kind.hint.as_deref().unwrap();
    assert!(hint.ends_with(&listed.join(", ")), "{hint} should list {listed:?}");

    let taken = refused(&doc, "part.add", add(json!({"id": "shirt"})));
    assert_error(&taken, ErrorCode::Invalid, "id", &["`shirt` is already a part", "mock-garment"], &["part.duplicate"]);

    let parent = refused(&doc, "part.add", add(json!({"parent": "nope"})));
    assert_error(&parent, ErrorCode::NotFound, "parent", &["no part `nope`"], &["`body`", "`vest`"]);

    let range = refused(&doc, "part.add", add(json!({"params": {"ease": 5.0}})));
    assert_error(&range, ErrorCode::Invalid, "params/ease", &["ease"], &["part.describe"]);

    let typo = refused(&doc, "part.add", add(json!({"params": {"colur": "#ffffff"}})));
    assert_error(&typo, ErrorCode::Invalid, "params", &["colur"], &["defaults"]);

    let missing = refused(&doc, "part.add", add(json!({"character": "ghost"})));
    assert_error(&missing, ErrorCode::NotFound, "character", &["no character `ghost`"], &["`mock`"]);
}

#[test]
fn add_refuses_a_garment_with_no_body_and_names_the_kind_to_add_first() {
    let doc = project_with(|node| {
        node["parts"].as_object_mut().unwrap().retain(|id, _| id == "head");
    });
    let e = refused(&doc, "part.add", json!({"character": "mock", "kind": "mock-garment", "id": "jacket"}));
    assert_error(
        &e,
        ErrorCode::Invalid,
        "kind",
        &["adding `jacket` would leave the character unbuildable", "`jacket` needs a part of kind `mock-body`"],
        &["add or enable a part of kind `mock-body` first"],
    );
    assert!(!e.hint.as_deref().unwrap().contains("remove"), "the part being added can't be removed: {e:?}");
    // Adding the body first makes the same call work.
    let mut doc = doc;
    apply(&mut doc, "part.add", json!({"character": "mock", "kind": "mock-body", "id": "body"}));
    apply(&mut doc, "part.add", json!({"character": "mock", "kind": "mock-garment", "id": "jacket"}));
    assert_eq!(built_parts(&doc, "mock"), ["body", "head", "jacket"]);
}

#[test]
fn add_to_a_character_stored_without_a_parts_table_creates_it_and_undo_removes_it() {
    let mut doc = project_with(|node| {
        node.as_object_mut().unwrap().remove("parts");
    });
    let planned = round_trip(&mut doc, "part.add", json!({"character": "mock", "kind": "mock-body", "id": "body"}));
    assert_eq!(planned.patch.changes.len(), 2);
}

// ------------------------------------------------------------------------------------------------ part.remove

#[test]
fn remove_takes_the_part_and_its_weight_brushes_and_undo_brings_both_back() {
    let brush: Value = serde_json::from_str(BRUSH).unwrap();
    let mut doc = project_with(|node| node["weight_brushes"] = json!({ "vest": [brush] }));
    let planned = round_trip(&mut doc, "part.remove", json!({"character": "mock", "part": "vest"}));
    assert_eq!(planned.patch.changes.len(), 2, "the part and its brushes");
    assert_eq!(at(&doc, "characters/mock/parts/vest"), None);
    assert_eq!(at(&doc, "characters/mock/weight_brushes/vest"), None);
    assert_eq!(built_parts(&doc, "mock"), ["body", "head", "hair", "shirt"], "no dangling brush error either");
    assert_eq!(names(&planned.output.as_ref().unwrap()["build_order"]), ["body", "head", "hair", "shirt"]);
}

#[test]
fn remove_refuses_while_other_parts_need_the_kind_and_names_them() {
    let doc = project();
    let e = refused(&doc, "part.remove", json!({"character": "mock", "part": "body"}));
    assert_error(
        &e,
        ErrorCode::Invalid,
        "part",
        &["removing `body` would leave the character unbuildable", "`shirt`, `vest` need a part of kind `mock-body`"],
        &["add or enable a part of kind `mock-body`", "`shirt`, `vest`"],
    );

    // A second body keeps them buildable, so the first can go.
    let mut doc = doc;
    apply(&mut doc, "part.add", json!({"character": "mock", "kind": "mock-body", "id": "body-b"}));
    round_trip(&mut doc, "part.remove", json!({"character": "mock", "part": "body"}));
}

#[test]
fn remove_refuses_a_parent_even_if_its_children_are_disabled() {
    let doc = project_with(|node| {
        node["parts"]["hood"] = json!({"kind": "mock-garment", "parent": "shirt", "layer": 25, "enabled": false});
    });
    let e = refused(&doc, "part.remove", json!({"character": "mock", "part": "shirt"}));
    assert_error(
        &e,
        ErrorCode::Invalid,
        "part",
        &["can't remove `shirt`", "parent of `hood`"],
        &["remove `hood` first", "part.reorder"],
    );
}

#[test]
fn remove_refuses_a_part_others_are_attached_to_or_variants_override() {
    let doc = project_with(|node| {
        node["parts"]["pin"] =
            json!({"kind": "mock-garment", "layer": 50, "attach": {"to": "anchor", "part": "head", "anchor": "top"}});
        node["variants"] = serde_json::from_str(VARIANTS_JSON).unwrap();
    });
    let attached = refused(&doc, "part.remove", json!({"character": "mock", "part": "head"}));
    assert_error(&attached, ErrorCode::Invalid, "part", &["`pin` are fixed to its anchors"], &["remove `pin` first"]);

    let varied = refused(&doc, "part.remove", json!({"character": "mock", "part": "hair"}));
    assert_error(
        &varied,
        ErrorCode::Invalid,
        "part",
        &["variants override its settings", "close-up: parts/hair/params/groups/b-bangs"],
        &["variant.clear_override"],
    );

    // The variants of other parts don't matter: the pin, with nothing attached to it, can go.
    let mut doc = doc;
    round_trip(&mut doc, "part.remove", json!({"character": "mock", "part": "pin"}));
}

#[test]
fn remove_of_an_unknown_part_lists_the_parts() {
    let e = refused(&project(), "part.remove", json!({"character": "mock", "part": "tail"}));
    assert_error(&e, ErrorCode::NotFound, "part", &["no part `tail`"], &["`body`", "`vest`"]);
}

// ------------------------------------------------------------------------------------------------ part.set_enabled

#[test]
fn set_enabled_round_trips_even_when_the_file_leaves_the_flag_out() {
    let mut doc = project();
    assert_eq!(at(&doc, "characters/mock/parts/vest/enabled"), None, "the fixture relies on the default");
    let planned =
        round_trip(&mut doc, "part.set_enabled", json!({"character": "mock", "part": "vest", "enabled": false}));
    assert_eq!(at(&doc, "characters/mock/parts/vest/enabled"), Some(json!(false)));
    assert_eq!(built_parts(&doc, "mock"), ["body", "head", "hair", "shirt"]);
    assert_eq!(planned.output.unwrap()["changed"], json!(true));

    let on = round_trip(&mut doc, "part.set_enabled", json!({"character": "mock", "part": "vest", "enabled": true}));
    assert_eq!(on.patch.changes[0].before, Some(json!(false)));
}

#[test]
fn set_enabled_to_the_current_state_changes_nothing() {
    let doc = project();
    let planned =
        plan(&doc, "part.set_enabled", json!({"character": "mock", "part": "vest", "enabled": true})).unwrap();
    assert!(planned.patch.is_empty());
    assert_eq!(planned.output.unwrap()["changed"], json!(false));
}

#[test]
fn set_enabled_refuses_to_disable_what_others_need_and_says_who() {
    let e = refused(&project(), "part.set_enabled", json!({"character": "mock", "part": "body", "enabled": false}));
    assert_error(
        &e,
        ErrorCode::Invalid,
        "enabled",
        &["disabling `body` would leave the character unbuildable", "`shirt`, `vest` need a part of kind `mock-body`"],
        &["enable a part of kind `mock-body`"],
    );
}

#[test]
fn disabling_a_parent_skips_its_children_and_enabling_a_child_under_it_explains_why_it_is_idle() {
    let mut doc = project_with(|node| {
        node["parts"]["hood"] = json!({"kind": "mock-garment", "parent": "shirt", "layer": 25});
    });
    round_trip(&mut doc, "part.set_enabled", json!({"character": "mock", "part": "shirt", "enabled": false}));
    apply(&mut doc, "part.set_enabled", json!({"character": "mock", "part": "shirt", "enabled": false}));
    assert_eq!(built_parts(&doc, "mock"), ["body", "head", "hair", "vest"], "the hood goes with its parent");

    apply(&mut doc, "part.set_enabled", json!({"character": "mock", "part": "hood", "enabled": false}));
    let again = plan(&doc, "part.set_enabled", json!({"character": "mock", "part": "hood", "enabled": true})).unwrap();
    let note = again.output.unwrap()["note"].as_str().unwrap().to_owned();
    assert!(note.contains("`hood` is enabled but isn't built because its ancestor `shirt` is disabled"), "{note}");
}

// ------------------------------------------------------------------------------------------------ part.reorder

#[test]
fn reorder_changes_the_layer_and_reports_the_new_build_order() {
    let mut doc = project();
    let planned = round_trip(&mut doc, "part.reorder", json!({"character": "mock", "part": "shirt", "layer": 35}));
    assert_eq!(at(&doc, "characters/mock/parts/shirt/layer"), Some(json!(35)));
    assert_eq!(names(&planned.output.as_ref().unwrap()["build_order"]), ["body", "head", "hair", "vest", "shirt"]);
    assert_eq!(built_parts(&doc, "mock"), ["body", "head", "hair", "vest", "shirt"]);

    let same = plan(&doc, "part.reorder", json!({"character": "mock", "part": "body", "layer": 0})).unwrap();
    assert!(same.patch.is_empty(), "the body's layer is already 0 (by default, not by the file)");
}

#[test]
fn reorder_sets_and_clears_a_parent_and_undo_of_both_restores_the_original() {
    let mut doc = project();
    let original = doc.root().clone();
    let moved = round_trip(&mut doc, "part.reorder", json!({"character": "mock", "part": "hair", "parent": "head"}));
    assert_eq!(at(&doc, "characters/mock/parts/hair/parent"), Some(json!("head")));
    assert_eq!(moved.output.unwrap()["parent"], json!("head"));

    let freed = round_trip(&mut doc, "part.reorder", json!({"character": "mock", "part": "hair", "detach": true}));
    assert_eq!(at(&doc, "characters/mock/parts/hair/parent"), None, "detaching removes the key, it doesn't store null");
    assert_eq!(doc.root(), &original, "set then clear leaves the file as it was");
    assert!(freed.output.unwrap().get("parent").is_none());
}

#[test]
fn reorder_refuses_contradictions_naming_the_parts() {
    let doc = project();
    let call = |extra: Value| {
        let mut args = json!({"character": "mock", "part": "shirt"});
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        args
    };

    let nothing = refused(&doc, "part.reorder", call(json!({})));
    assert_error(&nothing, ErrorCode::Invalid, "layer", &["nothing to change"], &["`layer`", "`parent`", "detach"]);
    let both = refused(&doc, "part.reorder", call(json!({"parent": "vest", "detach": true})));
    assert_error(&both, ErrorCode::Invalid, "detach", &["contradict"], &["not both"]);
    let missing = refused(&doc, "part.reorder", call(json!({"parent": "cape"})));
    assert_error(&missing, ErrorCode::NotFound, "parent", &["no part `cape`"], &["`body`"]);
    let itself = refused(&doc, "part.reorder", call(json!({"parent": "shirt"})));
    assert_error(&itself, ErrorCode::Invalid, "parent", &["`shirt` can't be its own parent"], &["detach"]);

    // The vest (layer 30) can't be the shirt's parent: it would have to build before a part that builds before it.
    let wait = refused(&doc, "part.reorder", call(json!({"parent": "vest"})));
    assert_error(
        &wait,
        ErrorCode::Invalid,
        "parent",
        &["moving `shirt` would leave", "wait on each other in a loop", "`shirt`", "`vest`"],
        &["layer or the parent"],
    );

    // Layers contradicting the kind rule: the garments need the body, so the body can't sit above them.
    let layers = refused(&doc, "part.reorder", json!({"character": "mock", "part": "body", "layer": 25}));
    assert_error(
        &layers,
        ErrorCode::Invalid,
        "layer",
        &["wait on each other in a loop", "`body`", "`shirt`"],
        &["layer or the parent"],
    );
}

#[test]
fn reorder_refuses_a_parent_that_is_below_the_part() {
    let mut doc = project();
    apply(&mut doc, "part.reorder", json!({"character": "mock", "part": "hair", "parent": "head"}));
    let e = refused(&doc, "part.reorder", json!({"character": "mock", "part": "head", "parent": "hair"}));
    assert_error(&e, ErrorCode::Invalid, "parent", &["`hair` is below `head` (hair -> head)"], &["isn't below"]);
}

// ------------------------------------------------------------------------------------------------ part.duplicate

#[test]
fn duplicate_copies_the_part_disabled_with_its_parameters_and_brushes() {
    let brush: Value = serde_json::from_str(BRUSH).unwrap();
    let mut doc = project_with(|node| node["weight_brushes"] = json!({ "hair": [brush] }));
    let planned =
        round_trip(&mut doc, "part.duplicate", json!({"character": "mock", "part": "hair", "new_id": "hair-b"}));
    let (original, copy) =
        (at(&doc, "characters/mock/parts/hair").unwrap(), at(&doc, "characters/mock/parts/hair-b").unwrap());
    assert_eq!(copy["enabled"], json!(false));
    assert_eq!(copy["kind"], original["kind"]);
    assert_eq!(copy["params"], original["params"]);
    assert_eq!(at(&doc, "characters/mock/weight_brushes/hair-b"), at(&doc, "characters/mock/weight_brushes/hair"));
    assert_eq!(built_parts(&doc, "mock"), ["body", "head", "hair", "shirt", "vest"], "the copy isn't built");
    let note = planned.output.unwrap()["note"].as_str().unwrap().to_owned();
    assert!(note.contains("part.set_enabled"), "{note}");
}

#[test]
fn duplicating_twice_then_enabling_one_leaves_exactly_one_of_three_hair_parts_enabled() {
    let mut doc = project();
    apply(&mut doc, "part.duplicate", json!({"character": "mock", "part": "hair", "new_id": "hair-b"}));
    apply(&mut doc, "part.duplicate", json!({"character": "mock", "part": "hair", "new_id": "hair-c"}));
    // Switch to version B: enable it, then disable the original.
    apply(&mut doc, "part.set_enabled", json!({"character": "mock", "part": "hair-b", "enabled": true}));
    apply(&mut doc, "part.set_enabled", json!({"character": "mock", "part": "hair", "enabled": false}));

    let def = definition(&doc, "mock");
    let enabled: Vec<_> = def
        .parts
        .iter()
        .filter(|(_, p)| p.kind.as_str() == "mock-hair" && p.enabled)
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(enabled, ["hair-b"]);
    assert_eq!(def.parts.values().filter(|p| p.kind.as_str() == "mock-hair").count(), 3);
    assert_eq!(built_parts(&doc, "mock"), ["body", "head", "hair-b", "shirt", "vest"]);
}

#[test]
fn duplicate_refuses_a_taken_id_and_an_unknown_part() {
    let doc = project();
    let taken = refused(&doc, "part.duplicate", json!({"character": "mock", "part": "hair", "new_id": "vest"}));
    assert_error(
        &taken,
        ErrorCode::Invalid,
        "new_id",
        &["`vest` is already a part", "mock-garment"],
        &["id no part uses"],
    );
    let unknown = refused(&doc, "part.duplicate", json!({"character": "mock", "part": "wig", "new_id": "wig-b"}));
    assert_error(&unknown, ErrorCode::NotFound, "part", &["no part `wig`"], &["`hair`"]);
}

// ------------------------------------------------------------------------------------------------ which argument is blamed

fn garment(extra: &Value) -> Value {
    let mut args = json!({"character": "mock", "kind": "mock-garment", "id": "hood"});
    args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    args
}

#[test]
fn add_blames_the_argument_that_causes_a_loop() {
    let doc = project();
    // The parent alone: a garment at its default layer 20 can't have the vest (layer 30) as its parent.
    let parent = refused(&doc, "part.add", garment(&json!({"parent": "vest"})));
    assert_error(&parent, ErrorCode::Invalid, "parent", &["`hood`", "`vest`", "loop"], &["layer or the parent"]);

    // The layer alone: below the body, a garment would have to build before the body it is built over.
    let layer = refused(&doc, "part.add", garment(&json!({"layer": -5})));
    assert_error(&layer, ErrorCode::Invalid, "layer", &["`hood`", "loop"], &["layer or the parent"]);

    // Only the two together: layer 5 is fine without a parent, a parent is fine at the default layer.
    let both = refused(&doc, "part.add", garment(&json!({"parent": "shirt", "layer": 5})));
    assert_error(&both, ErrorCode::Invalid, "layer", &["`hood`", "`shirt`", "loop"], &["layer or the parent"]);

    // Neither given: the kind's default layer against a body that sits above it.
    let high_body = project_with(|node| {
        node["parts"].as_object_mut().unwrap().retain(|id, _| id == "body" || id == "head");
        node["parts"]["body"]["layer"] = json!(25);
    });
    let kind = refused(&high_body, "part.add", garment(&json!({})));
    assert_error(&kind, ErrorCode::Invalid, "kind", &["adding `hood`", "`body`", "loop"], &["layer or the parent"]);
}

#[test]
fn reorder_blames_the_argument_that_causes_a_loop() {
    let doc = project();
    let call = |extra: Value| {
        let mut args = json!({"character": "mock", "part": "shirt"});
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        args
    };
    // The parent is the cause although a layer is given too: layer 21 alone is harmless.
    let parent = refused(&doc, "part.reorder", call(json!({"layer": 21, "parent": "vest"})));
    assert_error(&parent, ErrorCode::Invalid, "parent", &["`shirt`", "`vest`", "loop"], &["layer or the parent"]);

    // The layer is the cause although a parent is given too: head as the body's parent is harmless.
    let layer =
        refused(&doc, "part.reorder", json!({"character": "mock", "part": "body", "layer": 25, "parent": "head"}));
    assert_error(&layer, ErrorCode::Invalid, "layer", &["`body`", "loop"], &["layer or the parent"]);

    // Detaching a part from a disabled parent makes it build, and it has no body to build over.
    let idle = project_with(|node| {
        node["parts"] = json!({
            "head": {"kind": "mock-head"},
            "cape": {"kind": "mock-head", "enabled": false},
            "shirt": {"kind": "mock-garment", "layer": 20, "parent": "cape"},
        });
    });
    let detach = refused(&idle, "part.reorder", call(json!({"detach": true})));
    assert_error(
        &detach,
        ErrorCode::Invalid,
        "detach",
        &["`shirt` needs a part of kind `mock-body`"],
        &["add or enable"],
    );
}

// ------------------------------------------------------------------------------------------------ repairing a broken character

/// A character whose body was deleted by hand: the shirt and the vest have nothing to be built over.
fn without_body() -> MemDoc {
    project_with(|node| {
        node["parts"].as_object_mut().unwrap().remove("body");
    })
}

#[test]
fn an_edit_is_refused_only_for_the_problems_it_introduces_so_a_broken_character_can_be_repaired() {
    // The shirt and the vest are orphaned already, so removing, disabling or moving the vest breaks nothing new.
    for (op, args) in [
        ("part.remove", json!({"character": "mock", "part": "vest"})),
        ("part.set_enabled", json!({"character": "mock", "part": "vest", "enabled": false})),
        ("part.reorder", json!({"character": "mock", "part": "vest", "layer": 31})),
    ] {
        round_trip(&mut without_body(), op, args);
    }
    let mut doc = without_body();

    // A new garment is blamed alone: the message doesn't pin the old orphans on the edit.
    let e = refused(&doc, "part.add", json!({"character": "mock", "kind": "mock-garment", "id": "jacket"}));
    assert_error(
        &e,
        ErrorCode::Invalid,
        "kind",
        &["adding `jacket`", "`jacket` needs a part of kind `mock-body`"],
        &["add or enable"],
    );
    assert!(!e.message.contains("shirt") && !e.message.contains("vest"), "only the new part is named: {}", e.message);
    assert!(!e.hint.as_deref().unwrap().contains("remove"), "{e:?}");

    // Adding the missing body repairs the character, and the shirt builds again.
    apply(&mut doc, "part.add", json!({"character": "mock", "kind": "mock-body", "id": "body"}));
    assert_eq!(built_parts(&doc, "mock"), ["body", "head", "hair", "shirt", "vest"]);
}

#[test]
fn a_loop_that_was_already_there_does_not_block_an_unrelated_edit_and_can_be_repaired() {
    // The body sits above the garments that need it: body, shirt and vest are in a loop already.
    let mut doc = project_with(|node| node["parts"]["body"]["layer"] = json!(25));
    round_trip(&mut doc, "part.set_enabled", json!({"character": "mock", "part": "hair", "enabled": false}));
    round_trip(&mut doc, "part.duplicate", json!({"character": "mock", "part": "hair", "new_id": "hair-b"}));
    // Moving the body back down repairs it.
    round_trip(&mut doc, "part.reorder", json!({"character": "mock", "part": "body", "layer": 0}));
}

#[test]
fn add_rejects_parameters_that_are_not_an_object_naming_the_field() {
    let doc = project();
    for (params, what) in [(json!([]), "a list"), (json!("ease"), "text"), (json!(0.01), "a number")] {
        let e = refused(&doc, "part.add", garment(&json!({"params": params})));
        assert_error(&e, ErrorCode::Invalid, "params", &["`params` must be an object", what], &["{\"ease\": 0.01}"]);
    }
}

// ------------------------------------------------------------------------------------------------ body.set_template

/// `body.set_template` on the kit's own character (#681), whose real body reads the template: switching between
/// templates of different heights reports the garments that sit on the body (the tights and the shirt every time), never
/// the body itself, and never the head or the hair, which keep the sizes stored in their own parameters; switching to
/// the template the character already has reports nothing and changes nothing. The starter's skirt paints a plaid, so
/// the op builds with painting left blank, as the other ops that read geometry do.
#[test]
fn set_template_on_the_starter_reports_the_garments_whose_fit_moved() {
    for (from, to) in [("soft-sd", "tall"), ("standard", "tall"), ("tall", "standard"), ("mini", "soft-sd")] {
        let mut doc = MemDoc::new();
        apply(&mut doc, "character.create", json!({"id": "mira", "name": "Mira", "template": from, "seed": 7}));
        let planned = plan(&doc, "body.set_template", json!({"character": "mira", "template": to})).unwrap();
        let output = planned.output.unwrap();
        let moved: Vec<&str> = output["fit_changed"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
        eprintln!("{from} -> {to}: {moved:?}");
        for part in ["legwear", "shirt"] {
            assert!(moved.contains(&part), "{from} -> {to}: {part} is not reported: {moved:?}");
        }
        for part in &moved {
            assert!(
                ["legwear", "shirt", "skirt", "boots"].contains(part),
                "{from} -> {to}: {part} reported: {moved:?}"
            );
        }
        let same = plan(&doc, "body.set_template", json!({"character": "mira", "template": from})).unwrap();
        assert!(same.patch.is_empty(), "{from}");
        assert_eq!(same.output.unwrap()["fit_changed"], json!([]), "{from}");
    }
}
