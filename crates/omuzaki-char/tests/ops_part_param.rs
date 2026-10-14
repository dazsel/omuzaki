//! `part.set_param`, `part.describe` and `part.list_kinds` against the in-memory document, for every part kind this test
//! binary can see: the four mock kinds (which take the real head, hair and body parameter types) and `sample`, a kind
//! registered here that has one of every parameter shape the mocks lack (an optional section, a unit (`x-ui.unit`), a pattern, a
//! bound the validator ties to another field).
//!
//! The ops are generated against whatever kinds are registered, so a real kind that lands later is covered without a
//! change here. Each subject is a part of one kind stored three ways: with no parameters at all (every section is
//! created whole by the first edit), with the full normalised parameters (an edit is one leaf), and the fixture's own
//! sparse parts.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::sync::OnceLock;

use common::{apply, assert_error, definition, plan, project, refused, round_trip};
use omuzaki_char as _;
use omuzaki_char_api::{PartKindRegistration, find_kind, kinds};
use omuzaki_char_mock as _;
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_face_api::ParamErrors;
use omuzaki_ops_api::{ErrorCode, OpError, OpKind, Plan, Scope, Undo, find};
use proptest::prelude::*;
use serde_json::{Value, json};

/// A kind with one of every parameter shape the mock kinds lack, taking part in every "for each kind" test.
mod sample {
    use std::collections::BTreeMap;

    use omuzaki_char_api::{BuildCtx, BuildError, PartCategory, PartKind, PartMeta, PartOutput, register_part_kind};
    use omuzaki_face_api::{Check, Validate};
    use schemars::JsonSchema;
    use serde::{Deserialize, Serialize};

    /// How the surface looks.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum Finish {
        /// No shine.
        #[default]
        Matte,
        /// Shiny.
        Gloss,
    }

    /// An optional trim along the edge.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    pub struct Trim {
        /// How deep the trim cuts, `0..=1`.
        pub depth: f64,
        /// How far the edge is rounded: never more than the depth.
        pub edge: f64,
    }

    impl Default for Trim {
        fn default() -> Self {
            Self { depth: 0.5, edge: 0.1 }
        }
    }

    impl Validate for Trim {
        fn check(&self, c: &mut Check) {
            c.range("depth", self.depth, 0.0, 1.0);
            c.range("edge", self.edge, 0.0, 1.0);
            if self.edge > self.depth {
                c.fail("edge", "must not exceed the depth");
            }
        }
    }

    /// A named knob.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    pub struct Knob {
        /// How far it is turned, `0..=1`.
        pub level: f64,
    }

    impl Default for Knob {
        fn default() -> Self {
            Self { level: 0.5 }
        }
    }

    impl Validate for Knob {
        fn check(&self, c: &mut Check) {
            c.range("level", self.level, 0.0, 1.0);
        }
    }

    /// The parameters of the `sample` kind.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    pub struct SampleParams {
        /// How wide it is.
        #[schemars(extend("x-ui" = {"unit": "m"}))]
        pub width: f64,
        /// How many teeth it has.
        pub teeth: u8,
        /// A radius that must be above zero.
        pub radius: f64,
        /// Whether it glows.
        pub glow: bool,
        /// The surface.
        pub finish: Finish,
        /// A label in lowercase letters.
        #[schemars(regex(pattern = "^[a-z]+$"))]
        pub tag: String,
        /// A trim, or none.
        #[serde(skip_serializing_if = "Option::is_none")]
        pub trim: Option<Trim>,
        /// Two positions, `0..=1` each.
        pub pair: [f64; 2],
        /// Any number of marks, `-10..=10` each.
        pub marks: Vec<f64>,
        /// Knobs by name.
        pub knobs: BTreeMap<String, Knob>,
    }

    impl Default for SampleParams {
        fn default() -> Self {
            Self {
                width: 0.5,
                teeth: 3,
                radius: 1.0,
                glow: false,
                finish: Finish::Matte,
                tag: "ab".to_owned(),
                trim: None,
                pair: [0.2, 0.8],
                marks: Vec::new(),
                knobs: BTreeMap::from([("ear".to_owned(), Knob { level: 0.25 })]),
            }
        }
    }

    impl Validate for SampleParams {
        fn check(&self, c: &mut Check) {
            c.range("width", self.width, 0.1, 2.0);
            if !(2..=5).contains(&self.teeth) {
                c.fail("teeth", format!("{} is out of range: use 2 to 5", self.teeth));
            }
            c.positive("radius", self.radius);
            if self.tag.is_empty() || !self.tag.chars().all(|ch| ch.is_ascii_lowercase()) {
                c.fail("tag", "use lowercase letters only");
            }
            if let Some(trim) = &self.trim {
                c.nested("trim", trim);
            }
            for (i, v) in self.pair.iter().enumerate() {
                c.range(&format!("pair/{i}"), *v, 0.0, 1.0);
            }
            for (i, v) in self.marks.iter().enumerate() {
                c.range(&format!("marks/{i}"), *v, -10.0, 10.0);
            }
            for (name, knob) in &self.knobs {
                c.nested(&format!("knobs/{name}"), knob);
            }
        }
    }

    /// The `sample` kind: builds nothing.
    pub struct Sample;

    impl PartKind for Sample {
        type Params = SampleParams;

        fn meta() -> PartMeta {
            PartMeta {
                id: "sample",
                summary: "Build nothing; take one of every parameter shape.",
                category: PartCategory::Accessory,
                requires: &[],
            }
        }

        fn build(_: &SampleParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
            Ok(PartOutput::default())
        }
    }

    register_part_kind!(Sample);
}

// ------------------------------------------------------------------------------------------------ harness

const CHARACTER: &str = "mock";

/// The mock character with one more part, `probe`, of kind `kind` holding `params`.
fn project_with_probe(kind: &str, params: &Value) -> MemDoc {
    let mut node = common::mock_node();
    node["parts"]["probe"] = json!({"kind": kind, "params": params});
    MemDoc::from_value(json!({ "characters": { CHARACTER: node } }))
}

fn describe(doc: &MemDoc, part: &str) -> Value {
    plan(doc, "part.describe", json!({"character": CHARACTER, "part": part})).unwrap().output.unwrap()
}

fn set(doc: &MemDoc, part: &str, path: &str, value: &Value) -> Result<Plan, OpError> {
    plan(doc, "part.set_param", json!({"character": CHARACTER, "part": part, "path": path, "value": value}))
}

fn set_args(part: &str, path: &str, value: &Value) -> Value {
    json!({"character": CHARACTER, "part": part, "path": path, "value": value})
}

/// The stored parameters of `part`, as the document holds them.
fn stored(doc: &MemDoc, part: &str) -> Value {
    common::at(doc, &format!("characters/{CHARACTER}/parts/{part}/params")).unwrap_or_else(|| json!({}))
}

/// A part of one kind stored one way, with its description.
struct Subject {
    label: String,
    doc: MemDoc,
    part: String,
    kind: &'static PartKindRegistration,
    /// Every parameter is stored, so an edit is exactly one leaf.
    filled: bool,
    description: Value,
}

/// Every registered kind stored empty and filled, and the fixture's own sparse parts. Built once: describing a part
/// probes every number, which is the slow part.
fn subjects() -> &'static [Subject] {
    static SUBJECTS: OnceLock<Vec<Subject>> = OnceLock::new();
    SUBJECTS.get_or_init(|| {
        let mut all = Vec::new();
        let mut registered: Vec<_> = kinds().collect();
        registered.sort_by_key(|k| (k.meta)().id);
        for kind in registered {
            let id = (kind.meta)().id;
            for (label, params, filled) in [("empty", json!({}), false), ("filled", (kind.defaults)(), true)] {
                let doc = project_with_probe(id, &params);
                let description = describe(&doc, "probe");
                all.push(Subject {
                    label: format!("{id} {label}"),
                    doc,
                    part: "probe".into(),
                    kind,
                    filled,
                    description,
                });
            }
        }
        for part in ["body", "head", "hair", "shirt", "vest"] {
            let doc = project();
            let id = definition(&doc, CHARACTER).parts[&part.parse().unwrap()].kind.to_string();
            let description = describe(&doc, part);
            let kind = find_kind(&id).unwrap();
            all.push(Subject {
                label: format!("fixture {part}"),
                doc,
                part: part.into(),
                kind,
                filled: false,
                description,
            });
        }
        all
    })
}

/// The entries of a description that hold a value (not the sections and lists that are listed through their members).
fn leaves(description: &Value) -> impl Iterator<Item = &Value> {
    description["parameters"].as_array().unwrap().iter().filter(|p| p.get("current").is_some())
}

fn path_of(entry: &Value) -> &str {
    entry["path"].as_str().unwrap()
}

fn is_number(entry: &Value) -> bool {
    matches!(entry["type"].as_str(), Some("number" | "integer"))
}

/// What the kind makes of `current` with the number at `path` replaced: the oracle every edit is held to.
fn oracle(subject: &Subject, path: &str, value: &Value) -> Result<Value, ParamErrors> {
    let mut working = subject.description["current"].clone();
    *working.pointer_mut(&format!("/{path}")).expect("the leaf is in the current values") = value.clone();
    (subject.kind.check)(&working)
}

/// The parameters stored after `doc` was edited, normalised by the kind.
fn normalised(subject: &Subject, doc: &MemDoc) -> Value {
    (subject.kind.check)(&stored(doc, &subject.part)).expect("the edited parameters pass the check")
}

/// A value different from the entry's current one that its kind accepts, if one can be found.
fn another_valid(subject: &Subject, entry: &Value) -> Option<Value> {
    let current = &entry["current"];
    let number = |key: &str| entry.get(key).and_then(Value::as_f64);
    let candidates: Vec<Value> = match entry["type"].as_str()? {
        "number" => {
            let (lo, hi) = (number("minimum"), number("maximum"));
            let middle = lo.zip(hi).map(|(lo, hi)| lo / 2.0 + hi / 2.0);
            [hi, lo, middle, current.as_f64().map(|c| c * 0.5 + 0.01)].into_iter().flatten().map(|x| json!(x)).collect()
        }
        "integer" => {
            let at = current.as_i64()?;
            let (lo, hi) = (entry.get("minimum").and_then(Value::as_i64), entry.get("maximum").and_then(Value::as_i64));
            [hi, lo, Some(at + 1), Some(at - 1)].into_iter().flatten().map(|i| json!(i)).collect()
        }
        "boolean" => vec![json!(!current.as_bool()?)],
        "choice" => entry["values"].as_array()?.clone(),
        _ => return None,
    };
    let path = path_of(entry);
    candidates.into_iter().find(|candidate| candidate != current && oracle(subject, path, candidate).is_ok())
}

// ------------------------------------------------------------------------------------------------ the ops as an agent sees them

#[test]
fn the_three_ops_are_registered_with_closed_documented_schemas() {
    let expected = [
        ("part.set_param", OpKind::Edit, Scope::EditCharacter, Undo::Auto, vec!["character", "part", "path", "value"]),
        ("part.describe", OpKind::Query, Scope::Read, Undo::None, vec!["character", "part"]),
        ("part.list_kinds", OpKind::Query, Scope::Read, Undo::None, vec![]),
    ];
    for (name, kind, scope, undo, required) in expected {
        let registration = find(name).unwrap_or_else(|| panic!("{name} isn't registered"));
        let meta = (registration.meta)();
        assert_eq!((meta.kind, meta.scope, meta.undo), (kind, scope, undo), "{name}");

        let schema = serde_json::to_value((registration.schema)()).unwrap();
        assert_eq!(schema["additionalProperties"], json!(false), "{name}: a typo'd argument must be refused");
        assert!(!schema.to_string().contains("$ref"), "{name}: MCP schemas are inlined");
        assert!(schema["description"].as_str().unwrap().len() > 100, "{name}: the struct docs are the agent's manual");
        for (field, property) in schema["properties"].as_object().into_iter().flatten() {
            assert!(property["description"].is_string(), "{name}.{field} has no description");
        }
        let mut got: Vec<&str> =
            schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        got.sort_unstable();
        assert_eq!(got, required, "{name}");
    }
}

#[test]
fn list_kinds_gives_every_registered_kind_sorted_by_id() {
    let answer = plan(&MemDoc::new(), "part.list_kinds", json!({})).unwrap().output.unwrap();
    let listed = answer["kinds"].as_array().unwrap();
    let ids: Vec<&str> = listed.iter().map(|k| k["id"].as_str().unwrap()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    assert_eq!(ids, sorted, "sorted by id");
    assert_eq!(ids.len(), kinds().count(), "every kind, once");
    for kind in kinds() {
        let meta = (kind.meta)();
        let entry = listed.iter().find(|k| k["id"] == meta.id).unwrap();
        assert_eq!(entry["summary"], meta.summary);
        assert_eq!(entry["category"], serde_json::to_value(meta.category).unwrap());
        assert_eq!(entry["requires"], json!(meta.requires));
    }
    let hair = listed.iter().find(|k| k["id"] == "mock-hair").unwrap();
    assert_eq!(hair["requires"], json!(["mock-head"]));
    assert_eq!(hair["category"], "hair");

    let typo = refused(&MemDoc::new(), "part.list_kinds", json!({"category": "hair"}));
    assert!(typo.message.contains("unknown field `category`"), "{}", typo.message);
}

// ------------------------------------------------------------------------------------------------ part.describe

#[test]
fn describe_gives_the_kind_the_schema_the_defaults_and_the_values_of_every_subject() {
    for subject in subjects() {
        let d = &subject.description;
        let meta = (subject.kind.meta)();
        assert_eq!(
            (d["kind"].as_str(), d["summary"].as_str()),
            (Some(meta.id), Some(meta.summary)),
            "{}",
            subject.label
        );
        assert_eq!(d["category"], serde_json::to_value(meta.category).unwrap(), "{}", subject.label);
        assert_eq!(d["requires"], json!(meta.requires), "{}", subject.label);
        assert_eq!(d["valid"], true, "{}", subject.label);
        assert_eq!(d["schema"], serde_json::to_value((subject.kind.schema)()).unwrap(), "{}", subject.label);
        assert_eq!(d["defaults"], (subject.kind.defaults)(), "{}", subject.label);
        let current = (subject.kind.check)(&stored(&subject.doc, &subject.part)).unwrap();
        assert_eq!(d["current"], current, "{}: the values are what the kind makes of the stored ones", subject.label);
        assert!(!d["parameters"].as_array().unwrap().is_empty(), "{}", subject.label);
        assert!(
            d["ranges"].as_str().unwrap().contains("bound that is missing"),
            "the answer says what a missing bound means"
        );
    }
}

#[test]
fn every_parameter_says_what_it_is_and_every_number_its_range() {
    let mut bounded = 0;
    for subject in subjects() {
        let mut seen = std::collections::BTreeSet::new();
        for entry in subject.description["parameters"].as_array().unwrap() {
            let path = path_of(entry);
            assert!(seen.insert(path.to_owned()), "{}: `{path}` is listed twice", subject.label);
            assert!(entry["type"].is_string(), "{}: `{path}` has no type", subject.label);
            // The tag of a tagged enum (`type`) is a name the schema derives, not a field anyone wrote a comment on.
            let tag = entry["type"] == "choice" && entry["values"].as_array().is_some_and(|v| v.len() == 1);
            assert!(
                tag || entry["description"].as_str().is_some_and(|d| !d.is_empty()),
                "{}: `{path}` has no description",
                subject.label
            );
            let range = ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"].map(|k| entry.get(k));
            if range.iter().any(Option::is_some) {
                bounded += 1;
                assert!(is_number(entry), "{}: `{path}` has a range but is not a number", subject.label);
                let (default, current) = (entry["default"].as_f64(), entry["current"].as_f64());
                for value in [default, current].into_iter().flatten() {
                    let at_least = |k| entry.get(k).and_then(Value::as_f64);
                    assert!(
                        at_least("minimum").is_none_or(|m| m <= value),
                        "{}: `{path}` {value} below its minimum",
                        subject.label
                    );
                    assert!(
                        at_least("maximum").is_none_or(|m| m >= value),
                        "{}: `{path}` {value} above its maximum",
                        subject.label
                    );
                    assert!(
                        at_least("exclusiveMinimum").is_none_or(|m| m < value),
                        "{}: `{path}` {value}",
                        subject.label
                    );
                }
            }
            if entry.get("current").is_some()
                && entry["type"] != "list"
                && entry["type"] != "map"
                && entry["type"] != "any"
            {
                assert!(
                    entry.get("default").is_some()
                        || entry["current"].is_null()
                        || entry["optional"] == true
                        || subject.label.starts_with("fixture"),
                    "{}: `{path}` has a value but no default",
                    subject.label
                );
            }
        }
    }
    assert!(bounded > 100, "most numbers of the real parameter types are bounded; found {bounded}");
}

#[test]
fn describe_is_deterministic_and_lists_parents_before_children_with_sorted_keys() {
    for subject in subjects() {
        assert_eq!(
            describe(&subject.doc, &subject.part),
            subject.description,
            "{}: a second call differs",
            subject.label
        );
        let paths: Vec<&str> = subject.description["parameters"].as_array().unwrap().iter().map(path_of).collect();
        for (i, path) in paths.iter().enumerate() {
            if let Some((parent, _)) = path.rsplit_once('/') {
                let at = paths
                    .iter()
                    .position(|p| *p == parent)
                    .unwrap_or_else(|| panic!("{}: `{path}` has no parent", subject.label));
                assert!(at < i, "{}: `{parent}` must come before `{path}`", subject.label);
            }
        }
        let siblings = |parent: &str| -> Vec<&str> {
            paths
                .iter()
                .filter_map(|p| p.strip_prefix(parent)?.strip_prefix('/').or(parent.is_empty().then_some(*p)))
                .filter(|r| !r.contains('/'))
                .collect()
        };
        let top = siblings("");
        let mut sorted = top.clone();
        sorted.sort_unstable();
        assert_eq!(top, sorted, "{}: the top-level keys are in alphabetical order", subject.label);
    }
}

#[test]
fn the_sample_kind_reports_every_parameter_shape() {
    let doc = project_with_probe("sample", &json!({"knobs": {"ear": {"level": 0.25}, "tail": {"level": 0.75}}}));
    let d = describe(&doc, "probe");
    let entry = |path: &str| {
        d["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| path_of(p) == path)
            .unwrap_or_else(|| panic!("no `{path}`"))
            .clone()
    };
    let paths: Vec<&str> = d["parameters"].as_array().unwrap().iter().map(path_of).collect();
    assert_eq!(
        paths,
        [
            "finish",
            "glow",
            "knobs",
            "knobs/ear",
            "knobs/ear/level",
            "knobs/tail",
            "knobs/tail/level",
            "marks",
            "pair",
            "pair/0",
            "pair/1",
            "radius",
            "tag",
            "teeth",
            "trim",
            "trim/depth",
            "trim/edge",
            "width"
        ]
    );

    let width = entry("width");
    assert_eq!((width["type"].as_str(), width["unit"].as_str()), (Some("number"), Some("m")));
    assert_eq!(
        (&width["minimum"], &width["maximum"], &width["default"], &width["current"]),
        (&json!(0.1), &json!(2.0), &json!(0.5), &json!(0.5))
    );
    assert_eq!(width["description"], "How wide it is.");

    let teeth = entry("teeth");
    assert_eq!(
        (teeth["type"].as_str(), &teeth["minimum"], &teeth["maximum"]),
        (Some("integer"), &json!(2), &json!(5)),
        "the validator is tighter than the u8 in the schema"
    );
    assert_eq!(entry("radius")["exclusiveMinimum"], json!(0.0), "'greater than 0' is an open bound, not 5e-324");
    assert!(entry("radius").get("minimum").is_none() && entry("radius").get("maximum").is_none());
    assert_eq!((entry("glow")["type"].as_str(), &entry("glow")["default"]), (Some("boolean"), &json!(false)));
    assert_eq!(entry("finish")["type"], "choice");
    assert_eq!(entry("finish")["values"], json!(["matte", "gloss"]), "in the order the kind declares them");
    assert_eq!(entry("tag")["pattern"], "^[a-z]+$");
    assert_eq!(entry("knobs")["type"], "map");
    assert_eq!(entry("knobs/tail/level")["current"], json!(0.75));
    assert_eq!(entry("marks")["type"], "list");
    assert_eq!(entry("marks")["current"], json!([]));
    assert_eq!((&entry("pair/0")["minimum"], &entry("pair/1")["maximum"]), (&json!(0.0), &json!(1.0)));
    assert_eq!(
        entry("pair/1")["description"],
        "Two positions, `0..=1` each.",
        "members of a fixed-size list inherit its description"
    );

    // The optional section is off, so its members have no value to probe a range from.
    let trim = entry("trim");
    assert_eq!((trim["type"].as_str(), &trim["optional"]), (Some("section"), &json!(true)));
    let depth = entry("trim/depth");
    assert!(
        depth.get("current").is_none() && depth.get("default").is_none() && depth.get("maximum").is_none(),
        "{depth}"
    );
    assert_eq!(depth["description"], "How deep the trim cuts, `0..=1`.");
}

#[test]
fn a_cross_field_rule_shows_as_a_bound_that_moves_with_the_other_field() {
    let range_of_edge = |depth: f64| {
        let doc = project_with_probe("sample", &json!({"trim": {"depth": depth, "edge": 0.05}}));
        let d = describe(&doc, "probe");
        let edge = d["parameters"].as_array().unwrap().iter().find(|p| path_of(p) == "trim/edge").unwrap().clone();
        edge["maximum"].clone()
    };
    assert_eq!(range_of_edge(0.5), json!(0.5), "the edge may not exceed the depth");
    assert_eq!(range_of_edge(0.25), json!(0.25));
}

#[test]
fn a_part_whose_stored_parameters_fail_the_check_is_still_described_and_can_be_repaired() {
    let doc = project_with_probe("sample", &json!({"width": 99.0, "glow": true}));
    let d = describe(&doc, "probe");
    assert_eq!(d["valid"], false);
    assert!(d["problems"][0].as_str().unwrap().starts_with("width: 99"), "{}", d["problems"]);
    assert_eq!(d["current"]["width"], json!(99.0), "the stored values are shown as they are");
    let width = d["parameters"].as_array().unwrap().iter().find(|p| path_of(p) == "width").unwrap().clone();
    assert_eq!(
        (&width["maximum"], &width["default"]),
        (&json!(2.0), &json!(0.5)),
        "ranges and defaults come from the defaults"
    );

    let other = refused(&doc, "part.set_param", set_args("probe", "glow", &json!(false)));
    assert_error(&other, ErrorCode::Invalid, "width", &["width: 99"], &["a number from 0.1 to 2"]);

    let mut doc = doc;
    apply(&mut doc, "part.set_param", set_args("probe", "width", &json!(0.5)));
    assert_eq!(describe(&doc, "probe")["valid"], true);
    assert_eq!(
        common::at(&doc, "characters/mock/parts/probe/params/glow"),
        Some(json!(true)),
        "the other stored values stay"
    );
}

// ------------------------------------------------------------------------------------------------ part.set_param

#[test]
fn setting_every_leaf_to_its_current_value_changes_nothing() {
    let mut count = 0;
    for subject in subjects() {
        for entry in leaves(&subject.description) {
            let path = path_of(entry);
            let planned = set(&subject.doc, &subject.part, path, &entry["current"])
                .unwrap_or_else(|e| panic!("{} `{path}`: {e:?}", subject.label));
            assert!(planned.patch.is_empty(), "{} `{path}`: {:?}", subject.label, planned.patch);
            assert_eq!(planned.output.unwrap()["changed"], false, "{} `{path}`", subject.label);
            if subject.label.ends_with("empty") || subject.label.ends_with("filled") {
                assert_eq!(entry.get("default"), entry.get("current"), "{} `{path}`", subject.label);
            }
            count += 1;
        }
    }
    assert!(count > 300, "every kind's parameters were tried; got {count}");
}

#[test]
fn a_new_value_is_written_as_the_check_returns_it_and_undoes_exactly() {
    let mut tried = 0;
    for subject in subjects() {
        for entry in leaves(&subject.description) {
            let Some(new) = another_valid(subject, entry) else { continue };
            let path = path_of(entry);
            let expected = oracle(subject, path, &new).unwrap();
            let mut doc = subject.doc.clone();
            let planned = round_trip(&mut doc, "part.set_param", set_args(&subject.part, path, &new));
            assert_eq!(
                normalised(subject, &doc),
                expected,
                "{} `{path}`: the stored result is what check returns",
                subject.label
            );
            let out = planned.output.as_ref().unwrap();
            assert_eq!(
                (&out["before"], &out["changed"]),
                (&entry["current"], &json!(true)),
                "{} `{path}`",
                subject.label
            );
            if subject.filled {
                assert_eq!(
                    planned.patch.changes.len(),
                    1,
                    "{} `{path}`: only the changed leaf is written",
                    subject.label
                );
                let change = &planned.patch.changes[0];
                assert_eq!(
                    change.path.to_string(),
                    format!("characters/{CHARACTER}/parts/{}/params/{path}", subject.part)
                );
                assert_eq!(change.before.as_ref(), Some(&entry["current"]));
            }
            tried += 1;
        }
    }
    assert!(tried > 300, "tried {tried}");
}

#[test]
fn out_of_range_values_are_refused_naming_the_parameter_and_what_it_accepts() {
    let (mut refusals, mut blamed_elsewhere) = (0, 0);
    for subject in subjects() {
        for entry in leaves(&subject.description).filter(|e| is_number(e)) {
            let path = path_of(entry);
            let whole = entry["type"] == "integer";
            let plain = |bound: &Value| match bound.as_f64() {
                Some(x) if !whole => x.to_string(),
                _ => bound.to_string(),
            };
            let mut hint = Vec::new();
            for key in ["minimum", "maximum", "exclusiveMinimum", "exclusiveMaximum"] {
                hint.extend(entry.get(key).map(plain));
            }
            let below = entry.get("minimum").map(|m| step(m, false)).or_else(|| entry.get("exclusiveMinimum").cloned());
            let above = entry.get("maximum").map(|m| step(m, true)).or_else(|| entry.get("exclusiveMaximum").cloned());
            for value in [below, above].into_iter().flatten() {
                let error = set(&subject.doc, &subject.part, path, &value)
                    .expect_err(&format!("{} `{path}` = {value}", subject.label));
                // A rule between two fields can blame the other one (`to` must be greater than `from`): then the hint
                // is that field's, and the message still names it.
                if error.field.as_deref() == Some(path) {
                    let hints: Vec<&str> = hint.iter().map(String::as_str).collect();
                    assert_error(&error, ErrorCode::Invalid, path, &[path], &hints);
                } else {
                    assert_eq!(error.code, ErrorCode::Invalid);
                    let other = error.field.as_deref().unwrap();
                    assert!(error.message.contains(other), "{}: {error:?}", subject.label);
                    blamed_elsewhere += 1;
                }
                refusals += 1;
            }
            // The edge itself is accepted when it is a closed bound.
            for key in ["minimum", "maximum"] {
                if let Some(edge) = entry.get(key) {
                    set(&subject.doc, &subject.part, path, edge)
                        .unwrap_or_else(|e| panic!("{} `{path}` = {edge}: {e:?}", subject.label));
                }
            }
        }
    }
    assert!(refusals > 150, "refused {refusals}");
    assert!(blamed_elsewhere * 10 < refusals, "{blamed_elsewhere} of {refusals} blame another field");
}

/// The number just past `bound`, in the direction of `up`: the next float, or one more whole number.
fn step(bound: &Value, up: bool) -> Value {
    if let Some(whole) = bound.as_i64() {
        return json!(if up { whole + 1 } else { whole - 1 });
    }
    let x = bound.as_f64().unwrap();
    json!(if up { x.next_up() } else { x.next_down() })
}

#[test]
fn values_of_the_wrong_type_are_refused_naming_the_parameter_and_what_it_takes() {
    let (mut refusals, mut patterned) = (0, 0);
    for subject in subjects() {
        for entry in leaves(&subject.description) {
            let path = path_of(entry);
            let wrong: Vec<(Value, &str)> = match entry["type"].as_str().unwrap() {
                "number" => vec![(json!("wide"), "number")],
                "integer" => vec![(json!("many"), "whole number"), (json!(2.5), "whole number")],
                "boolean" => vec![(json!(1), "true or false")],
                "text" => vec![(json!(7), "text")],
                "choice" => vec![(json!("zzz-no-such-name"), "one of")],
                _ => continue,
            };
            for (value, takes) in wrong {
                let error = set(&subject.doc, &subject.part, path, &value)
                    .expect_err(&format!("{} `{path}` = {value}", subject.label));
                assert_error(&error, ErrorCode::Invalid, path, &[path, takes, "was given"], &["part.describe"]);
                refusals += 1;
            }
            // Text of the wrong shape: the kind's own parsing refuses it, and the hint says what the pattern is.
            if entry["type"] == "text" && entry.get("pattern").is_some() {
                let error = set(&subject.doc, &subject.part, path, &json!("\u{1}no!"))
                    .expect_err(&format!("{} `{path}` accepted text that does not match its pattern", subject.label));
                assert_error(&error, ErrorCode::Invalid, path, &[path], &["text matching `^"]);
                patterned += 1;
            }
            let null = set(&subject.doc, &subject.part, path, &Value::Null);
            if entry["optional"] != true {
                assert_error(
                    &null.unwrap_err(),
                    ErrorCode::Invalid,
                    path,
                    &[path, "null was given"],
                    &["always has a value"],
                );
            }
        }
    }
    assert!(refusals > 400, "refused {refusals}");
    assert!(patterned > 5, "text with a pattern was refused {patterned} times");
}

#[test]
fn a_mistyped_path_names_the_nearest_valid_key() {
    let mut typos = 0;
    for subject in subjects() {
        // `hands` minus its `s` is another parameter (`hand`): that is not a typo.
        let listed = |p: &str| subject.description["parameters"].as_array().unwrap().iter().any(|e| path_of(e) == p);
        for entry in subject.description["parameters"].as_array().unwrap() {
            let path = path_of(entry);
            let (parent, last) = path.rsplit_once('/').map_or(("", path), |(p, l)| (p, l));
            if last.chars().count() < 4 {
                continue;
            }
            let typo = &last[..last.len() - 1];
            let wrong = if parent.is_empty() { typo.to_owned() } else { format!("{parent}/{typo}") };
            if listed(&wrong) {
                continue;
            }
            let error = set(&subject.doc, &subject.part, &wrong, &json!(0)).expect_err(&wrong);
            assert_error(&error, ErrorCode::Invalid, "path", &[&wrong], &[&format!("did you mean `{path}`?")]);
            typos += 1;
        }
        // A wrong first step is found before the later ones are looked at.
        if let Some(path) = leaves(&subject.description)
            .map(path_of)
            .find(|p| p.split('/').next().is_some_and(|f| f.len() > 4 && p.contains('/')))
        {
            let (first, rest) = path.split_once('/').unwrap();
            let wrong = format!("{}/{rest}", &first[..first.len() - 1]);
            if listed(&first[..first.len() - 1]) {
                continue;
            }
            let error = set(&subject.doc, &subject.part, &wrong, &json!(0)).expect_err(&wrong);
            assert_error(
                &error,
                ErrorCode::Invalid,
                "path",
                &[&first[..first.len() - 1]],
                &[&format!("did you mean `{first}`?")],
            );
        }
    }
    assert!(typos > 300, "tried {typos}");
}

#[test]
fn paths_through_a_value_or_naming_nothing_say_what_is_there() {
    let doc = project_with_probe("sample", &json!({"marks": [1.0, 2.0]}));
    let message = |path: &str| {
        let e = set(&doc, "probe", path, &json!(1)).unwrap_err();
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("path")), "{path}");
        format!("{} | {}", e.message, e.hint.unwrap())
    };
    assert!(message("width/depth").contains("`width` is a number and has no `depth` inside it | set `width` itself: it takes a number from 0.1 to 2 (default 0.5)"));
    assert!(message("marks/0").contains("`marks` is a list"), "a variable-length list of numbers is set whole");
    assert!(message("knobs/nose/level").contains("did you mean `knobs/ear`?"), "the entries of a map are suggested");
    assert!(
        message("knobs/nose").contains("to add an entry, set the map itself")
            || message("knobs/nose").contains("the parameters under `knobs` are: `ear`")
    );
    assert!(message("").contains("empty step"));
    assert!(message("width/").contains("empty step"));
    assert!(message("params/width").contains("the parameters of this kind are: `finish`, `glow`, `knobs`"));
}

#[test]
fn sample_values_of_every_shape_are_set_validated_and_undone() {
    let mut doc = project_with_probe("sample", &json!({}));
    let at = |doc: &MemDoc, path: &str| common::at(doc, &format!("characters/mock/parts/probe/params/{path}"));

    // A number, an integer, a name, text and a boolean.
    for (path, value) in [
        ("width", json!(1.5)),
        ("teeth", json!(5)),
        ("finish", json!("gloss")),
        ("tag", json!("zz")),
        ("glow", json!(true)),
    ] {
        round_trip(&mut doc, "part.set_param", set_args("probe", path, &value));
    }

    // An optional section is switched on by setting one of its members; the rest of it takes the kind's defaults.
    let on = round_trip(&mut doc, "part.set_param", set_args("probe", "trim/depth", &json!(0.75)));
    assert_eq!(on.output.as_ref().unwrap()["after"], json!(0.75));
    let mut with_trim = doc.clone();
    apply(&mut with_trim, "part.set_param", set_args("probe", "trim/depth", &json!(0.75)));
    assert_eq!(at(&with_trim, "trim"), Some(json!({"depth": 0.75, "edge": 0.1})), "{}", with_trim.root());
    // ... and off again with null.
    let off = round_trip(&mut with_trim, "part.set_param", set_args("probe", "trim", &Value::Null));
    assert_eq!(off.patch.changes.len(), 1);
    assert_eq!(off.patch.changes[0].after, None, "a removed parameter is removed from the file, not set to null");

    // A rule between two fields names the field at fault and the other.
    let mut trimmed = with_trim.clone();
    apply(&mut trimmed, "part.set_param", set_args("probe", "trim/depth", &json!(0.5)));
    let tie = refused(&trimmed, "part.set_param", set_args("probe", "trim/edge", &json!(0.9)));
    assert_error(
        &tie,
        ErrorCode::Invalid,
        "trim/edge",
        &["trim/edge: must not exceed the depth"],
        &["a number from 0 to 0.5"],
    );

    // An object sets the fields it names; a map gains an entry; a list is replaced whole.
    let merged = round_trip(&mut doc, "part.set_param", set_args("probe", "knobs", &json!({"nose": {"level": 0.9}})));
    let mut with_knob = doc.clone();
    apply(&mut with_knob, "part.set_param", set_args("probe", "knobs", &json!({"nose": {"level": 0.9}})));
    assert_eq!(at(&with_knob, "knobs"), Some(json!({"ear": {"level": 0.25}, "nose": {"level": 0.9}})));
    assert_eq!(merged.patch.changes.len(), 1, "the whole table is created (the file left it out) in one change");
    round_trip(&mut with_knob, "part.set_param", set_args("probe", "knobs/nose/level", &json!(0.1)));
    round_trip(&mut doc, "part.set_param", set_args("probe", "marks", &json!([1.0, -2.5])));
    round_trip(&mut doc, "part.set_param", set_args("probe", "pair", &json!([0.9, 0.1])));
    round_trip(&mut doc, "part.set_param", set_args("probe", "pair/1", &json!(0.3)));

    // Refusals.
    let teeth = refused(&doc, "part.set_param", set_args("probe", "teeth", &json!(6)));
    assert_error(
        &teeth,
        ErrorCode::Invalid,
        "teeth",
        &["teeth: 6 is out of range: use 2 to 5"],
        &["a whole number from 2 to 5 (default 3)"],
    );
    let radius = refused(&doc, "part.set_param", set_args("probe", "radius", &json!(0)));
    assert_error(
        &radius,
        ErrorCode::Invalid,
        "radius",
        &["radius: 0 must be greater than 0"],
        &["a number greater than 0"],
    );
    let whole = refused(&doc, "part.set_param", set_args("probe", "teeth", &json!(3.0)));
    assert_error(
        &whole,
        ErrorCode::Invalid,
        "teeth",
        &["`teeth` takes a whole number from 2 to 5 (default 3), but 3.0 was given"],
        &["without a decimal point"],
    );
    let finish = refused(&doc, "part.set_param", set_args("probe", "finish", &json!("glos")));
    assert_error(
        &finish,
        ErrorCode::Invalid,
        "finish",
        &["takes one of `matte`, `gloss` (default \"matte\")", "\"glos\" was given"],
        &["did you mean `gloss`?"],
    );
    let tag = refused(&doc, "part.set_param", set_args("probe", "tag", &json!("Capital")));
    assert_error(&tag, ErrorCode::Invalid, "tag", &["tag: use lowercase letters only"], &["text matching `^[a-z]+$`"]);
    let pair = refused(&doc, "part.set_param", set_args("probe", "pair/0", &json!(1.5)));
    assert_error(
        &pair,
        ErrorCode::Invalid,
        "pair/0",
        &["pair/0: 1.5 is out of range: use a number from 0 to 1"],
        &["a number from 0 to 1"],
    );
    let marks = refused(&doc, "part.set_param", set_args("probe", "marks", &json!([1.0, 50.0])));
    assert_error(&marks, ErrorCode::Invalid, "marks/1", &["marks/1: 50 is out of range"], &["part.describe"]);
    let missing_field = refused(&doc, "part.set_param", set_args("probe", "knobs", &json!({"nose": {"levl": 0.2}})));
    assert_error(&missing_field, ErrorCode::Invalid, "knobs", &["knobs: ", "unknown field `levl`"], &["part.describe"]);
}

#[test]
fn several_errors_are_all_listed_the_first_names_the_field() {
    let doc = project_with_probe("sample", &json!({}));
    let e = refused(
        &doc,
        "part.set_param",
        set_args("probe", "knobs", &json!({"a": {"level": 5.0}, "b": {"level": -1.0}})),
    );
    assert_eq!(e.field.as_deref(), Some("knobs/a/level"));
    assert!(
        e.message.contains("knobs/a/level: 5 is out of range")
            && e.message.contains("knobs/b/level: -1 is out of range"),
        "{}",
        e.message
    );
}

#[test]
fn edits_to_sparse_files_create_the_missing_tables_whole_and_leave_the_rest_alone() {
    // The fixture's body and head have no `params` at all, and the hair names only what differs from the defaults.
    let mut doc = project();
    let body = describe(&doc, "body");
    let leaf = leaves(&body).find(|e| e["type"] == "number" && e.get("maximum").is_some()).unwrap();
    let (path, new) = (path_of(leaf), json!(leaf["maximum"].as_f64().unwrap()));
    let planned = round_trip(&mut doc, "part.set_param", set_args("body", path, &new));
    assert_eq!(planned.patch.changes.len(), 1);
    assert_eq!(
        planned.patch.changes[0].path.to_string(),
        "characters/mock/parts/body/params",
        "the table is created whole"
    );
    assert_eq!(planned.patch.changes[0].before, None);

    let hair_before = stored(&doc, "hair");
    let planned =
        round_trip(&mut doc, "part.set_param", set_args("hair", "groups/a-cap/dynamics/simulate", &json!(true)));
    assert_eq!(planned.patch.changes.len(), 1);
    assert_eq!(planned.patch.changes[0].path.to_string(), "characters/mock/parts/hair/params/groups/a-cap/dynamics");
    assert_eq!(
        planned.patch.changes[0].after.as_ref().unwrap()["simulate"],
        json!(true),
        "the dynamics table the file left out is created with its normalised values"
    );
    assert_ne!(stored(&doc, "hair"), hair_before);

    // A section the file does have is edited in place.
    let planned =
        round_trip(&mut doc, "part.set_param", set_args("hair", "groups/c-curtain/dynamics/follow", &json!("head")));
    assert_eq!(planned.patch.changes.len(), 1);
    assert_eq!(
        planned.patch.changes[0].path.to_string(),
        "characters/mock/parts/hair/params/groups/c-curtain/dynamics/follow"
    );
}

#[test]
fn tagged_enums_and_mixed_names_are_followed_through_the_current_value() {
    let doc = project();
    let hair = describe(&doc, "hair");
    let paths: Vec<&str> = hair["parameters"].as_array().unwrap().iter().map(path_of).collect();
    assert!(paths.contains(&"groups/a-cap/generator/hairline_front_deg"), "the cap's own members");
    assert!(!paths.contains(&"groups/a-cap/generator/part_front_deg"), "not another generator's");
    assert!(paths.contains(&"groups/c-curtain/generator/length_front"));
    assert!(!paths.contains(&"groups/b-bangs/generator/sweep_deg"), "the fixture has no bangs group");
    let tag =
        hair["parameters"].as_array().unwrap().iter().find(|p| path_of(p) == "groups/a-cap/generator/type").unwrap();
    assert_eq!((tag["type"].as_str(), &tag["values"]), (Some("choice"), &json!(["cap"])));

    // `rng` is the name `forked` or an object: it is a choice that can also be set to the object form.
    let rng = hair["parameters"].as_array().unwrap().iter().find(|p| path_of(p) == "rng").unwrap();
    assert_eq!(
        (rng["type"].as_str(), &rng["values"], &rng["current"]),
        (Some("choice"), &json!(["forked"]), &json!("forked"))
    );
    let mut doc = doc;
    round_trip(&mut doc, "part.set_param", set_args("hair", "rng", &json!({"legacy_lcg": {"seed": 7}})));
    let legacy = describe(&doc, "hair");
    let seed = legacy["parameters"].as_array().unwrap().iter().find(|p| path_of(p) == "rng/legacy_lcg/seed").unwrap();
    assert_eq!((&seed["current"], &seed["minimum"], &seed["maximum"]), (&json!(7), &json!(1), &json!(2_147_483_646)));
    round_trip(&mut doc, "part.set_param", set_args("hair", "rng", &json!("forked")));
}

#[test]
fn unknown_characters_parts_and_kinds_are_refused_with_what_exists() {
    let doc = project();
    let e = refused(&doc, "part.set_param", json!({"character": "nobody", "part": "hair", "path": "x", "value": 1}));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::NotFound, Some("character")));
    let e = refused(&doc, "part.describe", json!({"character": "mock", "part": "wig"}));
    assert_eq!((e.code, e.field.as_deref()), (ErrorCode::NotFound, Some("part")));
    assert!(e.hint.unwrap().contains("`hair`"), "the parts the character has are listed");

    let orphan = project_with_probe("no-such-kind", &json!({}));
    for (op, args) in [
        ("part.describe", json!({"character": "mock", "part": "probe"})),
        ("part.set_param", set_args("probe", "x", &json!(1))),
    ] {
        let e = refused(&orphan, op, args);
        assert_eq!(e.code, ErrorCode::Unsupported, "{op}");
        assert!(
            e.message.contains("`no-such-kind`") && e.hint.unwrap().contains("`sample`"),
            "{op}: names the kind and the registered ones"
        );
    }
}

// ------------------------------------------------------------------------------------------------ properties

/// Every number of every subject: the parameter an edit may land on.
fn number_cases() -> &'static [(usize, String, bool)] {
    static CASES: OnceLock<Vec<(usize, String, bool)>> = OnceLock::new();
    CASES.get_or_init(|| {
        subjects()
            .iter()
            .enumerate()
            .flat_map(|(i, s)| {
                leaves(&s.description)
                    .filter(|e| is_number(e))
                    .map(move |e| (i, path_of(e).to_owned(), e["type"] == "integer"))
            })
            .collect()
    })
}

proptest! {
    // Failures are not persisted: the tests directory has no lib.rs or main.rs to anchor the regressions file to.
    #![proptest_config(ProptestConfig { cases: 96, failure_persistence: None, ..ProptestConfig::default() })]

    /// Whatever number is set on whatever parameter, the op accepts it exactly when the kind's own check does, and
    /// then the file holds what the check returned; otherwise it refuses with a field the check complained about.
    #[test]
    fn set_param_agrees_with_the_check_of_the_kind_on_any_number(
        pick in 0usize..100_000,
        real in prop_oneof![-1.0f64..1.0, -400.0f64..400.0, 0.0f64..0.5, proptest::num::f64::NORMAL | proptest::num::f64::ZERO],
        whole in -300i64..300,
    ) {
        let cases = number_cases();
        let (subject, path, integer) = &cases[pick % cases.len()];
        let subject = &subjects()[*subject];
        let value = if *integer { json!(whole) } else { json!(real) };
        let expected = oracle(subject, path, &value);
        match (expected, set(&subject.doc, &subject.part, path, &value)) {
            (Ok(norm), Ok(planned)) => {
                let mut doc = subject.doc.clone();
                doc.apply(&planned.patch).unwrap();
                prop_assert_eq!(normalised(subject, &doc), norm, "{} `{}` = {}", &subject.label, path, &value);
            }
            (Err(errors), Err(error)) => {
                prop_assert_eq!(error.code, ErrorCode::Invalid);
                let field = error.field.clone().unwrap_or_default();
                prop_assert!(
                    field == *path || errors.0.iter().any(|e| e.path == field),
                    "{} `{}` = {}: refused for `{}`, the check says {:?}", &subject.label, path, &value, field, errors
                );
            }
            (expected, got) => prop_assert!(false, "{} `{}` = {}: the check says {:?}, the op says {:?}", &subject.label, path, &value, expected, got),
        }
    }

    /// Set, then describe: the value is what describe reports as current, and the bounds describe reports are unchanged
    /// by an edit to another parameter.
    #[test]
    fn what_was_set_is_what_describe_reports(width in 0.1f64..2.0, glow in any::<bool>()) {
        let mut doc = project_with_probe("sample", &json!({}));
        apply(&mut doc, "part.set_param", set_args("probe", "width", &json!(width)));
        apply(&mut doc, "part.set_param", set_args("probe", "glow", &json!(glow)));
        let d = describe(&doc, "probe");
        prop_assert_eq!(&d["current"]["width"], &json!(width));
        prop_assert_eq!(&d["current"]["glow"], &json!(glow));
        let entry = d["parameters"].as_array().unwrap().iter().find(|p| path_of(p) == "width").unwrap().clone();
        prop_assert_eq!((&entry["minimum"], &entry["maximum"]), (&json!(0.1), &json!(2.0)));
    }
}

/// The JSON path of the first `null` in `value`, if any.
fn first_null(value: &Value, at: &str) -> Option<String> {
    match value {
        Value::Null => Some(at.to_owned()),
        Value::Array(items) => items.iter().enumerate().find_map(|(i, v)| first_null(v, &format!("{at}/{i}"))),
        Value::Object(map) => map.iter().find_map(|(k, v)| first_null(v, &format!("{at}/{k}"))),
        _ => None,
    }
}

#[test]
fn no_part_kind_defaults_or_check_output_holds_a_null() {
    for kind in kinds() {
        let id = (kind.meta)().id;
        let defaults = (kind.defaults)();
        assert_eq!(first_null(&defaults, id), None, "defaults of `{id}`: {defaults}");
        let checked = (kind.check)(&defaults).expect("a kind's defaults pass its own check");
        assert_eq!(first_null(&checked, id), None, "check output of `{id}`: {checked}");
        let empty = (kind.check)(&json!({})).expect("an empty parameter set passes the check");
        assert_eq!(first_null(&empty, id), None, "check of empty parameters of `{id}`: {empty}");
    }
}
