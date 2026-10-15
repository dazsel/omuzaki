//! `part.set_param` beyond writing a leaf, and what `part.describe` costs: switching a tagged-enum variant (a hair
//! generator's type), removing a map entry (a hair group), replacing an object instead of merging into it, repairing a
//! part whose stored parameters fail the check, and `describe` doing work in proportion to the part's size.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};

use common::{apply, assert_error, plan, project, refused, round_trip};
use omuzaki_char as _;
use omuzaki_char_mock as _;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::ErrorCode;
use serde_json::{Value, json};

/// A kind of many identical groups that counts how many groups its validator looks at, to measure what `describe` costs.
mod wide {
    use std::collections::BTreeMap;
    use std::sync::atomic::Ordering;

    use omuzaki_char_api::{BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput, register_part_kind};
    use omuzaki_face_api::{Check, Validate};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    /// One group of numbers.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    pub struct Group {
        /// First, `0..=1`.
        pub a: f64,
        /// Second, `0..=2`.
        pub b: f64,
        /// Third, `0..=3`.
        pub c: f64,
    }

    impl Default for Group {
        fn default() -> Self {
            Self { a: 0.5, b: 1.0, c: 1.5 }
        }
    }

    /// The groups.
    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    pub struct WideParams {
        /// Groups by name.
        pub groups: BTreeMap<String, Group>,
    }

    impl Validate for WideParams {
        fn check(&self, c: &mut Check) {
            super::GROUPS_CHECKED.fetch_add(self.groups.len(), Ordering::Relaxed);
            for (name, group) in &self.groups {
                c.range(&format!("groups/{name}/a"), group.a, 0.0, 1.0);
                c.range(&format!("groups/{name}/b"), group.b, 0.0, 2.0);
                c.range(&format!("groups/{name}/c"), group.c, 0.0, 3.0);
            }
        }
    }

    /// The `wide` kind: builds nothing.
    pub struct Wide;

    impl PartKind for Wide {
        type Params = WideParams;

        fn meta() -> PartMeta {
            PartMeta {
                id: "wide",
                summary: "Many groups of numbers.",
                category: PartCategory::Accessory,
                requires: &[],
            }
        }

        fn build(_: &WideParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
            Ok(PartOutput::default())
        }
    }

    register_part_kind!(Wide);
}

/// Groups the `wide` kind's validator has looked at.
static GROUPS_CHECKED: AtomicUsize = AtomicUsize::new(0);

fn describe(doc: &MemDoc, part: &str) -> Value {
    plan(doc, "part.describe", json!({"character": "mock", "part": part})).unwrap().output.unwrap()
}

fn paths(description: &Value) -> Vec<&str> {
    description["parameters"].as_array().unwrap().iter().map(|p| p["path"].as_str().unwrap()).collect()
}

fn set(part: &str, path: &str, value: &Value) -> Value {
    json!({"character": "mock", "part": part, "path": path, "value": value})
}

/// The mock character with one more part, `probe`, of kind `kind` holding `params`.
fn with_probe(kind: &str, params: &Value) -> MemDoc {
    let mut node = common::mock_node();
    node["parts"]["probe"] = json!({"kind": kind, "params": params});
    MemDoc::from_value(json!({ "characters": { "mock": node } }))
}

// ------------------------------------------------------------------------------------------------ variants and entries

#[test]
fn the_tag_lists_every_variant_and_setting_it_switches_the_variant_dropping_the_old_fields() {
    let mut doc = project();
    let before = describe(&doc, "hair");
    let tag = before["parameters"].as_array().unwrap().iter().find(|p| p["path"] == "groups/a-cap/generator/type");
    let variants = tag.unwrap()["values"].as_array().unwrap();
    assert!(variants.contains(&json!("cap")) && variants.contains(&json!("bangs")), "{variants:?}");
    assert!(paths(&before).contains(&"groups/a-cap/generator/hairline_front_deg"), "the cap's own field");

    let planned = round_trip(&mut doc, "part.set_param", set("hair", "groups/a-cap/generator/type", &json!("bangs")));
    assert_eq!(planned.output.as_ref().unwrap()["changed"], true);
    let after = describe(&doc, "hair");
    assert_eq!(after["valid"], true, "{}", after["problems"]);
    let listed = paths(&after);
    assert!(listed.contains(&"groups/a-cap/generator/sweep_deg"), "the bangs' own field is there now");
    assert!(!listed.contains(&"groups/a-cap/generator/hairline_front_deg"), "the cap's field is gone");
    assert_eq!(common::at(&doc, "characters/mock/parts/hair/params/groups/a-cap/generator/type"), Some(json!("bangs")));
    assert_eq!(
        common::at(&doc, "characters/mock/parts/hair/params/groups/c-curtain/generator/type"),
        Some(json!("curtain"))
    );

    // The same variant again changes nothing.
    let same = plan(&doc, "part.set_param", set("hair", "groups/a-cap/generator/type", &json!("bangs"))).unwrap();
    assert_eq!(same.output.unwrap()["changed"], false);

    let wrong = refused(&doc, "part.set_param", set("hair", "groups/a-cap/generator/type", &json!("bagns")));
    assert_error(&wrong, ErrorCode::Invalid, "groups/a-cap/generator/type", &["bagns"], &["did you mean `bangs`"]);
}

#[test]
fn an_object_merges_unless_replace_is_set_and_the_refusal_says_the_old_fields_were_kept() {
    let mut doc = project();
    let merged = refused(&doc, "part.set_param", set("hair", "groups/a-cap/generator", &json!({"type": "bangs"})));
    assert_eq!(merged.code, ErrorCode::Invalid);
    assert!(merged.hint.as_deref().unwrap().contains("were kept"), "{:?}", merged.hint);
    assert!(merged.hint.as_deref().unwrap().contains("replace: true"), "{:?}", merged.hint);

    let mut args = set("hair", "groups/a-cap/generator", &json!({"type": "bangs"}));
    args["replace"] = json!(true);
    round_trip(&mut doc, "part.set_param", args);
    let after = describe(&doc, "hair");
    assert_eq!(after["valid"], true, "{}", after["problems"]);
    assert!(paths(&after).contains(&"groups/a-cap/generator/sweep_deg"));
    assert!(!paths(&after).contains(&"groups/a-cap/generator/hairline_front_deg"));
}

#[test]
fn null_removes_a_map_entry_and_nothing_else_that_is_required() {
    let mut doc = project();
    let before = describe(&doc, "hair");
    let entry = before["parameters"].as_array().unwrap().iter().find(|p| p["path"] == "groups/c-curtain").unwrap();
    assert_eq!(entry["optional"], true, "an entry says it can be removed");

    let planned = round_trip(&mut doc, "part.set_param", set("hair", "groups/c-curtain", &Value::Null));
    assert_eq!(planned.output.as_ref().unwrap()["after"], Value::Null);
    let after = describe(&doc, "hair");
    assert_eq!(after["valid"], true, "{}", after["problems"]);
    assert!(paths(&after).iter().all(|p| !p.starts_with("groups/c-curtain")), "{:?}", paths(&after));
    assert!(paths(&after).contains(&"groups/a-cap"), "the other group stays");
    assert_eq!(common::at(&doc, "characters/mock/parts/hair/params/groups/c-curtain"), None);

    // A parameter that is not optional still refuses null.
    let required = refused(&doc, "part.set_param", set("hair", "groups/a-cap/generator", &Value::Null));
    assert_eq!(required.code, ErrorCode::Invalid);
}

// ------------------------------------------------------------------------------------------------ repair

#[test]
fn a_stored_only_entry_with_an_invalid_value_is_listed_reachable_and_repaired_with_one_change() {
    let bad = json!({"groups": {"z-extra": {"generator": {"type": "cap", "thickness": 99.0}}}});
    let mut doc = with_probe("mock-hair", &bad);
    let d = describe(&doc, "probe");
    assert_eq!(d["valid"], false);
    let listed = paths(&d);
    assert!(listed.contains(&"groups/z-extra/generator/thickness"), "{listed:?}");
    assert!(listed.contains(&"groups/a-cap"), "the defaults' entries are listed too");

    let planned =
        round_trip(&mut doc, "part.set_param", set("probe", "groups/z-extra/generator/thickness", &json!(0.02)));
    let changes = planned.output.as_ref().unwrap()["changes"].as_array().unwrap().len();
    assert_eq!(changes, 1, "a repair writes the leaf, not every default the file left out");
    assert_eq!(describe(&doc, "probe")["valid"], true);
}

#[test]
fn repairing_a_sparse_invalid_part_writes_only_what_differs_from_the_defaults() {
    let mut doc =
        with_probe("mock-hair", &json!({"groups": {"a-cap": {"generator": {"type": "cap", "thickness": 99.0}}}}));
    let planned = apply(&mut doc, "part.set_param", set("probe", "groups/a-cap/generator/thickness", &json!(0.02)));
    assert_eq!(planned.output.unwrap()["changes"].as_array().unwrap().len(), 1);
}

// ------------------------------------------------------------------------------------------------ cost

/// The groups the validator looks at over one `describe` of a `wide` part of `n` groups.
fn groups_checked_describing(n: usize) -> usize {
    let groups: serde_json::Map<String, Value> = (0..n).map(|i| (format!("g{i:03}"), json!({}))).collect();
    let doc = with_probe("wide", &json!({ "groups": groups }));
    let before = GROUPS_CHECKED.load(Ordering::Relaxed);
    let d = describe(&doc, "probe");
    assert_eq!(d["valid"], true);
    let after = GROUPS_CHECKED.load(Ordering::Relaxed);
    let probed = d["parameters"].as_array().unwrap().iter().filter(|p| p.get("maximum").is_some()).count();
    assert_eq!(probed, 3 * n, "every number still gets its range");
    after - before
}

#[test]
fn describe_does_work_in_proportion_to_the_part_not_its_square() {
    // Tests of one binary run in parallel and share the counter: this one is the only user of the `wide` kind.
    let (small, large) = (groups_checked_describing(10), groups_checked_describing(20));
    // Linear doubles; a probe that checks the whole part quadruples.
    assert!(large * 10 <= small * 25, "10 groups checked {small} groups, 20 checked {large}: more than linear");
    assert!(large > small, "the larger part costs more");
}

#[test]
fn a_range_does_not_depend_on_the_other_entries() {
    let one = describe(&with_probe("wide", &json!({"groups": {"g1": {"b": 0.25}}})), "probe");
    let many =
        describe(&with_probe("wide", &json!({"groups": {"g0": {}, "g1": {"b": 0.25}, "g2": {"c": 2.0}}})), "probe");
    let find = |d: &Value, path: &str| d["parameters"].as_array().unwrap().iter().find(|p| p["path"] == path).cloned();
    assert_eq!(find(&one, "groups/g1/b"), find(&many, "groups/g1/b"));
}
