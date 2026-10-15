//! Reading a JSON Schema into the tree of fields a form draws (`shell.md` section 7, "Generated controls").
//!
//! The schema is read once, in [`Form::from_schema`]; drawing never looks at JSON again. Every decision about which
//! control a field gets is made here, so the table in the module docs is this file and nothing else.

use omuzaki_doc_api::Value;
use schemars::Schema;

use super::pattern::Pattern;
use super::value::{FormValue, integer};

/// How many Essentials a form shows before the rest move to Advanced (principle P5).
pub const MAX_ESSENTIALS: usize = 7;
/// The most options a segmented control holds; more become a dropdown.
pub const MAX_SEGMENTS: usize = 5;
/// How deep `$ref`s are followed before the field is given up as read-only (a schema that refers to itself).
const MAX_REF_DEPTH: usize = 16;
/// The range of a number field's open side: no bound the schema states is ever replaced by a smaller one.
pub(super) const OPEN: f64 = 1.0e300;
/// Integers up to here are exact as floats, so an integer slider (which edits a float) loses nothing.
const EXACT: i128 = 1 << 53;

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
    /// An `integer`, within the bounds the schema gives (inclusive, exclusive ones already moved in by one): a
    /// `ParamSlider` when both exist and are exact as floats, else a `NumberField` that enforces the ones it has.
    Int { min: Option<i128>, max: Option<i128>, slider: bool },
    /// A `number`, with its bounds made inclusive (an exclusive one is moved to the next float inside): a
    /// `ParamSlider` when both exist, else a `NumberField` that enforces the one it has.
    Real { min: Option<f64>, max: Option<f64>, slider: bool },
    /// An enum, or an integer with few values: a `Segmented` up to [`MAX_SEGMENTS`], else a `Dropdown`.
    Choice(Vec<Opt>),
    /// A boolean: a `Toggle`.
    Toggle,
    /// A string, checked against its `pattern`, `minLength` and `maxLength`: a `TextField`.
    Text(TextRule),
    /// A nested object: a section of fields.
    Section(Vec<Node>),
    /// A shape no control handles. The row names the type and the value passes through untouched.
    ReadOnly,
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
    /// The JSON Schema type, for the read-only row ("object", "array of string").
    pub(super) type_name: String,
    pub(super) kind: Kind,
    pub(super) level: Level,
    pub(super) group: Option<String>,
    /// The default as a typed value, when the schema gives one that fits the field.
    pub(super) default: Option<FormValue>,
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
        Self { root, essentials, advanced, demoted }
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

fn count_invalid(node: &Node, value: &FormValue) -> usize {
    match (&node.kind, value) {
        (Kind::Text(rule), FormValue::Text(text)) => usize::from(!rule.is_valid(text)),
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
    let path = if key.is_empty() && parent_path.is_empty() {
        String::new()
    } else {
        format!("{parent_path}/{}", key.replace('~', "~0").replace('/', "~1"))
    };
    let Some(view) = view(root, schema) else {
        return read_only(key, &path, schema, "unresolved reference");
    };
    let label = view.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()).unwrap_or(key).to_owned();
    let (kind, type_name) = read_kind(root, &view, &path);
    let level = match view.ui("level").and_then(Value::as_str).map(str::to_ascii_lowercase).as_deref() {
        Some("advanced") => Level::Advanced,
        _ => Level::Essentials,
    };
    let mut node = Node {
        key: key.to_owned(),
        path,
        label,
        type_name,
        kind,
        level,
        group: view.ui("group").and_then(Value::as_str).map(str::to_owned),
        default: None,
    };
    let declared = view.ui("default").or_else(|| view.get("default"));
    node.default = declared.and_then(|json| node.fitted_value(json));
    node
}

fn read_only(key: &str, path: &str, schema: &Value, type_name: &str) -> Node {
    Node {
        key: key.to_owned(),
        path: path.to_owned(),
        label: schema.get("title").and_then(Value::as_str).filter(|t| !t.is_empty()).unwrap_or(key).to_owned(),
        type_name: type_name.to_owned(),
        kind: Kind::ReadOnly,
        level: Level::Essentials,
        group: None,
        default: None,
    }
}

fn read_kind(root: &Value, view: &View<'_>, path: &str) -> (Kind, String) {
    let unsupported = |what: String| (Kind::ReadOnly, what);
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
        "string" => read_string(view),
        "object" => match view.get("properties").and_then(Value::as_object) {
            Some(props) if !props.is_empty() => {
                let children = props.iter().map(|(k, s)| read_node(root, s, k, path)).collect();
                (Kind::Section(children), "object".to_owned())
            }
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
    match (min, max) {
        (Some(min), Some(max)) if min >= max => Kind::ReadOnly,
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
    let (min, max) = (limit(true), limit(false));
    let Some((lo, hi)) = min.zip(max) else { return Kind::Int { min, max, slider: false } };
    if lo > hi {
        return Kind::ReadOnly;
    }
    if hi - lo < MAX_SEGMENTS as i128 {
        let opts = (lo..=hi).map(|n| Opt { value: integer(n), label: n.to_string() }).collect();
        return Kind::Choice(opts);
    }
    Kind::Int { min, max, slider: lo >= -EXACT && hi <= EXACT }
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
    (Kind::ReadOnly, format!("array of {}", item_type.as_deref().unwrap_or("any")))
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
        if let Some(default) = &self.default {
            return default.clone();
        }
        match &self.kind {
            Kind::Int { min, max, slider } => FormValue::Integer(match (slider, min) {
                (true, Some(min)) => *min,
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
            Kind::Text(_) => FormValue::Text(String::new()),
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
        match (&self.kind, json) {
            (Kind::Int { min, max, .. }, Value::Number(n)) => {
                let whole = n.as_i64().map(i128::from).or_else(|| n.as_u64().map(i128::from)).or_else(|| {
                    n.as_f64().filter(|f| f.fract() == 0.0 && f.abs() <= EXACT as f64).map(|f| f as i128)
                })?;
                (min.is_none_or(|m| whole >= m) && max.is_none_or(|m| whole <= m)).then_some(FormValue::Integer(whole))
            }
            (Kind::Real { min, max, .. }, Value::Number(n)) => {
                let n = n.as_f64().filter(|n| min.is_none_or(|m| *n >= m) && max.is_none_or(|m| *n <= m))?;
                Some(FormValue::Number(n))
            }
            (Kind::Section(_), Value::Object(_)) => Some(self.value_from_json(json)),
            (Kind::Choice(opts), _) => {
                opts.iter().position(|o| &o.value == json).map(|index| FormValue::Choice { index, value: json.clone() })
            }
            (Kind::Toggle, Value::Bool(on)) => Some(FormValue::Bool(*on)),
            (Kind::Text(_), Value::String(text)) => Some(FormValue::Text(text.clone())),
            _ => None,
        }
    }
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
