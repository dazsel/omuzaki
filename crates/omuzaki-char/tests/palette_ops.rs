//! The palette ops (`palette.set`, `palette.add_slot`, `palette.remove_slot`, `palette.rename_slot`, `palette.usage`)
//! and the colourway ops (`colourway.save`, `colourway.apply`, `colourway.delete`, `colourway.list`,
//! `colourway.preview`) against the in-memory document.
//!
//! The ops are planned through the shared harness in `common` (`round_trip` spells out the editing contract). The
//! character is the mock one (`omuzaki-char-mock`), whose parts are real part kinds. `dressed` stores the colour
//! references a character made in the editor carries, so a rename has parts and a face to rewrite.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{apply, assert_error, at, build, mock_node, plan, project, refused, round_trip};
use omuzaki_char_api::BuiltCharacter;
use omuzaki_char_mock::VARIANTS_JSON;
use omuzaki_doc_api::DocPath;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{ErrorCode, OpKind, Plan, Scope, Undo, find};
use serde_json::{Value, json};

const PALETTE_OPS: [&str; 5] =
    ["palette.set", "palette.add_slot", "palette.remove_slot", "palette.rename_slot", "palette.usage"];
const COLOURWAY_OPS: [&str; 5] =
    ["colourway.save", "colourway.apply", "colourway.delete", "colourway.list", "colourway.preview"];

/// The skin references the head and body store: the `skin` slots, as `palette.set_skin_tone` writes them. Stored,
/// not left to the parts' defaults, so the palette ops can see them.
fn skin_refs() -> Value {
    json!({ "base": "@skin", "shade": "@skin_shade", "line": "@skin_line", "rim": "#ffd9cf" })
}

/// The mock character's node with the colour references a character made in the editor stores: the head and body take
/// the `skin` slots, the hair's root takes `hair_root`, the cheeks take `blush`, and the vest takes `outfit`.
fn dressed_node() -> Value {
    let mut node = mock_node();
    node["palette"]["hair_root"] = json!("#c8b09a");
    node["palette"]["blush"] = json!("#e0707a");
    node["parts"]["head"]["params"] = json!({ "skin": skin_refs() });
    node["parts"]["body"]["params"] = json!({ "skin": skin_refs() });
    node["parts"]["hair"]["params"]["shading"] = json!({ "root": "@hair_root" });
    node["face"] = json!({ "cheeks": { "colour": "@blush" } });
    node
}

/// `dressed_node` as a project holding it as `mock`.
fn dressed() -> MemDoc {
    MemDoc::from_value(json!({ "characters": { "mock": dressed_node() } }))
}

/// What the face painter receives from a build: the face's projection and its profile with the colours resolved
/// through the build's own palette. A rename changes the slot names the stored profile holds, not the colours painted.
fn painted(built: &BuiltCharacter) -> String {
    let face =
        built.face.as_ref().map(|face| (&face.projection, face.profile.resolve_colours(&built.palette).unwrap()));
    format!("{face:?}")
}

/// Every part of a build a palette rename must leave alone: what the renderer receives. The character as built and
/// its palette are not compared, since they hold the names an edit changes.
#[track_caller]
fn assert_same_build(before: &BuiltCharacter, after: &BuiltCharacter) {
    assert_eq!(before.meshes, after.meshes, "the meshes must build the same");
    assert_eq!(before.materials, after.materials, "the materials must resolve to the same colours");
    assert_eq!(before.textures, after.textures, "the textures must be the same");
    assert_eq!(before.anchors, after.anchors, "the anchors must be the same");
    // The face binding keeps the profile's slot names, so compare what the painter receives: see `painted`.
    assert_eq!(painted(before), painted(after), "the face must paint the same");
    assert_eq!(format!("{:?}", before.rig), format!("{:?}", after.rig), "the rig must be the same");
}

/// The paths a patch touches, as text, so a test can say which part of the document an edit changed.
fn touched(plan: &Plan) -> Vec<String> {
    plan.patch.changes.iter().map(|c| c.path.to_string()).collect()
}

// ------------------------------------------------------------------------------------------------ the ops as an agent sees them

#[test]
fn every_palette_and_colourway_op_is_registered_with_its_kind_scope_and_undo() {
    for name in PALETTE_OPS.into_iter().chain(COLOURWAY_OPS) {
        let registration = find(name).unwrap_or_else(|| panic!("{name} isn't registered"));
        let meta = (registration.meta)();
        let (kind, scope, undo) = if meta.kind == OpKind::Query {
            (OpKind::Query, Scope::Read, Undo::None)
        } else {
            (OpKind::Edit, Scope::EditCharacter, Undo::Auto)
        };
        assert_eq!((meta.kind, meta.scope, meta.undo), (kind, scope, undo), "{name}");

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
    assert_eq!(required("palette.set"), ["character", "colour", "slot"]);
    assert_eq!(required("palette.add_slot"), ["character", "colour", "slot"]);
    assert_eq!(required("palette.remove_slot"), ["character", "slot"]);
    assert_eq!(required("palette.rename_slot"), ["character", "from", "to"]);
    assert_eq!(required("palette.usage"), ["character"]);
    assert_eq!(required("colourway.save"), ["character", "name"]);
    assert_eq!(required("colourway.apply"), ["character", "name"]);
}

#[test]
fn a_misspelt_argument_or_a_bad_slot_name_is_refused_before_anything_is_planned() {
    let doc = project();
    let typo = refused(&doc, "palette.set", json!({"character": "mock", "slot": "skin", "hex": "#ffffff"}));
    assert_eq!(typo.code, ErrorCode::Invalid);
    assert!(typo.message.contains("unknown field `hex`"), "{}", typo.message);

    let bad_name =
        refused(&doc, "palette.add_slot", json!({"character": "mock", "slot": "Hair Root", "colour": "#ffffff"}));
    assert_eq!(bad_name.code, ErrorCode::Invalid, "{bad_name:?}");
}

// ------------------------------------------------------------------------------------------------ palette.set

#[test]
fn set_recolours_an_existing_slot_and_touches_only_its_palette_entry() {
    let mut doc = project();
    let planned =
        round_trip(&mut doc, "palette.set", json!({"character": "mock", "slot": "skin", "colour": "#FF8800"}));
    assert_eq!(touched(&planned), ["characters/mock/palette/skin"]);
    assert_eq!(at(&doc, "characters/mock/palette/skin"), Some(json!("#ff8800")), "colours are stored canonically");
    assert_eq!(planned.output.unwrap()["changed"], json!(true));
}

#[test]
fn set_refuses_an_unknown_slot_and_says_to_add_it_and_refuses_a_colour_that_is_not_one() {
    let doc = project();
    let unknown = refused(&doc, "palette.set", json!({"character": "mock", "slot": "blush", "colour": "#ffffff"}));
    assert_error(
        &unknown,
        ErrorCode::NotFound,
        "slot",
        &["has no palette slot `blush`"],
        &["palette.add_slot", "skin"],
    );

    for bad in ["@skin", "fff", "red"] {
        let e = refused(&doc, "palette.set", json!({"character": "mock", "slot": "skin", "colour": bad}));
        let said = format!("`{bad}` isn't a colour");
        assert_error(&e, ErrorCode::Invalid, "colour", &[said.as_str(), "#rrggbb"], &["@slot_name"]);
    }
    let nobody = refused(&doc, "palette.set", json!({"character": "nobody", "slot": "skin", "colour": "#ffffff"}));
    assert_error(&nobody, ErrorCode::NotFound, "character", &["there is no character `nobody`"], &["mock"]);
}

#[test]
fn set_to_the_colour_a_slot_already_has_changes_nothing() {
    let doc = project();
    let same = plan(&doc, "palette.set", json!({"character": "mock", "slot": "skin", "colour": "#F9D3C5"})).unwrap();
    assert!(same.patch.is_empty(), "setting the current colour is a no-op");
    assert_eq!(same.output.unwrap()["changed"], json!(false));
}

// ------------------------------------------------------------------------------------------------ palette.add_slot

#[test]
fn add_slot_round_trips_and_creates_the_palette_table_when_the_character_has_none() {
    let mut doc = project();
    let planned =
        round_trip(&mut doc, "palette.add_slot", json!({"character": "mock", "slot": "blush", "colour": "#e0707a"}));
    assert_eq!(touched(&planned), ["characters/mock/palette/blush"]);
    assert_eq!(at(&doc, "characters/mock/palette/blush"), Some(json!("#e0707a")));

    let mut bare =
        MemDoc::from_value(json!({ "characters": { "mock": { "name": "Bare", "template": "soft-sd", "parts": {} } } }));
    let made =
        round_trip(&mut bare, "palette.add_slot", json!({"character": "mock", "slot": "skin", "colour": "#f9d3c5"}));
    assert_eq!(made.patch.changes.len(), 2, "the table and the slot, in one patch");
    assert_eq!(at(&bare, "characters/mock/palette"), Some(json!({"skin": "#f9d3c5"})));
}

#[test]
fn add_slot_refuses_a_slot_that_exists_and_names_its_colour() {
    let doc = project();
    let e = refused(&doc, "palette.add_slot", json!({"character": "mock", "slot": "skin", "colour": "#000000"}));
    assert_error(&e, ErrorCode::Invalid, "slot", &["already has palette slot `skin` (#f9d3c5)"], &["palette.set"]);
}

// ------------------------------------------------------------------------------------------------ palette.remove_slot

#[test]
fn remove_slot_round_trips_once_nothing_names_the_slot() {
    let mut doc = project();
    apply(&mut doc, "palette.add_slot", json!({"character": "mock", "slot": "spare", "colour": "#123456"}));
    let planned = round_trip(&mut doc, "palette.remove_slot", json!({"character": "mock", "slot": "spare"}));
    assert_eq!(touched(&planned), ["characters/mock/palette/spare"]);
    assert_eq!(at(&doc, "characters/mock/palette/spare"), None);
}

#[test]
fn remove_slot_refuses_a_used_slot_and_lists_every_place_that_names_it() {
    let doc = dressed();
    let in_head = refused(&doc, "palette.remove_slot", json!({"character": "mock", "slot": "skin"}));
    assert_error(
        &in_head,
        ErrorCode::Invalid,
        "slot",
        &[
            "can't remove palette slot `skin`",
            "characters/mock/parts/head/params/skin/base",
            "characters/mock/parts/body/params/skin/base",
        ],
        &["another slot", "rename_slot"],
    );

    let on_face = refused(&doc, "palette.remove_slot", json!({"character": "mock", "slot": "blush"}));
    assert_error(
        &on_face,
        ErrorCode::Invalid,
        "slot",
        &["used by `characters/mock/face/cheeks/colour`"],
        &["fixed colour"],
    );

    let on_vest = refused(&doc, "palette.remove_slot", json!({"character": "mock", "slot": "outfit"}));
    assert!(on_vest.message.contains("characters/mock/parts/vest/params/colour"), "{}", on_vest.message);

    let unknown = refused(&doc, "palette.remove_slot", json!({"character": "mock", "slot": "gone"}));
    assert_error(&unknown, ErrorCode::NotFound, "slot", &["has no palette slot `gone`"], &["skin"]);
}

// ------------------------------------------------------------------------------------------------ palette.rename_slot

#[test]
fn rename_builds_the_character_exactly_as_before_and_moves_every_reference() {
    let mut doc = dressed();
    let before = build(&doc, "mock");

    let planned =
        round_trip(&mut doc, "palette.rename_slot", json!({"character": "mock", "from": "skin", "to": "skin_base"}));
    let refs = planned.output.as_ref().unwrap()["references"].as_array().unwrap();
    assert_eq!(refs.len(), 2, "the head's and the body's base, the only references to `@skin`: {refs:?}");

    assert_eq!(at(&doc, "characters/mock/palette/skin"), None);
    assert_eq!(at(&doc, "characters/mock/palette/skin_base"), Some(json!("#f9d3c5")));
    assert_eq!(at(&doc, "characters/mock/parts/head/params/skin/base"), Some(json!("@skin_base")));
    assert_eq!(at(&doc, "characters/mock/parts/body/params/skin/base"), Some(json!("@skin_base")));
    assert_eq!(at(&doc, "characters/mock/parts/body/params/skin/line"), Some(json!("@skin_line")), "other slots stay");
    assert_eq!(at(&doc, "characters/mock/parts/head/params/skin/rim"), Some(json!("#ffd9cf")), "fixed colours stay");

    let after = build(&doc, "mock");
    assert_same_build(&before, &after);
}

#[test]
fn rename_rewrites_references_in_the_face_and_hair_and_still_builds_identically() {
    let mut doc = dressed();
    let before = build(&doc, "mock");
    round_trip(&mut doc, "palette.rename_slot", json!({"character": "mock", "from": "blush", "to": "cheek"}));
    round_trip(&mut doc, "palette.rename_slot", json!({"character": "mock", "from": "hair_root", "to": "hair_crown"}));
    assert_eq!(at(&doc, "characters/mock/face/cheeks/colour"), Some(json!("@cheek")));
    assert_eq!(at(&doc, "characters/mock/parts/hair/params/shading/root"), Some(json!("@hair_crown")));
    assert_same_build(&before, &build(&doc, "mock"));
}

#[test]
fn rename_moves_the_key_in_every_saved_colourway_so_the_colour_is_kept() {
    let mut doc = dressed();
    apply(&mut doc, "colourway.save", json!({"character": "mock", "name": "winter"}));
    round_trip(&mut doc, "palette.rename_slot", json!({"character": "mock", "from": "skin", "to": "skin_base"}));
    assert_eq!(at(&doc, "characters/mock/colourways/winter/skin"), None);
    assert_eq!(at(&doc, "characters/mock/colourways/winter/skin_base"), Some(json!("#f9d3c5")));
}

#[test]
fn rename_refuses_a_name_the_palette_has_and_names_the_slot_it_cannot_find() {
    let doc = dressed();
    let taken = refused(&doc, "palette.rename_slot", json!({"character": "mock", "from": "skin", "to": "outfit"}));
    assert_error(&taken, ErrorCode::Invalid, "to", &["already has palette slot `outfit`"], &["doesn't use yet"]);

    let same = refused(&doc, "palette.rename_slot", json!({"character": "mock", "from": "skin", "to": "skin"}));
    assert_eq!(same.field.as_deref(), Some("to"), "{same:?}");

    let missing = refused(&doc, "palette.rename_slot", json!({"character": "mock", "from": "nope", "to": "fresh"}));
    assert_error(&missing, ErrorCode::NotFound, "from", &["has no palette slot `nope`"], &["skin"]);
}

#[test]
fn rename_refuses_a_name_that_something_already_names_without_the_palette_defining_it() {
    let mut node = dressed_node();
    node["parts"]["shirt"]["params"]["colour"] = json!("@trim");
    let doc = MemDoc::from_value(json!({ "characters": { "mock": node } }));
    let e = refused(&doc, "palette.rename_slot", json!({"character": "mock", "from": "outfit", "to": "trim"}));
    assert_error(
        &e,
        ErrorCode::Invalid,
        "to",
        &["has no palette slot `trim`", "characters/mock/parts/shirt/params/colour"],
        &["palette.add_slot", "nothing refers to"],
    );
}

// ------------------------------------------------------------------------------------------------ the character's siblings

/// The keys a character node holds beside its definition. Each belongs to another file or op, so a palette edit leaves
/// them as stored; `colourways` is not in the list because the colourway ops change it, and [`touches_colourways`] checks
/// that only they do.
const SIBLINGS: [&str; 4] = ["variants", "rig", "look", "licence"];

/// The dressed character with the keys that sit beside its definition: the fixture's variants, a rig, a look and a
/// licence. None of them is a palette concern, so no palette or colourway edit may change them.
fn with_siblings() -> MemDoc {
    let mut node = dressed_node();
    node["variants"] = serde_json::from_str(VARIANTS_JSON).unwrap();
    node["rig"] = json!({ "extra_bones": [] });
    node["look"] = json!({ "lighting": "studio" });
    node["licence"] = json!({ "holder": "owner", "terms": "private" });
    MemDoc::from_value(json!({ "characters": { "mock": node } }))
}

/// Whether a change under `colourways` is one the op makes on purpose: saving, deleting or renaming a colourway.
fn touches_colourways(op: &str) -> bool {
    matches!(op, "colourway.save" | "colourway.delete" | "palette.rename_slot")
}

#[test]
fn palette_and_colourway_edits_leave_the_variants_rig_look_and_licence_as_stored() {
    let mut doc = with_siblings();
    let stored = |doc: &MemDoc| SIBLINGS.map(|key| at(doc, &format!("characters/mock/{key}")));
    let before = stored(&doc);
    let colourways = DocPath::parse("characters/mock/colourways").unwrap();

    let steps = [
        ("colourway.save", json!({"character": "mock", "name": "day"})),
        ("palette.set", json!({"character": "mock", "slot": "skin", "colour": "#202020"})),
        ("colourway.apply", json!({"character": "mock", "name": "day"})),
        ("palette.add_slot", json!({"character": "mock", "slot": "spare", "colour": "#123456"})),
        ("palette.rename_slot", json!({"character": "mock", "from": "skin", "to": "skin_base"})),
        ("palette.remove_slot", json!({"character": "mock", "slot": "spare"})),
        ("colourway.delete", json!({"character": "mock", "name": "day"})),
    ];
    for (op, args) in steps {
        let planned = round_trip(&mut doc, op, args);
        for change in &planned.patch.changes {
            for key in SIBLINGS {
                let sibling = DocPath::parse(&format!("characters/mock/{key}")).unwrap();
                assert!(!change.path.starts_with(&sibling), "{op} changed `{key}` at {}", change.path);
            }
            if change.path.starts_with(&colourways) {
                assert!(touches_colourways(op), "{op} changed a colourway at {}", change.path);
            }
        }
    }
    assert_eq!(stored(&doc), before, "the siblings are as they were stored after every edit");
}

// ------------------------------------------------------------------------------------------------ palette.usage

#[test]
fn usage_lists_each_slot_with_its_colour_and_where_it_is_used_and_flags_dangling_references() {
    // The vest now names `trim`, which the palette lacks, so `outfit` is used by nothing.
    let mut node = at(&dressed(), "characters/mock").unwrap();
    node["parts"]["vest"]["params"]["colour"] = json!("@trim");
    let doc = MemDoc::from_value(json!({ "characters": { "mock": node } }));

    let before = doc.root().clone();
    let usage = plan(&doc, "palette.usage", json!({"character": "mock"})).unwrap();
    assert!(usage.patch.is_empty(), "a query changes nothing");
    assert_eq!(doc.root(), &before);
    let out = usage.output.unwrap();

    assert_eq!(out["slots"]["skin"]["colour"], json!("#f9d3c5"));
    assert_eq!(
        out["slots"]["skin"]["used_by"],
        json!(["characters/mock/parts/body/params/skin/base", "characters/mock/parts/head/params/skin/base"])
    );
    assert_eq!(out["slots"]["skin_shade"]["used_by"].as_array().unwrap().len(), 2, "body and head shade");
    assert_eq!(out["slots"]["blush"]["used_by"], json!(["characters/mock/face/cheeks/colour"]));
    assert_eq!(out["slots"]["hair_root"]["used_by"], json!(["characters/mock/parts/hair/params/shading/root"]));
    assert_eq!(out["slots"]["outfit"]["used_by"], json!([]), "a slot nothing names is listed with no uses");
    assert_eq!(out["undefined"], json!({"trim": ["characters/mock/parts/vest/params/colour"]}));
}

// ------------------------------------------------------------------------------------------------ colourways

#[test]
fn save_copies_the_whole_palette_round_trips_and_refuses_an_existing_name() {
    let mut doc = dressed();
    let palette = at(&doc, "characters/mock/palette").unwrap();
    let planned = round_trip(&mut doc, "colourway.save", json!({"character": "mock", "name": "winter"}));
    assert_eq!(
        touched(&planned),
        ["characters/mock/colourways", "characters/mock/colourways/winter"],
        "the colourways table is created in the same patch as the save"
    );
    assert_eq!(at(&doc, "characters/mock/colourways/winter"), Some(palette));

    apply(&mut doc, "colourway.save", json!({"character": "mock", "name": "summer"}));
    let again = refused(&doc, "colourway.save", json!({"character": "mock", "name": "winter"}));
    assert_error(&again, ErrorCode::Invalid, "name", &["already has a colourway `winter`"], &["colourway.delete"]);
}

#[test]
fn apply_changes_only_palette_colours_round_trips_and_undo_restores_the_previous_ones() {
    let mut doc = dressed();
    apply(&mut doc, "colourway.save", json!({"character": "mock", "name": "day"}));
    apply(&mut doc, "palette.set", json!({"character": "mock", "slot": "skin", "colour": "#101010"}));
    apply(&mut doc, "palette.set", json!({"character": "mock", "slot": "hair_root", "colour": "#202020"}));

    let planned = round_trip(&mut doc, "colourway.apply", json!({"character": "mock", "name": "day"}));
    let palette = DocPath::parse("characters/mock/palette").unwrap();
    assert!(planned.patch.changes.iter().all(|c| c.path.starts_with(&palette)), "{:?}", touched(&planned));
    assert_eq!(planned.patch.changes.len(), 2, "one change per colour that differs");
    assert_eq!(at(&doc, "characters/mock/palette/skin"), Some(json!("#f9d3c5")));
    assert_eq!(at(&doc, "characters/mock/palette/hair_root"), Some(json!("#c8b09a")));
    let parts = at(&doc, "characters/mock/parts/head/params/skin/base");
    assert_eq!(parts, Some(json!("@skin")), "parts are not touched");
}

#[test]
fn apply_keeps_the_slots_a_colourway_does_not_name_and_does_not_add_the_ones_the_palette_lacks() {
    let mut node = mock_node();
    node["colourways"] = json!({ "partial": { "skin": "#010203", "gone": "#000000" } });
    let mut doc = MemDoc::from_value(json!({ "characters": { "mock": node } }));

    let planned = round_trip(&mut doc, "colourway.apply", json!({"character": "mock", "name": "partial"}));
    assert_eq!(touched(&planned), ["characters/mock/palette/skin"]);
    let out = planned.output.unwrap();
    assert_eq!(out["not_in_colourway"], json!(["outfit", "skin_line", "skin_shade"]), "the palette's slots, in order");
    assert_eq!(out["not_in_palette"], json!(["gone"]));
    assert_eq!(at(&doc, "characters/mock/palette/gone"), None, "the palette does not gain a slot from a colourway");
    assert_eq!(at(&doc, "characters/mock/palette/outfit"), Some(json!("#24392f")));
}

#[test]
fn apply_of_the_colours_already_there_changes_nothing() {
    let mut doc = project();
    apply(&mut doc, "colourway.save", json!({"character": "mock", "name": "now"}));
    let same = plan(&doc, "colourway.apply", json!({"character": "mock", "name": "now"})).unwrap();
    assert!(same.patch.is_empty());
    assert_eq!(same.output.unwrap()["changed"], json!([]));
}

#[test]
fn preview_reports_what_apply_would_change_without_changing_anything() {
    let mut doc = dressed();
    apply(&mut doc, "colourway.save", json!({"character": "mock", "name": "day"}));
    apply(&mut doc, "palette.set", json!({"character": "mock", "slot": "skin", "colour": "#101010"}));
    let before = doc.root().clone();

    let preview = plan(&doc, "colourway.preview", json!({"character": "mock", "name": "day"})).unwrap();
    assert!(preview.patch.is_empty());
    assert_eq!(doc.root(), &before, "a preview changes nothing");
    let applied = plan(&doc, "colourway.apply", json!({"character": "mock", "name": "day"})).unwrap();
    assert_eq!(preview.output, applied.output, "the preview is the apply's report");
    assert_eq!(preview.output.unwrap()["changed"][0]["slot"], json!("skin"));
}

#[test]
fn list_names_every_saved_colourway_and_delete_round_trips() {
    let mut doc = project();
    apply(&mut doc, "colourway.save", json!({"character": "mock", "name": "winter"}));
    apply(&mut doc, "colourway.save", json!({"character": "mock", "name": "summer"}));
    let listed = plan(&doc, "colourway.list", json!({"character": "mock"})).unwrap().output.unwrap();
    let names: Vec<&str> =
        listed["colourways"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["summer", "winter"]);

    let planned = round_trip(&mut doc, "colourway.delete", json!({"character": "mock", "name": "summer"}));
    assert_eq!(touched(&planned), ["characters/mock/colourways/summer"]);
    assert_eq!(at(&doc, "characters/mock/colourways/summer"), None);

    let missing = refused(&doc, "colourway.delete", json!({"character": "mock", "name": "autumn"}));
    assert_error(&missing, ErrorCode::NotFound, "name", &["no colourway `autumn`"], &["winter"]);
}

#[test]
fn a_colourway_that_does_not_read_as_a_palette_is_refused_naming_it() {
    let mut node = mock_node();
    node["colourways"] = json!({ "broken": "not a palette" });
    let doc = MemDoc::from_value(json!({ "characters": { "mock": node } }));
    let e = refused(&doc, "colourway.apply", json!({"character": "mock", "name": "broken"}));
    assert_error(
        &e,
        ErrorCode::Invalid,
        "name",
        &["colourway `broken` is stored in a shape the palette format rejects"],
        &["repair"],
    );
}
