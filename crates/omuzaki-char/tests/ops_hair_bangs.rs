//! The bangs designer ops: `hair.bangs.set` and `hair.bangs.list_presets` are each one undoable patch of one group's
//! generator, a dry run changes nothing, a refusal names the field, and the five presets are valid `BangsGen`s.
//! The helpers at the top set up a hair part with a bangs group for the preset op's tests too.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{assert_error, at, plan, project, refused, round_trip};
use omuzaki_char::ops::hair::bangs::access;
use omuzaki_char::ops::hair::bangs::presets::{PRESETS, find};
use omuzaki_char_api::{BangsGen, Generator, GroupId, HairParams};
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::check::Validate;
use omuzaki_ops_api::ErrorCode;
use serde_json::{Value, json};

/// The mock character with a second hair part, `fringe`, of the real `hair` kind: a cap and a `b-bangs` bangs group
/// written the short way a project file does, with no parameters spelled out.
pub fn bangs_project() -> MemDoc {
    let mut node = common::mock_node();
    node["parts"]["fringe"] = json!({
        "kind": "hair",
        "params": { "groups": {
            "a-cap": { "generator": { "type": "cap" } },
            "b-bangs": { "generator": { "type": "bangs" } },
        } },
    });
    MemDoc::from_value(json!({ "characters": { "mock": node } }))
}

const BANGS: &str = "characters/mock/parts/fringe/params/groups/b-bangs/generator";

fn set(field: &str, value: &Value) -> Value {
    json!({ "character": "mock", "part": "fringe", "group": "b-bangs", "field": field, "value": value })
}

/// The bangs the document holds for the group: its stored node with the generator's defaults filled in.
fn stored(doc: &MemDoc) -> BangsGen {
    let mut node = at(doc, BANGS).expect("the group has a generator");
    node.as_object_mut().unwrap().remove("type");
    serde_json::from_value(node).expect("the stored fringe decodes")
}

#[test]
fn every_preset_validates_and_the_query_lists_them_in_order() {
    let ids: Vec<_> = PRESETS.iter().map(|p| p.id).collect();
    assert_eq!(ids, ["blunt", "side-swept", "curtain", "wispy", "none"]);
    for preset in PRESETS {
        let bangs = (preset.make)();
        access::check(&bangs).unwrap_or_else(|f| panic!("{} is out of range: {f:?}", preset.id));
        let mut hair = HairParams::default();
        let group = hair.groups.get_mut(&GroupId::new("b-bangs").unwrap()).expect("the default hair has a bangs group");
        group.generator = Generator::Bangs(bangs);
        hair.validate().unwrap_or_else(|e| panic!("{} doesn't validate: {e:?}", preset.id));
        assert_eq!(find(preset.id).map(|p| p.id), Some(preset.id));
    }
    assert!(find("bob").is_none());
    let answer = plan(&project(), "hair.bangs.list_presets", json!({})).unwrap().output.unwrap();
    let listed: Vec<_> = answer["presets"].as_array().unwrap().iter().map(|p| p["id"].as_str().unwrap()).collect();
    assert_eq!(listed, ids);
    for (shown, preset) in answer["presets"].as_array().unwrap().iter().zip(PRESETS) {
        assert_eq!(shown["bangs"], serde_json::to_value((preset.make)()).unwrap());
        assert!(shown["summary"].as_str().is_some_and(|s| !s.is_empty()));
    }
}

#[test]
fn the_curtain_hooks_its_tips_outward_and_the_others_do_not() {
    let hooks = |id: &str| (find(id).unwrap().make)().layers.iter().map(|l| l.hook_deg).collect::<Vec<_>>();
    assert!(hooks("curtain").iter().all(|h| *h < 0.0), "a negative hook is outward: {:?}", hooks("curtain"));
    for id in ["blunt", "side-swept", "wispy", "none"] {
        assert!(hooks(id).iter().all(|h| *h >= 0.0), "{id} hooks inward or not at all: {:?}", hooks(id));
    }
}

#[test]
fn the_presets_differ_from_each_other() {
    for (i, a) in PRESETS.iter().enumerate() {
        for b in &PRESETS[i + 1..] {
            assert_ne!((a.make)(), (b.make)(), "{} and {} are the same fringe", a.id, b.id);
        }
    }
}

#[test]
fn each_field_is_one_undoable_edit_of_the_generator_only() {
    let cases = [
        ("layers", json!(3)),
        ("clumps", json!(12)),
        ("layers/0/clumps", json!(5)),
        ("tip_heights", json!([0.03, -0.02, 0.01])),
        ("tip_heights", json!(0.04)),
        ("sweep_deg", json!(-20.5)),
        ("hook_deg", json!(12.0)),
        ("layers/0/hook_deg", json!(-8)),
        ("wisps", json!([{ "phi_deg": 10.0, "tip_height": -0.06, "width": 0.02 }])),
    ];
    for (field, value) in cases {
        let mut doc = bangs_project();
        let planned = round_trip(&mut doc, "hair.bangs.set", set(field, &value));
        assert_eq!(planned.patch.changes.len(), 1, "{field}: one patch");
        assert!(
            planned.patch.changes[0].path.to_string().starts_with(BANGS),
            "{field}: only the group's generator changes: {}",
            planned.patch.changes[0].path
        );
        let summary = planned.output.unwrap();
        assert_eq!(summary["field"], field);
        assert_ne!(summary["before"], summary["after"], "{field}: the summary shows the change");
    }
}

#[test]
fn the_values_land_where_the_field_says() {
    let mut doc = bangs_project();
    common::apply(&mut doc, "hair.bangs.set", set("layers", &json!(3)));
    common::apply(&mut doc, "hair.bangs.set", set("layers/1/hook_deg", &json!(9.5)));
    common::apply(&mut doc, "hair.bangs.set", set("layers/2/tip_heights", &json!([0.01, 0.02])));
    common::apply(&mut doc, "hair.bangs.set", set("sweep_deg", &json!(15)));
    let bangs = stored(&doc);
    assert_eq!(bangs.layers.len(), 3);
    assert_eq!(bangs.layers.iter().map(|l| l.hook_deg).collect::<Vec<_>>(), [3.0, 9.5, 3.0]);
    assert_eq!(bangs.layers[2].tip_heights, [0.01, 0.02]);
    assert_eq!(bangs.layers[0].tip_heights, [0.0]);
    assert_eq!(bangs.sweep_deg, 15.0);
    common::apply(&mut doc, "hair.bangs.set", set("layers", &json!(1)));
    assert_eq!(stored(&doc).layers.len(), 1, "fewer layers drop the top ones");
}

#[test]
fn setting_the_value_a_group_holds_changes_nothing() {
    let doc = bangs_project();
    let planned = plan(&doc, "hair.bangs.set", set("sweep_deg", &json!(7.0))).unwrap();
    assert!(planned.patch.is_empty(), "the default sweep is 7");
    let planned = plan(&doc, "hair.bangs.set", set("layers", &json!(1))).unwrap();
    assert!(planned.patch.is_empty());
}

#[test]
fn a_part_without_parameters_is_edited_from_its_defaults() {
    let mut node = common::mock_node();
    node["parts"]["fringe"] = json!({ "kind": "hair" });
    let mut doc = MemDoc::from_value(json!({ "characters": { "mock": node } }));
    let planned = round_trip(&mut doc, "hair.bangs.set", set("sweep_deg", &json!(12)));
    assert_eq!(planned.patch.changes.len(), 1);
    assert_eq!(at(&doc, "characters/mock/parts/fringe/params/groups/b-bangs/generator/sweep_deg"), Some(json!(12.0)));
    assert!(at(&doc, "characters/mock/parts/fringe/params/groups/a-cap").is_some(), "the other groups are kept");
}

#[test]
fn bad_input_is_refused_naming_it_and_changes_nothing() {
    let doc = bangs_project();
    let before = doc.root().clone();
    let e = refused(&doc, "hair.bangs.set", set("fringe_len", &json!(1)));
    assert_error(&e, ErrorCode::Invalid, "field", &["`fringe_len`", "not a bangs field"], &["sweep_deg"]);
    let e = refused(&doc, "hair.bangs.set", set("swep_deg", &json!(1)));
    assert_error(&e, ErrorCode::Invalid, "field", &["swep_deg"], &["did you mean `sweep_deg`"]);
    let e = refused(&doc, "hair.bangs.set", set("layers/5/hook_deg", &json!(1)));
    assert_error(&e, ErrorCode::Invalid, "field", &["layers/5/hook_deg", "1 layers"], &["layers"]);
    let e = refused(&doc, "hair.bangs.set", set("layers/0/width", &json!(1)));
    assert_error(&e, ErrorCode::Invalid, "field", &["layers/0/width"], &["hook_deg"]);

    let out_of_range = [
        ("sweep_deg", json!(61)),
        ("hook_deg", json!(90)),
        ("clumps", json!(31)),
        ("clumps", json!(0)),
        ("layers", json!(7)),
        ("layers", json!(0)),
        ("tip_heights", json!([])),
        ("tip_heights", json!([0.9])),
        ("wisps", json!([{ "phi_deg": 120.0 }])),
        ("wisps", json!([{}, {}, {}, {}, {}, {}, {}, {}, {}])),
    ];
    for (field, value) in out_of_range {
        let e = refused(&doc, "hair.bangs.set", set(field, &value));
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("value")), "{field} = {value}: {e:?}");
        assert!(e.message.contains("out of range"), "{field} = {value}: {}", e.message);
    }
    for (field, value) in
        [("sweep_deg", json!("left")), ("clumps", json!(2.5)), ("clumps", json!(-1)), ("wisps", json!(3))]
    {
        let e = refused(&doc, "hair.bangs.set", set(field, &value));
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("value")), "{field} = {value}: {e:?}");
        assert!(e.message.contains(field), "the message names the field: {}", e.message);
    }
    assert_eq!(doc.root(), &before);
}

#[test]
fn an_unknown_character_part_or_group_is_refused() {
    let doc = bangs_project();
    let ask = |character: &str, part: &str, group: &str| json!({ "character": character, "part": part, "group": group, "field": "sweep_deg", "value": 1 });
    let e = refused(&doc, "hair.bangs.set", ask("ghost", "fringe", "b-bangs"));
    assert_error(&e, ErrorCode::NotFound, "character", &["ghost"], &["mock"]);
    let e = refused(&doc, "hair.bangs.set", ask("mock", "wig", "b-bangs"));
    assert_error(&e, ErrorCode::NotFound, "part", &["wig"], &["fringe"]);
    let e = refused(&doc, "hair.bangs.set", ask("mock", "fringe", "b-fringe"));
    assert_error(&e, ErrorCode::NotFound, "group", &["b-fringe"], &["b-bangs"]);
    let e = refused(&doc, "hair.bangs.set", ask("mock", "fringe", "a-cap"));
    assert_error(&e, ErrorCode::Invalid, "group", &["a-cap", "not a bangs group"], &["b-bangs"]);
    let e = refused(&doc, "hair.bangs.set", ask("mock", "body", "b-bangs"));
    assert_error(&e, ErrorCode::Invalid, "part", &["body", "not hair"], &["fringe"]);
}

/// Each field's accepted range, as the refusals report it: the edge values are accepted and the values just outside
/// are refused with the range in the message, so a suggestion can be followed back into `set`.
#[test]
fn the_ranges_the_refusals_suggest_are_the_ranges_set_accepts() {
    use access::{HOOK_LIMIT_DEG, MAX_CLUMPS, MAX_LAYERS, MAX_TIP_HEIGHTS, MAX_WISPS, SWEEP_LIMIT_DEG};
    let doc = bangs_project();
    let accepted = |field: &str, value: Value| {
        plan(&doc, "hair.bangs.set", set(field, &value)).unwrap_or_else(|e| panic!("{field} = {value}: {e:?}"));
    };
    let refused_with = |field: &str, value: Value, range: &str| {
        let e = refused(&doc, "hair.bangs.set", set(field, &value));
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("value")), "{field} = {value}: {e:?}");
        assert!(e.message.contains(range), "{field} = {value} should suggest {range}: {}", e.message);
    };
    let ints =
        [("layers", MAX_LAYERS), ("clumps", usize::from(MAX_CLUMPS)), ("layers/0/clumps", usize::from(MAX_CLUMPS))];
    for (field, hi) in ints {
        let range = format!("use 1 to {hi}");
        accepted(field, json!(1));
        accepted(field, json!(hi));
        refused_with(field, json!(0), &range);
        refused_with(field, json!(hi + 1), &range);
    }
    for (field, limit) in [("sweep_deg", SWEEP_LIMIT_DEG), ("hook_deg", HOOK_LIMIT_DEG)] {
        let range = format!("use {} to {limit}", -limit);
        accepted(field, json!(-limit));
        accepted(field, json!(limit));
        refused_with(field, json!(limit + 0.5), &range);
        refused_with(field, json!(-limit - 0.5), &range);
    }
    let height = access::HEIGHT_LIMIT;
    let range = format!("use {} to {height}", -height);
    accepted("tip_heights", json!([-height, height]));
    refused_with("tip_heights", json!([height + 0.01]), &range);
    refused_with("tip_heights", json!([-height - 0.01]), &range);
    accepted("tip_heights", json!(vec![0.0; MAX_TIP_HEIGHTS]));
    refused_with("tip_heights", json!(vec![0.0; MAX_TIP_HEIGHTS + 1]), &format!("use 1 to {MAX_TIP_HEIGHTS}"));
    refused_with("tip_heights", json!([]), &format!("use 1 to {MAX_TIP_HEIGHTS}"));
    accepted("wisps", json!(vec![json!({}); MAX_WISPS]));
    refused_with("wisps", json!(vec![json!({}); MAX_WISPS + 1]), &format!("use 0 to {MAX_WISPS}"));
    let phi = access::WISP_PHI_LIMIT_DEG;
    accepted("wisps", json!([{ "phi_deg": phi }, { "phi_deg": -phi }]));
    refused_with("wisps", json!([{ "phi_deg": phi + 0.5 }]), &format!("use {} to {phi}", -phi));
}

#[test]
fn the_none_preset_uses_the_lowest_width_the_check_accepts() {
    let none = (find("none").expect("the none preset").make)();
    assert_eq!(none.layers[0].width, access::WIDTH.0);
}
