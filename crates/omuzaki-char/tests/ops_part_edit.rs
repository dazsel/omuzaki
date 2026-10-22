//! `part.set_param` beyond writing a leaf, and what `part.describe` costs: switching a tagged-enum variant (a hair
//! generator's type), removing a map entry (a hair group), replacing an object instead of merging into it, repairing a
//! part whose stored parameters fail the check, and `describe` doing work in proportion to the part's size.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::Mutex;
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
        /// Groups named by other values, as a hair streak names the groups it paints: each must exist.
        pub refs: Vec<String>,
        /// Pins that name a group inside an object, as a hair collider names its bone: nothing checks the name.
        pub pins: Vec<Pin>,
    }

    /// A reference held inside an object.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct Pin {
        /// The group it names.
        pub group: String,
    }

    impl Validate for WideParams {
        fn check(&self, c: &mut Check) {
            super::GROUPS_CHECKED.fetch_add(self.groups.len(), Ordering::Relaxed);
            for (i, name) in self.refs.iter().enumerate() {
                if !self.groups.contains_key(name) {
                    c.fail(&format!("refs/{i}"), format!("there is no group `{name}`"));
                }
            }
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

    /// A point on a line.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    pub struct Point {
        /// Along, `0..=1`.
        pub x: f64,
        /// Up, `0..=2`.
        pub y: f64,
    }

    impl Default for Point {
        fn default() -> Self {
            Self { x: 0.5, y: 1.0 }
        }
    }

    /// One clump: a line of points, at least two.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    pub struct Clump {
        /// The points.
        pub points: Vec<Point>,
    }

    impl Default for Clump {
        fn default() -> Self {
            Self { points: vec![Point::default(), Point::default()] }
        }
    }

    /// Clumps of points.
    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    pub struct LineParams {
        /// The clumps.
        pub clumps: Vec<Clump>,
    }

    impl Validate for LineParams {
        fn check(&self, c: &mut Check) {
            for (k, clump) in self.clumps.iter().enumerate() {
                super::POINTS_CHECKED.fetch_add(clump.points.len(), Ordering::Relaxed);
                if clump.points.len() < 2 {
                    c.fail(&format!("clumps/{k}/points"), "a clump needs at least two points");
                }
                for (i, p) in clump.points.iter().enumerate() {
                    c.range(&format!("clumps/{k}/points/{i}/x"), p.x, 0.0, 1.0);
                    c.range(&format!("clumps/{k}/points/{i}/y"), p.y, 0.0, 2.0);
                }
            }
        }
    }

    /// The `line` kind: builds nothing.
    pub struct Line;

    impl PartKind for Line {
        type Params = LineParams;

        fn meta() -> PartMeta {
            PartMeta { id: "line", summary: "A line of points.", category: PartCategory::Accessory, requires: &[] }
        }

        fn build(_: &LineParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
            Ok(PartOutput::default())
        }
    }

    register_part_kind!(Line);

    /// One share of a whole.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    pub struct Share {
        /// Its part, `0..=1`.
        pub a: f64,
    }

    impl Default for Share {
        fn default() -> Self {
            Self { a: 0.1 }
        }
    }

    /// Shares that together must not exceed one: the entries constrain one another.
    #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    pub struct CoupledParams {
        /// Shares by name.
        pub shares: BTreeMap<String, Share>,
    }

    impl Validate for CoupledParams {
        fn check(&self, c: &mut Check) {
            for (name, share) in &self.shares {
                c.range(&format!("shares/{name}/a"), share.a, 0.0, 1.0);
            }
            if self.shares.values().map(|s| s.a).sum::<f64>() > 1.0 {
                c.fail("shares", "must not add up to more than 1");
            }
        }
    }

    /// The `coupled` kind: builds nothing.
    pub struct Coupled;

    impl PartKind for Coupled {
        type Params = CoupledParams;

        fn meta() -> PartMeta {
            PartMeta { id: "coupled", summary: "Shares of a whole.", category: PartCategory::Accessory, requires: &[] }
        }

        fn build(_: &CoupledParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
            Ok(PartOutput::default())
        }
    }

    register_part_kind!(Coupled);
}

/// Groups the `wide` kind's validator has looked at.
static GROUPS_CHECKED: AtomicUsize = AtomicUsize::new(0);
/// Held by the tests that use the `wide` kind, so the counter reads one test's work.
static WIDE: Mutex<()> = Mutex::new(());
/// Points the `line` kind's validator has looked at (under the same lock).
static POINTS_CHECKED: AtomicUsize = AtomicUsize::new(0);

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

/// The groups the validator looks at over one `describe` of a `wide` part of `n` groups, `named` of them named by
/// `refs`.
fn groups_checked_describing(n: usize, named: usize) -> usize {
    let refs: Vec<String> = (0..named).map(|i| format!("g{i:03}")).collect();
    groups_checked(n, &json!({ "refs": refs }))
}

/// The groups the validator looks at over one `describe` of a `wide` part of `n` groups and the other fields `more`.
fn groups_checked(n: usize, more: &Value) -> usize {
    let groups: serde_json::Map<String, Value> = (0..n).map(|i| (format!("g{i:03}"), json!({}))).collect();
    let mut params = more.clone();
    params["groups"] = Value::Object(groups);
    let doc = with_probe("wide", &params);
    let _only = WIDE.lock().unwrap();
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
    // Linear doubles; a probe that checks the whole part quadruples. Half the groups are named by a list of references
    // (a streak's `groups`), so the references grow with the part.
    let timed = |n: usize| {
        let start = std::time::Instant::now();
        let checked = groups_checked_describing(n, n / 2);
        eprintln!("{n} groups, {} named: {checked} groups checked, {:?}", n / 2, start.elapsed());
        checked
    };
    let (small, large) = (timed(10), timed(20));
    assert!(large * 10 <= small * 25, "10 groups checked {small}, 20 checked {large}: not linear");
    assert!(large > small, "the larger part costs more");
    // None named, and a few named, as before.
    let (small, large) = (groups_checked_describing(10, 2), groups_checked_describing(20, 2));
    assert!(large * 10 <= small * 25, "10 groups checked {small}, 20 checked {large}: not linear");
}

#[test]
fn describe_stays_linear_when_every_group_is_named_from_inside_an_object() {
    // As hair colliders name their bones: `{"group": "g007"}` objects in a list, one per group. A probe carries a few
    // pins and so keeps a few groups; one that kept every group a pin names would check the whole part per number.
    let pinned = |n: usize| {
        let pins: Vec<Value> = (0..n).map(|i| json!({"group": format!("g{i:03}")})).collect();
        let checked = groups_checked(n, &json!({ "pins": pins }));
        eprintln!("{n} groups, each pinned: {checked} groups checked");
        checked
    };
    let (small, large) = (pinned(10), pinned(20));
    assert!(large * 10 <= small * 25, "10 groups checked {small}, 20 checked {large}: not linear");
}

/// The points the `line` validator looks at over one `describe` of two clumps of `n` points each.
fn points_checked_describing(n: usize) -> usize {
    let points: Vec<Value> = (0..n).map(|_| json!({})).collect();
    let doc = with_probe("line", &json!({ "clumps": [{ "points": points }, { "points": points }] }));
    let _only = WIDE.lock().unwrap();
    let before = POINTS_CHECKED.load(Ordering::Relaxed);
    let start = std::time::Instant::now();
    let d = describe(&doc, "probe");
    eprintln!("two clumps of {n} points: {:?}", start.elapsed());
    let checked = POINTS_CHECKED.load(Ordering::Relaxed) - before;
    assert_eq!(d["valid"], true);
    let probed = d["parameters"].as_array().unwrap().iter().filter(|p| p.get("maximum").is_some()).count();
    assert_eq!(probed, 4 * n, "every number still gets its range");
    checked
}

#[test]
fn describe_does_work_in_proportion_to_growing_lists_not_their_square() {
    let (small, large) = (points_checked_describing(20), points_checked_describing(40));
    assert!(large * 10 <= small * 25, "20 points a clump checked {small}, 40 checked {large}: not linear");
}

#[test]
fn the_ends_of_a_list_keep_the_neighbour_a_minimum_length_needs() {
    // The first and last point are probed with one neighbour, so "at least two points" still holds.
    let d =
        describe(&with_probe("line", &json!({"clumps": [{"points": [{"x": 0.1}, {"x": 0.2}, {"x": 0.3}]}]})), "probe");
    let find = |path: &str| d["parameters"].as_array().unwrap().iter().find(|p| p["path"] == path).cloned().unwrap();
    for path in ["clumps/0/points/0/x", "clumps/0/points/1/x", "clumps/0/points/2/x"] {
        assert_eq!((find(path)["minimum"].clone(), find(path)["maximum"].clone()), (json!(0.0), json!(1.0)), "{path}");
    }
}

#[test]
fn a_range_does_not_depend_on_the_other_entries() {
    let _only = WIDE.lock().unwrap();
    let one = describe(&with_probe("wide", &json!({"groups": {"g1": {"b": 0.25}}})), "probe");
    let many =
        describe(&with_probe("wide", &json!({"groups": {"g0": {}, "g1": {"b": 0.25}, "g2": {"c": 2.0}}})), "probe");
    let find = |d: &Value, path: &str| d["parameters"].as_array().unwrap().iter().find(|p| p["path"] == path).cloned();
    assert_eq!(find(&one, "groups/g1/b"), find(&many, "groups/g1/b"));
}

#[test]
fn entries_that_constrain_one_another_get_the_range_the_whole_part_gives() {
    let d = describe(&with_probe("coupled", &json!({"shares": {"x": {"a": 0.25}, "y": {"a": 0.5}}})), "probe");
    let find = |path: &str| d["parameters"].as_array().unwrap().iter().find(|p| p["path"] == path).cloned().unwrap();
    // The bound is the last float the sum accepts, so it is the exact share to within the last digits.
    let max = |path: &str| find(path)["maximum"].as_f64().unwrap();
    assert!((max("shares/x/a") - 0.5).abs() < 1e-12, "x may take what y leaves: {}", max("shares/x/a"));
    assert!((max("shares/y/a") - 0.75).abs() < 1e-12, "{}", max("shares/y/a"));
}

#[test]
fn a_stored_tag_that_names_no_variant_is_listed_and_repaired_by_setting_it() {
    let bad = json!({"groups": {"z": {"generator": {"type": "bogus"}}}});
    let mut doc = with_probe("mock-hair", &bad);
    let d = describe(&doc, "probe");
    assert_eq!(d["valid"], false);
    let tag = d["parameters"].as_array().unwrap().iter().find(|p| p["path"] == "groups/z/generator/type").cloned();
    let tag = tag.expect("the tag is listed so it can be set");
    assert!(tag["values"].as_array().unwrap().contains(&json!("cap")), "{tag}");
    assert_eq!(tag["current"], json!("bogus"));

    round_trip(&mut doc, "part.set_param", set("probe", "groups/z/generator/type", &json!("cap")));
    assert_eq!(describe(&doc, "probe")["valid"], true);
}

#[test]
fn a_group_named_like_a_tag_is_probed_once() {
    // `cap` is a group's key and also a generator tag, so a value names it; its path must not be walked twice.
    let d = describe(&with_probe("mock-hair", &json!({"groups": {"cap": {"generator": {"type": "cap"}}}})), "probe");
    let thickness = d["parameters"].as_array().unwrap().iter().find(|p| p["path"] == "groups/cap/generator/thickness");
    let thickness = thickness.expect("listed");
    assert_eq!((&thickness["minimum"], &thickness["maximum"]), (&json!(0.002), &json!(0.06)), "{thickness}");
}
