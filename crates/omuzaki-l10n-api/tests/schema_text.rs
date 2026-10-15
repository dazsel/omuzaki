//! The schema walk and the title rule (`schema_text`), on schemas generated the way `omuzaki-ops-api` generates them.
//! The fixture types exist only for their generated schemas, so their fields are never read.
#![allow(dead_code)]

use std::collections::BTreeMap;

use omuzaki_l10n_api::schema_text::{description, title, walk};
use schemars::{JsonSchema, Schema};
use serde_json::{Map, Value, json};

/// The schema settings of `omuzaki-ops-api`'s `schema_of`: draft 2020-12, every subschema inlined.
fn schema_of<T: JsonSchema>() -> Schema {
    schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<T>()
}

/// Every visited node as `(path, description)`, with the path's keys joined by `--` (the root is `""`).
fn visited<T: JsonSchema>() -> Vec<(String, Option<String>)> {
    visited_in(&schema_of::<T>())
}

fn visited_in(schema: &Schema) -> Vec<(String, Option<String>)> {
    let mut out = Vec::new();
    walk(schema, |path, node| out.push((path.join("--"), description(node).map(str::to_owned))));
    out
}

/// The same paths as `visited`, without descriptions.
fn paths<T: JsonSchema>() -> Vec<String> {
    visited::<T>().into_iter().map(|(path, _)| path).collect()
}

/// A visited node's `(path, title)`, where the title is derived from the path's last key.
fn titles<T: JsonSchema>() -> Vec<(String, String)> {
    let mut out = Vec::new();
    walk(&schema_of::<T>(), |path, node| {
        let key = path.last().copied().unwrap_or_default();
        out.push((path.join("--"), title(node, key)));
    });
    out
}

/// A tuning pass.
#[derive(JsonSchema)]
struct Tune {
    /// The blend.
    blend: f32,
    /// Nested settings.
    inner: Inner,
}

/// Inner settings.
#[derive(JsonSchema)]
struct Inner {
    /// Its size.
    max_size: u32,
    /// Its twin.
    twin_tails: bool,
}

#[test]
fn a_struct_visits_its_fields() {
    let got = visited::<Tune>();
    assert_eq!(
        got,
        vec![
            (String::new(), Some("A tuning pass.".to_owned())),
            ("blend".into(), Some("The blend.".to_owned())),
            ("inner".into(), Some("Nested settings.".to_owned())),
            ("inner--max_size".into(), Some("Its size.".to_owned())),
            ("inner--twin_tails".into(), Some("Its twin.".to_owned())),
        ]
    );
}

/// Easing mode.
#[derive(JsonSchema)]
enum Mode {
    /// Reaches the target at once.
    Instant,
    /// Reaches the target in a third of the time.
    Fast,
}

#[test]
fn a_unit_enum_with_docs_visits_each_variant() {
    assert_eq!(
        visited::<Mode>(),
        vec![
            (String::new(), Some("Easing mode.".to_owned())),
            ("Instant".into(), Some("Reaches the target at once.".to_owned())),
            ("Fast".into(), Some("Reaches the target in a third of the time.".to_owned())),
        ]
    );
}

/// A pick with no docs on its values.
#[derive(JsonSchema)]
enum Plain {
    Slow,
    Quick,
}

#[test]
fn a_plain_enum_visits_each_value_with_no_description() {
    assert_eq!(
        visited::<Plain>(),
        vec![
            (String::new(), Some("A pick with no docs on its values.".to_owned())),
            ("Slow".into(), None),
            ("Quick".into(), None)
        ]
    );
}

/// A shape, tagged by its kind.
#[derive(JsonSchema)]
#[serde(tag = "kind")]
enum Shape {
    /// A round one.
    Circle {
        /// The radius.
        radius: f32,
        /// The centre.
        centre: f32,
    },
    /// A boxy one.
    Square {
        /// The side.
        side: f32,
    },
}

#[test]
fn an_internally_tagged_enum_keys_by_its_tag_and_skips_the_tag() {
    assert_eq!(
        visited::<Shape>(),
        vec![
            (String::new(), Some("A shape, tagged by its kind.".to_owned())),
            ("Circle".into(), Some("A round one.".to_owned())),
            ("Circle--centre".into(), Some("The centre.".to_owned())),
            ("Circle--radius".into(), Some("The radius.".to_owned())),
            ("Square".into(), Some("A boxy one.".to_owned())),
            ("Square--side".into(), Some("The side.".to_owned())),
        ]
    );
}

/// Holds a count.
#[derive(JsonSchema)]
struct Count {
    /// The count.
    count: u32,
    /// The step.
    step: u32,
}

/// A pick, tagged by its variant name.
#[derive(JsonSchema)]
enum Pick {
    /// Holds a number.
    Num(Count),
    /// Holds a name.
    Named {
        /// The name.
        name: String,
    },
}

#[test]
fn an_externally_tagged_enum_keys_by_the_variant_name() {
    assert_eq!(
        visited::<Pick>(),
        vec![
            (String::new(), Some("A pick, tagged by its variant name.".to_owned())),
            ("Num".into(), Some("Holds a number.".to_owned())),
            ("Num--count".into(), Some("The count.".to_owned())),
            ("Num--step".into(), Some("The step.".to_owned())),
            ("Named".into(), Some("Holds a name.".to_owned())),
            ("Named--name".into(), Some("The name.".to_owned())),
        ]
    );
}

/// Holds optional, repeated and keyed inner settings.
#[derive(JsonSchema)]
struct Holder {
    /// Optional settings.
    opt: Option<Inner>,
    /// Repeated settings.
    list: Vec<Inner>,
    /// Keyed settings.
    map: BTreeMap<String, Inner>,
}

#[test]
fn option_vec_and_map_add_no_key() {
    assert_eq!(
        paths::<Holder>(),
        vec![
            "",
            "list",
            "list--max_size",
            "list--twin_tails",
            "map",
            "map--max_size",
            "map--twin_tails",
            "opt",
            "opt--max_size",
            "opt--twin_tails"
        ]
    );
}

/// Sizes by name.
#[derive(JsonSchema)]
struct Titled {
    /// Derived from the key.
    max_size: u32,
    /// Derived from the key too.
    twin_tails: bool,
    /// Explicit wins.
    #[schemars(title = "Maximum size")]
    limit: u32,
}

#[test]
fn titles_explicit_or_derived_from_the_key() {
    assert_eq!(
        titles::<Titled>(),
        vec![
            (String::new(), "Titled".to_owned()),
            ("limit".into(), "Maximum size".into()),
            ("max_size".into(), "Max size".into()),
            ("twin_tails".into(), "Twin tails".into()),
        ]
    );
}

#[test]
fn a_derived_title_keeps_a_capital_and_handles_variant_names() {
    let mut map = Map::new();
    map.insert("title".into(), Value::String("   ".into()));
    assert_eq!(title(&map, "Fast"), "Fast");
    assert_eq!(title(&map, "max-size"), "Max size");
    assert_eq!(title(&map, "a__b"), "A b");
}

#[test]
fn a_ref_is_not_followed() {
    let schema = Schema::try_from(json!({
        "$ref": "#/$defs/A",
        "$defs": { "A": { "type": "object", "properties": { "b": { "type": "integer", "description": "Hidden." } } } }
    }))
    .unwrap();
    assert_eq!(visited_in(&schema), vec![(String::new(), None)]);
}

#[test]
fn a_boolean_schema_is_not_visited() {
    assert_eq!(visited_in(&Schema::from(true)), Vec::new());
}

#[test]
fn description_is_trimmed_and_blank_is_none() {
    let padded = json!({ "description": "  Spaced.  " });
    let blank = json!({ "description": "   " });
    assert_eq!(description(padded.as_object().unwrap()), Some("Spaced."));
    assert_eq!(description(blank.as_object().unwrap()), None);
}

/// Holds an optional single-field struct.
#[derive(JsonSchema)]
struct OptionalSingle {
    /// The optional struct.
    inner: Option<Single>,
}

/// One field.
#[derive(JsonSchema)]
struct Single {
    /// The only field.
    #[schemars(title = "Only")]
    only: u32,
}

#[test]
fn an_option_of_a_single_field_struct_keeps_the_field() {
    assert_eq!(
        visited::<OptionalSingle>(),
        vec![
            (String::new(), Some("Holds an optional single-field struct.".to_owned())),
            ("inner".into(), Some("The optional struct.".to_owned())),
            ("inner--only".into(), Some("The only field.".to_owned())),
        ]
    );
    assert_eq!(titles::<OptionalSingle>().last(), Some(&("inner--only".to_owned(), "Only".to_owned())));
}

#[test]
fn an_any_of_with_a_null_branch_is_an_option_not_a_variant() {
    // The `anyOf` form of `Option<T>`, which schemars does not emit for a struct but which the rule must hold for.
    let schema = Schema::try_from(json!({
        "type": "object",
        "properties": {
            "inner": {
                "description": "The optional struct.",
                "anyOf": [
                    {
                        "type": "object",
                        "properties": { "only": { "title": "Only", "description": "The only field.", "type": "integer" } },
                        "required": ["only"]
                    },
                    { "type": "null" }
                ]
            }
        }
    }))
    .expect("a valid schema");
    assert_eq!(
        visited_in(&schema),
        vec![
            (String::new(), None),
            ("inner".into(), Some("The optional struct.".to_owned())),
            ("inner--only".into(), Some("The only field.".to_owned())),
        ]
    );
}

/// A pair of inner settings and a count.
#[derive(JsonSchema)]
struct Pair {
    /// The pair.
    pair: (Inner, u32),
}

#[test]
fn a_tuple_walks_its_elements_transparently() {
    assert_eq!(paths::<Pair>(), vec!["", "pair", "pair--max_size", "pair--twin_tails"],);
}

/// Entries by key, tagged by kind.
#[derive(JsonSchema)]
#[serde(tag = "kind")]
enum Keyed {
    /// Named entries.
    Entries(BTreeMap<String, Inner>),
}

#[test]
fn a_tagged_map_payload_keeps_its_values() {
    assert_eq!(
        visited::<Keyed>(),
        vec![
            (String::new(), Some("Entries by key, tagged by kind.".to_owned())),
            ("Entries".into(), Some("Named entries.".to_owned())),
            ("Entries--max_size".into(), Some("Its size.".to_owned())),
            ("Entries--twin_tails".into(), Some("Its twin.".to_owned())),
        ]
    );
}

/// A figure, tagged by its variant name.
#[derive(JsonSchema)]
enum Figure {
    /// A round one.
    Round {
        /// The radius.
        radius: f32,
    },
    /// A boxy one.
    Square {
        /// The side.
        side: f32,
    },
}

/// A flat figure with a count.
#[derive(JsonSchema)]
#[serde(tag = "kind")]
enum Flattened {
    /// The flat variant.
    Flat {
        /// The figure, flattened in.
        #[serde(flatten)]
        figure: Figure,
        /// A count.
        count: u32,
    },
}

#[test]
fn a_flattened_union_inside_a_tagged_variant_is_walked() {
    assert_eq!(
        paths::<Flattened>(),
        vec!["", "Flat", "Flat--count", "Flat--Round", "Flat--Round--radius", "Flat--Square", "Flat--Square--side"],
    );
}
