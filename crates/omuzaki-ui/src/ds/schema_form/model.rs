//! Reading a JSON Schema into the tree of fields a form draws (`shell.md` section 7, "Generated controls").
//!
//! The schema is read once, in [`Form::from_schema`]; drawing never looks at JSON again. Every decision about which
//! control a field gets is made here, so the table in the module docs is this file and nothing else.

use omuzaki_doc_api::Value;
use schemars::Schema;

use super::pattern::Pattern;
use super::show::FieldId;
use super::value::{FormValue, curve_from_json, integer};
use crate::ds::widgets::Curve;

/// How many Essentials a form shows before the rest move to Advanced (principle P5).
pub const MAX_ESSENTIALS: usize = 7;
/// The most options a segmented control holds; more become a dropdown.
pub const MAX_SEGMENTS: usize = 5;
/// How deep `$ref`s are followed before the field is given up as read-only (a schema that refers to itself).
const MAX_REF_DEPTH: usize = 16;
/// The range of a number field's open side: no bound the schema states is ever replaced by a smaller one.
pub(super) const OPEN: f64 = 1.0e300;
/// The two patterns `omuzaki_core::colour::Colour` can publish, which mark a string as a colour without an `x-ui`
/// hint: with and without the alpha pair.
const COLOUR_PATTERNS: [(&str, bool); 2] = [("^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$", true), ("^#[0-9a-fA-F]{6}$", false)];
/// Integers up to here are exact as floats, so an integer slider or number field (which edit a float) loses nothing;
/// an integer field that can hold more is typed as text and parsed exactly.
pub(super) const EXACT: i128 = 1 << 53;
/// The widest whole-number range a slider stands for: past it one pixel skips values, so a port or a seed is typed.
const SLIDER_SPAN: i128 = 1000;

/// Where a field is listed (`x-ui.level`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Level {
    /// Shown first and open; at most [`MAX_ESSENTIALS`] per form.
    #[default]
    Essentials,
    /// Collapsed behind a count badge.
    Advanced,
}

/// One value a choice can take.
#[derive(Clone, Debug)]
pub(super) struct Opt {
    pub(super) value: Value,
    pub(super) label: String,
}

/// What a string must satisfy. Lengths count characters, as JSON Schema's do.
#[derive(Clone, Debug, Default)]
pub(super) struct TextRule {
    pub(super) pattern: Option<Pattern>,
    pub(super) min_len: usize,
    pub(super) max_len: Option<usize>,
}

impl TextRule {
    /// Whether the length is within `minLength` and `maxLength`.
    pub(super) fn length_ok(&self, text: &str) -> bool {
        let len = text.chars().count();
        len >= self.min_len && self.max_len.is_none_or(|max| len <= max)
    }

    /// Whether `text` satisfies every assertion (runs the pattern matcher; the form caches that between frames).
    pub(super) fn is_valid(&self, text: &str) -> bool {
        self.length_ok(text) && self.pattern.as_ref().is_none_or(|p| p.is_match(text))
    }
}

/// What a field edits, and so which control draws it.
#[derive(Clone, Debug)]
pub(super) enum Kind {
    /// An `integer`, within the bounds the schema gives (inclusive, exclusive ones already moved in by one, and the
    /// range of a `format` such as `uint8` applied): see [`IntControl`].
    Int { min: Option<i128>, max: Option<i128>, control: IntControl },
    /// A `number`, with its bounds made inclusive (an exclusive one is moved to the next float inside): a
    /// `ParamSlider` when both exist and differ, else a `NumberField` that enforces the ones it has (both equal: the
    /// one value the field allows).
    Real { min: Option<f64>, max: Option<f64>, slider: bool },
    /// An enum, or an integer with few values: a `Segmented` up to [`MAX_SEGMENTS`], else a `Dropdown`.
    Choice(Vec<Opt>),
    /// A boolean: a `Toggle`.
    Toggle,
    /// A colour, written as `#rrggbb` (or `#rrggbbaa` when `alpha`): a `ColourField`.
    Colour { alpha: bool },
    /// A `number[]` of x, y pairs over the unit square (`x-ui.widget = "curve"`): a `CurveEditor`.
    Curve,
    /// A string, checked against its `pattern`, `minLength` and `maxLength`: a `TextField`.
    Text(TextRule),
    /// A nested object: a section of fields.
    Section(Vec<Node>),
    /// Any other shape (a union, a list, a free-form object, any value at all): a `TextField` holding the value as
    /// JSON, checked as it is typed, so every argument an op takes can be given here as it can over MCP.
    Json,
    /// A shape that can't be edited safely: a reference that doesn't resolve, or a rule the form can't check (a
    /// pattern it can't evaluate, an empty range). The row names the type and the value passes through untouched.
    ReadOnly,
}

/// Which control edits an integer field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IntControl {
    /// Both bounds, within the integers a float holds exactly: a `ParamSlider` stepping by whole numbers.
    Slider,
    /// Every value it allows is exact as a float: a `NumberField` stepping by whole numbers.
    Number,
    /// It allows values a float cannot hold (an `i64` or `u64` field): a `TextField` whose text is parsed as an
    /// integer, exactly, and refused with a message when it is not a whole number within the bounds.
    Exact,
}

/// One field of the form.
#[derive(Clone, Debug)]
pub(super) struct Node {
    /// The property name, which is the key in the value.
    pub(super) key: String,
    /// The segments from the root as a JSON Pointer (`/a~1b/c`, each key escaped with `~0` and `~1`): unique in the
    /// form, so it keys every control's state.
    pub(super) path: String,
    /// What the row says: the schema's `title`, else the key.
    pub(super) label: String,
    /// The field's place in the form, counted parents first ([`FieldId`]).
    pub(super) index: usize,
    /// The JSON Schema type, for the read-only row ("object", "array of string").
    pub(super) type_name: String,
    pub(super) kind: Kind,
    pub(super) level: Level,
    pub(super) group: Option<String>,
    /// `x-ui.unit`: what a number field's value is in ("mm", "°"), shown after it and accepted when typed.
    pub(super) unit: String,
    /// `x-ui.step`: how far an arrow key moves a number field (a whole number of at least one for an integer).
    pub(super) step: Option<f64>,
    /// `x-ui.decimals`: how many decimals a number field shows (none for an integer).
    pub(super) decimals: Option<usize>,
    /// The default as a typed value, when the schema gives one that fits the field. An optional field without one
    /// defaults to [`FormValue::Absent`], so its reset takes the value back out.
    pub(super) default: Option<FormValue>,
    /// The schema allows `null` (an `Option` in the op's arguments): the field starts unset and is left out of the
    /// JSON until the person sets it.
    pub(super) optional: bool,
}

/// A place in a level: one field, or a named group of fields.
#[derive(Clone, Debug)]
pub(super) enum Entry {
    Field(usize),
    Group { title: String, fields: Vec<usize> },
}

/// The fields of one level, in order, with the fields that share an `x-ui.group` gathered under it.
#[derive(Clone, Debug, Default)]
pub(super) struct Layout {
    pub(super) entries: Vec<Entry>,
    pub(super) count: usize,
}

/// A schema read into fields. Build it once (it is cheap, but not free) and keep it beside the [`FormValue`].
#[derive(Clone, Debug)]
pub struct Form {
    pub(super) root: Node,
    pub(super) essentials: Layout,
    pub(super) advanced: Layout,
    demoted: usize,
    /// Every field's JSON Pointer, by [`FieldId`].
    pointers: Vec<String>,
}

/// A schema node together with the `$ref` it points to, so an annotation written next to a `$ref` and one written
/// in the definition are both found (the node's own wins).
#[derive(Clone, Copy)]
struct View<'a> {
    own: &'a Value,
    target: &'a Value,
}

impl<'a> View<'a> {
    fn get(&self, key: &str) -> Option<&'a Value> {
        self.own.get(key).or_else(|| self.target.get(key))
    }

    fn ui(&self, key: &str) -> Option<&'a Value> {
        self.own.get("x-ui").and_then(|u| u.get(key)).or_else(|| self.target.get("x-ui").and_then(|u| u.get(key)))
    }
}

/// Follows `$ref` (and a one-element `allOf`, which older schema generators wrap a reference in) to the definition.
fn view<'a>(root: &'a Value, schema: &'a Value) -> Option<View<'a>> {
    let mut target = schema;
    for _ in 0..MAX_REF_DEPTH {
        if let Some(reference) = target.get("$ref").and_then(Value::as_str) {
            target = root.pointer(reference.strip_prefix('#')?)?;
        } else if let Some([only]) = target.get("allOf").and_then(Value::as_array).map(Vec::as_slice) {
            target = only;
        } else {
            return Some(View { own: schema, target });
        }
    }
    None
}

impl Form {
    /// Reads `schema`, the shape `OpInfo.schema` and `part.describe` carry. Never fails: a field it cannot map
    /// becomes a read-only row that names its type.
    pub fn from_schema(schema: &Schema) -> Self {
        let root_value = schema.as_value();
        let mut root = read_node(root_value, root_value, "", "");
        let mut pointers = Vec::new();
        number(&mut root, &mut pointers);
        let (mut essentials, mut advanced, mut demoted) = (Layout::default(), Layout::default(), 0);
        if let Kind::Section(children) = &mut root.kind {
            let mut shown = 0;
            for child in children.iter_mut() {
                if child.level == Level::Essentials {
                    if shown == MAX_ESSENTIALS {
                        child.level = Level::Advanced;
                        demoted += 1;
                    } else {
                        shown += 1;
                    }
                }
            }
            essentials = layout(children, Level::Essentials);
            advanced = layout(children, Level::Advanced);
        }
        Self { root, essentials, advanced, demoted, pointers }
    }

    /// The fields no control edits, as `(JSON Pointer, type)`: they show as read-only rows and pass their value through.
    pub fn read_only(&self) -> Vec<(String, String)> {
        fn walk(node: &Node, out: &mut Vec<(String, String)>) {
            match &node.kind {
                Kind::ReadOnly => out.push((node.path.clone(), node.type_name.clone())),
                Kind::Section(children) => children.iter().for_each(|child| walk(child, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }

    /// Where `field` is in the value, as a JSON Pointer (`/cranium/half_width`; the root is the empty string).
    pub fn pointer(&self, field: FieldId) -> Option<&str> {
        self.pointers.get(field.0).map(String::as_str)
    }

    /// The field at the JSON Pointer `pointer`, if the form has one there.
    pub fn field(&self, pointer: &str) -> Option<FieldId> {
        self.pointers.iter().position(|p| p == pointer).map(FieldId)
    }

    /// The part of `value` that `field` edits, for a host that sends one field at a time (the inspector sends each
    /// edit as one `part.set_param`). `None` when `value` was not made from this form.
    pub fn field_value<'v>(&self, value: &'v FormValue, field: FieldId) -> Option<&'v FormValue> {
        find(&self.root, value, field.0)
    }

    /// How many fields asked for Essentials beyond the cap of [`MAX_ESSENTIALS`] and were listed under Advanced
    /// instead. A schema author wants this to be zero.
    pub const fn demoted(&self) -> usize {
        self.demoted
    }

    /// The value with every field at its default (`x-ui.default`, else the schema's `default`, else the first
    /// option, the lower bound or the nearest allowed value to zero, `false`, or an empty string). A field with no
    /// control yet (colours and curves, arrays) has no value.
    pub fn default_value(&self) -> FormValue {
        self.root.default_value()
    }

    /// Reads a value in the schema's own JSON shape (an op's arguments). A key that is missing takes its default;
    /// one that does not fit its field is kept as it is, read-only, so nothing is changed behind the person's back.
    pub fn value_from_json(&self, json: &Value) -> FormValue {
        self.root.value_from_json(json)
    }

    /// How many text fields currently break their `pattern`. A host that runs the form's value as a request waits
    /// for zero.
    pub fn invalid_count(&self, value: &FormValue) -> usize {
        count_invalid(&self.root, value)
    }
}

/// Numbers `node` and its fields parents first, recording each one's pointer.
fn number(node: &mut Node, pointers: &mut Vec<String>) {
    node.index = pointers.len();
    pointers.push(node.path.clone());
    if let Kind::Section(children) = &mut node.kind {
        for child in children {
            number(child, pointers);
        }
    }
}

fn find<'v>(node: &Node, value: &'v FormValue, index: usize) -> Option<&'v FormValue> {
    if node.index == index {
        return Some(value);
    }
    match (&node.kind, value) {
        (Kind::Section(children), FormValue::Object(values)) => {
            children.iter().zip(values).find_map(|(child, (_, v))| find(child, v, index))
        }
        _ => None,
    }
}

fn count_invalid(node: &Node, value: &FormValue) -> usize {
    match (&node.kind, value) {
        (Kind::Text(rule), FormValue::Text(text)) => usize::from(!rule.is_valid(text)),
        (Kind::Json, FormValue::Json(text)) => usize::from(serde_json::from_str::<Value>(text).is_err()),
        (Kind::Section(children), FormValue::Object(values)) => {
            children.iter().zip(values).map(|(child, (_, v))| count_invalid(child, v)).sum()
        }
        _ => 0,
    }
}

fn layout(children: &[Node], level: Level) -> Layout {
    let mut out = Layout::default();
    for (i, child) in children.iter().enumerate().filter(|(_, c)| c.level == level) {
        out.count += 1;
        match &child.group {
            None => out.entries.push(Entry::Field(i)),
            Some(name) => {
                let existing = out.entries.iter_mut().find_map(|e| match e {
                    Entry::Group { title, fields } if title == name => Some(fields),
                    _ => None,
                });
                match existing {
                    Some(fields) => fields.push(i),
                    None => out.entries.push(Entry::Group { title: name.clone(), fields: vec![i] }),
                }
            }
        }
    }
    out
}

fn read_node(root: &Value, schema: &Value, key: &str, parent_path: &str) -> Node {
    if let Some(inner) = non_null(root, schema) {
        let mut node = read_node(root, &inner, key, parent_path);
        node.optional = true;
        node.default = node.default.or(Some(FormValue::Absent));
        return node;
    }
    let path = if key.is_empty() && parent_path.is_empty() {
        String::new()
    } else {
        format!("{parent_path}/{}", key.replace('~', "~0").replace('/', "~1"))
    };
    let Some(view) = view(root, schema) else {
        return read_only(key, &path, schema, "unresolved reference");
    };
    let label =
        view.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()).map_or_else(|| words(key), str::to_owned);
    let (kind, type_name) = read_kind(root, &view, &path);
    let level = match view.ui("level").and_then(Value::as_str).map(str::to_ascii_lowercase).as_deref() {
        Some("advanced") => Level::Advanced,
        _ => Level::Essentials,
    };
    let mut node = Node {
        key: key.to_owned(),
        path,
        label,
        index: 0,
        type_name,
        kind,
        level,
        group: view.ui("group").and_then(Value::as_str).map(str::to_owned),
        unit: view.ui("unit").and_then(Value::as_str).unwrap_or_default().to_owned(),
        step: view.ui("step").and_then(Value::as_f64).filter(|s| s.is_finite() && *s > 0.0),
        decimals: view.ui("decimals").and_then(Value::as_u64).and_then(|d| usize::try_from(d).ok()),
        default: None,
        optional: false,
    };
    if let Kind::Int { .. } = node.kind {
        // A whole-number field moves by whole numbers and shows none of the decimals a schema may ask for.
        node.step = Some(node.step.map_or(1.0, |s| s.round().max(1.0)));
        node.decimals = Some(0);
    }
    let declared = view.ui("default").or_else(|| view.get("default"));
    node.default = declared.and_then(|json| node.fitted_value(json));
    node
}

/// A field's key as words, for a schema that gives it no title: `character` reads "Character" and `vmc_port` "VMC
/// port", never the raw key a person would read as code.
fn words(key: &str) -> String {
    crate::ds::text::key_words(key)
}

fn read_only(key: &str, path: &str, schema: &Value, type_name: &str) -> Node {
    Node {
        key: key.to_owned(),
        path: path.to_owned(),
        index: 0,
        label: schema
            .get("title")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
            .map_or_else(|| words(key), str::to_owned),
        type_name: type_name.to_owned(),
        kind: Kind::ReadOnly,
        level: Level::Essentials,
        group: None,
        unit: String::new(),
        step: None,
        decimals: None,
        default: None,
        optional: false,
    }
}

fn read_kind(root: &Value, view: &View<'_>, path: &str) -> (Kind, String) {
    let unsupported = |what: String| (Kind::Json, what);
    if let Some(options) = view.get("enum").and_then(Value::as_array) {
        let opts: Vec<Opt> = options.iter().map(|v| Opt { value: v.clone(), label: option_label(v, None) }).collect();
        return if opts.is_empty() { unsupported("enum".to_owned()) } else { (Kind::Choice(opts), "enum".to_owned()) };
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(arms) = view.get(key).and_then(Value::as_array) {
            return match const_options(root, arms) {
                Some(opts) => (Kind::Choice(opts), "enum".to_owned()),
                None => unsupported(describe_union(root, arms)),
            };
        }
    }
    let types: Vec<&str> = match view.get("type") {
        Some(Value::String(t)) => vec![t.as_str()],
        Some(Value::Array(list)) => list.iter().filter_map(Value::as_str).collect(),
        _ => return unsupported("unknown".to_owned()),
    };
    let [single] = types.as_slice() else { return unsupported(types.join(" | ")) };
    match *single {
        "boolean" => (Kind::Toggle, "boolean".to_owned()),
        "number" | "integer" => read_number(view, *single == "integer"),
        "string" => read_colour(view).unwrap_or_else(|| read_string(view)),
        "object" => match view.get("properties").and_then(Value::as_object) {
            Some(props) if !props.is_empty() => {
                let children = props.iter().map(|(k, s)| read_node(root, s, k, path)).collect();
                (Kind::Section(children), "object".to_owned())
            }
            // An op that takes no arguments: a form with no fields.
            _ if path.is_empty() => (Kind::Section(Vec::new()), "object".to_owned()),
            _ => unsupported("object".to_owned()),
        },
        "array" => read_array(root, view),
        other => unsupported(other.to_owned()),
    }
}

fn read_number(view: &View<'_>, whole: bool) -> (Kind, String) {
    let name = if whole { "integer" } else { "number" };
    // What the form cannot enforce is not guessed at.
    if let Some(key) = ["multipleOf", "exclusiveMinimum", "exclusiveMaximum"]
        .into_iter()
        .find(|k| view.get(k).is_some_and(|v| *k == "multipleOf" || v.is_boolean()))
    {
        return (Kind::ReadOnly, format!("{name} with {key}"));
    }
    if whole { (read_int(view), name.to_owned()) } else { (read_real(view), name.to_owned()) }
}

/// The tighter of a bound written as `minimum`/`maximum` and one written as `exclusiveMinimum`/`exclusiveMaximum`.
fn tighter<T: PartialOrd>(lower: bool, a: Option<T>, b: Option<T>) -> Option<T> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if (a > b) == lower { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn read_real(view: &View<'_>) -> Kind {
    let limit = |lower: bool| {
        let (inclusive, exclusive) =
            if lower { ("minimum", "exclusiveMinimum") } else { ("maximum", "exclusiveMaximum") };
        let read = |key: &str| view.get(key).and_then(Value::as_f64).filter(|b| b.is_finite());
        let beyond = read(exclusive).map(|b| if lower { b.next_up() } else { b.next_down() });
        tighter(lower, read(inclusive), beyond)
    };
    let (min, max) = (limit(true), limit(false));
    // Equal inclusive bounds allow one value, a choice of one (as an integer's do); only a range with nothing in it
    // (the lower bound above the upper, or an exclusive bound at the other's value) has no control.
    match (min, max) {
        (Some(lo), Some(hi)) if lo > hi => Kind::ReadOnly,
        #[allow(clippy::float_cmp, reason = "equal bounds are the case being found")]
        (Some(lo), Some(hi)) if lo == hi => {
            let value = super::value::number(lo);
            Kind::Choice(vec![Opt { label: option_label(&value, None), value }])
        }
        (Some(_), Some(_)) => Kind::Real { min, max, slider: true },
        _ => Kind::Real { min, max, slider: false },
    }
}

fn read_int(view: &View<'_>) -> Kind {
    let limit = |lower: bool| {
        let (inclusive, exclusive) =
            if lower { ("minimum", "exclusiveMinimum") } else { ("maximum", "exclusiveMaximum") };
        let read = |key: &str, beyond: bool| -> Option<i128> {
            let v = view.get(key)?;
            let step = if beyond { if lower { 1 } else { -1 } } else { 0 };
            if let Some(n) = v.as_i64().map(i128::from).or_else(|| v.as_u64().map(i128::from)) {
                return Some(n + step);
            }
            // A bound written as a float: the nearest whole number inside it.
            let f = v.as_f64().filter(|f| f.is_finite() && f.abs() < 1.0e30)?;
            #[allow(clippy::cast_possible_truncation, reason = "finite and below 1e30, which an i128 holds")]
            let n = if lower { f.ceil() } else { f.floor() } as i128;
            #[allow(clippy::float_cmp, reason = "whole numbers written in a schema compare exactly")]
            let on_the_bound = f == f.trunc();
            Some(if beyond && on_the_bound { n + step } else { n })
        };
        tighter(lower, read(inclusive, false), read(exclusive, true))
    };
    let format = view.get("format").and_then(Value::as_str).and_then(format_range);
    let (min, max) =
        (tighter(true, limit(true), format.map(|f| f.0)), tighter(false, limit(false), format.map(|f| f.1)));
    let exact = |n: Option<i128>| n.is_some_and(|n| (-EXACT..=EXACT).contains(&n));
    let control = if exact(min) && exact(max) { IntControl::Number } else { IntControl::Exact };
    let Some((lo, hi)) = min.zip(max) else { return Kind::Int { min, max, control } };
    if lo > hi {
        return Kind::ReadOnly;
    }
    if hi - lo < MAX_SEGMENTS as i128 {
        let opts = (lo..=hi).map(|n| Opt { value: integer(n), label: n.to_string() }).collect();
        return Kind::Choice(opts);
    }
    // A slider only for a range the schema states, and narrow enough to hit each value: the range of a bare `uint32`
    // is no parameter's, and a port from 1024 to 65535 is a number a person types.
    let stated = limit(true).is_some() && limit(false).is_some() && hi - lo <= SLIDER_SPAN;
    Kind::Int { min, max, control: if control == IntControl::Number && stated { IntControl::Slider } else { control } }
}

/// The range of an integer `format` as `schemars` writes it for Rust's integer types, so a `u8` field refuses 256
/// without the schema spelling the bounds out.
fn format_range(format: &str) -> Option<(i128, i128)> {
    let bits = |b: u32| (-(1i128 << (b - 1)), (1i128 << (b - 1)) - 1);
    let ubits = |b: u32| (0, (1i128 << b) - 1);
    Some(match format {
        "int8" => bits(8),
        "int16" => bits(16),
        "int32" => bits(32),
        "int64" => bits(64),
        "uint8" => ubits(8),
        "uint16" => ubits(16),
        "uint32" => ubits(32),
        "uint64" | "uint" => ubits(64),
        _ => return None,
    })
}

/// A string that holds a colour: `x-ui.widget` or `format` says `colour` (or `color`), or the pattern is one that
/// `Colour` publishes. `None` for any other string.
fn read_colour(view: &View<'_>) -> Option<(Kind, String)> {
    let named = |v: Option<&Value>| matches!(v.and_then(Value::as_str), Some("colour" | "color"));
    let pattern = view.get("pattern").and_then(Value::as_str);
    let published = pattern.and_then(|p| COLOUR_PATTERNS.iter().find(|(known, _)| *known == p));
    let alpha = match (published, pattern) {
        (Some((_, alpha)), _) => *alpha,
        (None, None) if named(view.ui("widget")) || named(view.get("format")) => false,
        // A colour hint over a pattern this field cannot promise to satisfy is not taken as a colour.
        _ => return None,
    };
    Some((Kind::Colour { alpha }, "colour".to_owned()))
}

fn read_string(view: &View<'_>) -> (Kind, String) {
    // Assertions the form cannot enforce make the field read-only, so nothing invalid can be sent from it.
    if let Some(key) =
        ["format", "contentEncoding", "contentMediaType", "contentSchema"].into_iter().find(|k| view.get(k).is_some())
    {
        return (Kind::ReadOnly, format!("string with {key}"));
    }
    let length = |key: &str| view.get(key).map(|v| v.as_u64().and_then(|n| usize::try_from(n).ok()));
    let (Some(min_len), Some(max_len)) =
        (length("minLength").unwrap_or(Some(0)), length("maxLength").map_or(Some(None), |m| m.map(Some)))
    else {
        return (Kind::ReadOnly, "string with an unsupported length".to_owned());
    };
    if max_len.is_some_and(|max| max < min_len) {
        return (Kind::ReadOnly, "string with an empty length range".to_owned());
    }
    let pattern = match view.get("pattern").and_then(Value::as_str).map(Pattern::parse) {
        None => None,
        Some(Some(pattern)) => Some(pattern),
        Some(None) => return (Kind::ReadOnly, "string with an unsupported pattern".to_owned()),
    };
    (Kind::Text(TextRule { pattern, min_len, max_len }), "string".to_owned())
}

fn read_array(root: &Value, view: &View<'_>) -> (Kind, String) {
    let item_type = view
        .get("items")
        .and_then(|items| self::view(root, items))
        .and_then(|v| v.get("type").and_then(Value::as_str).map(str::to_owned));
    let name = format!("array of {}", item_type.as_deref().unwrap_or("any"));
    if view.ui("widget").and_then(Value::as_str) == Some("curve") && item_type.as_deref() == Some("number") {
        return (Kind::Curve, name);
    }
    (Kind::Json, name)
}

/// The schema of a field that also allows `null` (an `Option`), without the `null`: `type: ["string", "null"]`, or an
/// `anyOf` or `oneOf` of one schema and `{"type": "null"}`. The wrapper's own title, description, default and `x-ui`
/// carry over to the inner schema where it lacks them. `None` when the field doesn't allow `null`.
fn non_null(root: &Value, schema: &Value) -> Option<Value> {
    let is_null = |arm: &Value| view(root, arm).and_then(|v| v.get("type")).and_then(Value::as_str) == Some("null");
    if let Some(Value::Array(types)) = schema.get("type") {
        let others: Vec<&Value> = types.iter().filter(|t| t.as_str() != Some("null")).collect();
        if let ([only], true) = (others.as_slice(), others.len() < types.len()) {
            let mut inner = schema.clone();
            inner["type"] = (*only).clone();
            return Some(inner);
        }
        return None;
    }
    for key in ["anyOf", "oneOf"] {
        let Some(arms) = schema.get(key).and_then(Value::as_array) else { continue };
        let others: Vec<&Value> = arms.iter().filter(|arm| !is_null(arm)).collect();
        let ([only], true) = (others.as_slice(), others.len() < arms.len()) else { return None };
        let mut inner = (*only).clone();
        if let (Value::Object(inner_map), Value::Object(outer)) = (&mut inner, schema) {
            for (k, v) in outer {
                if k != key {
                    inner_map.entry(k.clone()).or_insert_with(|| v.clone());
                }
            }
        }
        return Some(inner);
    }
    None
}

/// The options of a `oneOf` or `anyOf` whose every arm is a `const`, which is how documented enum variants are written.
fn const_options(root: &Value, arms: &[Value]) -> Option<Vec<Opt>> {
    let opts: Vec<Opt> = arms
        .iter()
        .map(|arm| {
            let arm = view(root, arm)?;
            let value = arm.get("const")?;
            Some(Opt { value: value.clone(), label: option_label(value, arm.get("title").and_then(Value::as_str)) })
        })
        .collect::<Option<_>>()?;
    (!opts.is_empty()).then_some(opts)
}

fn option_label(value: &Value, title: Option<&str>) -> String {
    match (title, value) {
        (Some(title), _) if !title.is_empty() => title.to_owned(),
        (_, Value::String(text)) => text.clone(),
        (_, other) => other.to_string(),
    }
}

fn describe_union(root: &Value, arms: &[Value]) -> String {
    let names: Vec<String> = arms
        .iter()
        .map(|arm| {
            view(root, arm)
                .and_then(|v| v.get("type").map(|t| t.as_str().map_or_else(|| t.to_string(), str::to_owned)))
                .unwrap_or_else(|| "any".to_owned())
        })
        .collect();
    names.join(" | ")
}

impl Node {
    /// The natural value of the field when the schema names no default.
    pub(super) fn default_value(&self) -> FormValue {
        match &self.default {
            Some(default) => default.clone(),
            None => self.natural_value(),
        }
    }

    /// The value a field takes when the person first sets it: its natural value, ignoring an optional field's unset
    /// default.
    pub(super) fn natural_value(&self) -> FormValue {
        if let Some(default) = self.default.as_ref().filter(|d| **d != FormValue::Absent) {
            return default.clone();
        }
        match &self.kind {
            Kind::Int { min, max, control } => FormValue::Integer(match (control, min) {
                (IntControl::Slider, Some(min)) => *min,
                _ => 0.max(min.unwrap_or(i128::MIN)).min(max.unwrap_or(i128::MAX)),
            }),
            Kind::Real { min, max, slider } => FormValue::Number(match (slider, min) {
                (true, Some(min)) => *min,
                _ => 0f64.max(min.unwrap_or(-OPEN)).min(max.unwrap_or(OPEN)),
            }),
            Kind::Choice(opts) => {
                opts.first().map_or(FormValue::Absent, |o| FormValue::Choice { index: 0, value: o.value.clone() })
            }
            Kind::Toggle => FormValue::Bool(false),
            Kind::Colour { .. } => FormValue::Colour(omuzaki_core::colour::Colour::BLACK),
            Kind::Curve => FormValue::Curve(Curve::linear()),
            Kind::Text(_) => FormValue::Text(String::new()),
            // An empty list or object is a value; any other shape starts empty, so it is invalid until typed.
            Kind::Json if self.type_name.starts_with("array") => FormValue::Json("[]".to_owned()),
            Kind::Json if self.type_name == "object" => FormValue::Json("{}".to_owned()),
            Kind::Json => FormValue::Json(String::new()),
            Kind::Section(children) => {
                FormValue::Object(children.iter().map(|c| (c.key.clone(), c.default_value())).collect())
            }
            Kind::ReadOnly => FormValue::Absent,
        }
    }

    pub(super) fn value_from_json(&self, json: &Value) -> FormValue {
        if let Kind::Section(children) = &self.kind {
            let Some(map) = json.as_object() else { return FormValue::Raw(json.clone()) };
            let fields = children
                .iter()
                .map(|c| (c.key.clone(), map.get(&c.key).map_or_else(|| c.default_value(), |v| c.value_from_json(v))))
                .collect();
            return FormValue::Object(fields);
        }
        self.fitted_value(json).unwrap_or_else(|| FormValue::Raw(json.clone()))
    }

    /// `json` as this field's typed value, or `None` when it does not fit (wrong type, out of range, not an option).
    fn fitted_value(&self, json: &Value) -> Option<FormValue> {
        if self.optional && json.is_null() {
            return Some(FormValue::Absent);
        }
        match (&self.kind, json) {
            (Kind::Int { min, max, .. }, Value::Number(n)) => {
                let whole = n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)).or_else(|| {
                    n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() <= EXACT as f64).map(|f| f as i128)
                })?;
                (min.is_none_or(|m| whole >= m) && max.is_none_or(|m| whole <= m)).then_some(FormValue::Integer(whole))
            }
            (Kind::Real { min, max, .. }, Value::Number(n)) => {
                let f = exact_f64(n)?;
                (min.is_none_or(|m| f >= m) && max.is_none_or(|m| f <= m)).then_some(FormValue::Number(f))
            }
            (Kind::Section(_), Value::Object(_)) => Some(self.value_from_json(json)),
            (Kind::Choice(opts), _) => opts
                .iter()
                .position(|o| same_json(&o.value, json))
                .map(|index| FormValue::Choice { index, value: json.clone() }),
            (Kind::Toggle, Value::Bool(on)) => Some(FormValue::Bool(*on)),
            (Kind::Colour { alpha }, Value::String(text)) => {
                let colour: omuzaki_core::colour::Colour = text.parse().ok()?;
                // A colour with alpha in a field that has none would lose it when edited.
                (*alpha || colour.a == u8::MAX).then_some(FormValue::Colour(colour))
            }
            (Kind::Curve, Value::Array(numbers)) => curve_from_json(numbers).map(FormValue::Curve),
            (Kind::Text(_), Value::String(text)) => Some(FormValue::Text(text.clone())),
            (Kind::Json, json) => Some(FormValue::Json(json.to_string())),
            _ => None,
        }
    }
}

/// Whether two JSON values are equal as JSON Schema's `enum` and `const` compare them: numbers by value (`1` is
/// `1.0`), everything else structurally.
fn same_json(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            let whole = |n: &serde_json::Number| n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from));
            match (whole(x), whole(y)) {
                (Some(x), Some(y)) => x == y,
                #[allow(clippy::float_cmp, reason = "values the schema wrote compare exactly")]
                _ => exact_f64(x).zip(exact_f64(y)).is_some_and(|(x, y)| x == y),
            }
        }
        _ => a == b,
    }
}

/// `n` as a float, or `None` when it is an integer a float cannot hold exactly: a `number` field edits a float, and
/// the value must not change just by being read into it.
fn exact_f64(n: &serde_json::Number) -> Option<f64> {
    let f = n.as_f64()?;
    // The float of an integer is whole, so it holds the integer exactly when it converts back to it.
    let exact = match (n.as_i64(), n.as_u64()) {
        (Some(i), _) => i128::from(i) == float_to_int(f),
        (None, Some(u)) => i128::from(u) == float_to_int(f),
        (None, None) => true,
    };
    exact.then_some(f)
}

/// A whole float as an integer, saturating; only compared with the integer it came from.
#[allow(clippy::cast_possible_truncation, reason = "saturating, and only compared with the integer it came from")]
const fn float_to_int(f: f64) -> i128 {
    f as i128
}

#[cfg(test)]
mod tests {
    use super::{Form, Kind, Node};
    use schemars::Schema;
    use serde_json::json;

    fn paths(node: &Node, out: &mut Vec<String>) {
        out.push(node.path.clone());
        if let Kind::Section(children) = &node.kind {
            for c in children {
                paths(c, out);
            }
        }
    }

    #[test]
    fn field_ids_cannot_collide() {
        let schema = Schema::try_from(json!({"type": "object", "properties": {
            "a.b": {"type": "boolean"},
            "a": {"type": "object", "properties": {"b": {"type": "boolean"}}},
            "x/y": {"type": "boolean"},
            "x": {"type": "object", "properties": {"y": {"type": "boolean"}}},
            "~": {"type": "boolean"},
            "~0": {"type": "boolean"}
        }}))
        .unwrap_or_else(|_| panic!("a schema"));
        let form = Form::from_schema(&schema);
        let mut all = Vec::new();
        paths(&form.root, &mut all);
        let unique: std::collections::BTreeSet<_> = all.iter().collect();
        assert_eq!(unique.len(), all.len(), "{all:?}");
        assert!(all.contains(&"/a.b".to_owned()) && all.contains(&"/a/b".to_owned()), "{all:?}");
        assert!(all.contains(&"/x~1y".to_owned()) && all.contains(&"/x/y".to_owned()), "{all:?}");
    }
}
