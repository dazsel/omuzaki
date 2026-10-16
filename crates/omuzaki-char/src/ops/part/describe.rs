//! `part.describe`: what a part's parameters are and what each one accepts. The same parameter model is what
//! `part.set_param` edits against, so the two cannot disagree about which paths exist.
//!
//! The model (`Params`) walks the kind's JSON Schema together with the part's current values and lists every node as
//! a `Param`, parents before children, keys in alphabetical order (the order of the schema's keys depends on a
//! `serde_json` feature, so it is never relied on). The schema gives each node its type, allowed names, description and
//! unit; the values decide which branch of a tagged enum, and which entries of a map or list, are there to list.
//!
//! **Ranges are probed, not declared.** A kind's validator keeps its bounds as code (`Check::range`), so the schema
//! has none (tracked as G10). The range of a number is found by asking the kind's own `check` whether it accepts
//! values, with every other parameter as it is now: a bisection from the current value towards the far end of the
//! type. The answer is exact, since validators compare against literal bounds, and it follows the validator when a
//! bound changes. Two limits: a rule that ties two parameters together (a chin point that must stay below the stretch
//! top) shows up as a bound that moves with the other one, and a parameter whose section is absent (an optional section
//! that is off) has no value to probe from, so it reports none.
//!
//! **The cost is linear.** A probe checks a copy of the part that leaves out what the number does not depend on: the
//! other entries of the maps on the way to it, and the list items more than one away from it (so a "at least two
//! points" rule still holds). A lone value that names an entry by key is kept with it, and lists of such names (a
//! streak's `groups`) are stripped of the keys of the entries left out, so the references do not grow the probe. The
//! check then costs one entry rather than the whole part, one scratch copy per number is written into by every probe
//! instead of being cloned per probe, and entries are found by key, not by walking the map. That assumes the entries of
//! a map do not constrain one another, which cannot be proved of a kind's `check`: so for each parameter that entries
//! or list items share, the found bounds of all of them are set at once and the kind must accept the whole part, and a
//! kind that refuses (a sum over the entries) has that parameter probed in the whole part instead. A rule tying two
//! different parameters of different entries is not found this way, nor is one between list items more than one
//! apart. If the kind refuses the smaller copy, the whole part is probed. A part whose stored parameters fail the check
//! is described from the defaults with the stored values laid over them, so its own entries are listed; a number only
//! the part holds has no default to probe from and reports no range. A stored tag that names no variant is still
//! listed, with every variant, so it can be set.

use std::cell::OnceCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::num::FpCategory;

use omuzaki_char_api::{CharId, PartDef, PartId, PartKindRegistration, find_kind};
use omuzaki_doc_api::Value;
use omuzaki_face_api::ParamErrors;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, json};

use super::list_kinds;
use crate::ops::character::access::{self, list};

/// What the answer's `ranges` field says, so an agent reading a missing bound knows what it means.
const RANGES_NOTE: &str = "found by trying values against the kind's own validation with every other parameter as \
                           it is now; a bound that is missing means the validator imposes none, and a rule that ties \
                           two parameters together shows as a bound that moves with the other one";

/// Describes a part's parameters: its kind, and for every parameter its path, type, description, default, current value
/// and what it accepts. Numbers carry their legal range (`minimum` / `maximum`, or `exclusiveMinimum` /
/// `exclusiveMaximum` when the edge itself is refused; a missing bound means none), names carry their allowed
/// `values`, text may carry a `pattern`, and a `unit` appears when the kind declares one. Sections and lists you can
/// descend into are listed as entries without values, followed by their members. `schema` is the kind's JSON Schema,
/// `defaults` are what a new part starts from and `current` is what the part holds now (defaults filled in). Give a
/// parameter's `path` to `part.set_param`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartDescribe {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part to describe.
    pub part: PartId,
}

impl Op for PartDescribe {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.describe",
            summary: "Describe a part's kind and parameters: path, type, range or allowed values, default, current \
                      value and what each is for.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let part = access::part(&def, &self.character, &self.part, "part")?;
        let kind = kind_of(&self.part, part)?;
        let params = Params::new(kind, &part.params)?;
        let meta = (kind.meta)();
        let mut answer = json!({
            "character": self.character,
            "part": self.part,
            "kind": meta.id,
            "summary": meta.summary,
            "category": meta.category,
            "requires": meta.requires,
            "valid": params.current.is_ok(),
            "parameters": params.entries.iter().map(|p| params.describe(p)).collect::<Vec<_>>(),
            "schema": params.schema,
            "defaults": params.defaults,
            "current": params.shown(),
            "ranges": RANGES_NOTE,
        });
        if let (Err(errors), Some(fields)) = (&params.current, answer.as_object_mut()) {
            let problems: Vec<String> = errors.0.iter().map(ToString::to_string).collect();
            fields.insert("problems".to_owned(), json!(problems));
        }
        Ok(Plan::answer(answer))
    }
}

register_op!(PartDescribe);

// ------------------------------------------------------------------------------------------------ the parameter model

/// The registration of the kind `part` is made by.
///
/// # Errors
/// `unsupported` naming the registered kinds: a project saved by a build with another set of generators.
pub(crate) fn kind_of(id: &PartId, part: &PartDef) -> Result<&'static PartKindRegistration, OpError> {
    find_kind(part.kind.as_str()).ok_or_else(|| {
        OpError::unsupported(format!("part `{id}` has kind `{}`, which no generator in this build provides", part.kind))
            .hint(format!(
                "the registered kinds are: {}; part.list_kinds describes them",
                list(list_kinds::registered().iter().map(|m| m.id))
            ))
    })
}

/// What a parameter holds.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ty {
    /// A floating-point number.
    Number,
    /// A whole number.
    Integer,
    /// `true` or `false`.
    Boolean,
    /// Free text (which may have a pattern).
    Text,
    /// One of these names. A kind may allow other forms too (an object); only the names are listed.
    Choice(Vec<String>),
    /// A list of values.
    List,
    /// Entries by name (hair groups).
    Map,
    /// A section of named parameters.
    Section,
    /// A value the schema does not describe (a recursive type): `check` decides what is allowed.
    Any,
}

impl Ty {
    /// The word `part.describe` shows.
    fn name(&self) -> &'static str {
        match self {
            Ty::Number => "number",
            Ty::Integer => "integer",
            Ty::Boolean => "boolean",
            Ty::Text => "text",
            Ty::Choice(_) => "choice",
            Ty::List => "list",
            Ty::Map => "map",
            Ty::Section => "section",
            Ty::Any => "any",
        }
    }
}

/// One node of a part's parameters: a value, or a section, list or map with its members listed after it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Param {
    /// Where, `/`-separated from the part's parameters: `cranium/half_width`, `groups/a-cap/generator/count`,
    /// `centre/0`. The same syntax as `ParamError::path`.
    pub(crate) path: String,
    /// What it holds.
    pub(crate) ty: Ty,
    /// The doc comment of the field.
    description: Option<String>,
    /// The unit, when the kind declares one (`x-ui.unit` in the field's schema, see [`unit_of`]).
    unit: Option<String>,
    /// The pattern text must match.
    pattern: Option<String>,
    /// Whether the value can be removed: the schema allows `null` (which here means "not set"), or it is an entry of a
    /// map (a hair group), which `null` deletes.
    pub(crate) optional: bool,
    /// Whether this is the tag of a tagged enum, whose names are the variants: setting it switches the variant.
    pub(crate) tag: bool,
    /// The length of a list that has one fixed (a pair of positions), whose items cannot be left out of a probe.
    fixed: Option<u64>,
    /// Whether its members are listed after it. A container that is not expanded is a value of its own (a list of
    /// numbers, an empty map) and is set whole.
    expanded: bool,
}

/// A part's parameters: its kind's schema and defaults, what the part holds, and the nodes listed from them.
pub(crate) struct Params {
    kind: &'static PartKindRegistration,
    schema: Value,
    /// The kind's defaults, without the `null`s of absent options.
    defaults: Value,
    /// The stored parameters checked and normalised by the kind (defaults filled in, `null`s dropped), or what is wrong
    /// with them.
    pub(crate) current: Result<Value, ParamErrors>,
    /// The parameters as the document holds them, which may leave out anything that has a default.
    pub(crate) stored: Value,
    /// Every node, parents before children.
    entries: Vec<Param>,
    /// Where each path is in `entries`, so a lookup does not scan the part.
    index: HashMap<String, usize>,
    /// For each map (by path) the entries other parts of the tree name by key (a streak's `groups`), which a probe keeps
    /// beside the entry it is about so the kind's own cross-references still hold.
    named: HashMap<String, Vec<String>>,
    /// The base with every list of names that are map keys (a streak's `groups`) stripped of them, so a probe that leaves
    /// entries out does not keep, copy and check a reference to each.
    lean: Value,
    /// The range of every number, found when first asked for (see [`Self::ranges`]).
    ranges: OnceCell<HashMap<String, Range>>,
}

impl Params {
    /// The parameters of a part of `kind` that holds `stored`. A part whose stored parameters fail the kind's check is
    /// still described so it can be repaired: from the defaults with the stored values laid over them, so the entries
    /// only the part holds (a hair group the defaults lack) are listed and reachable.
    ///
    /// # Errors
    /// `internal` if the schema cannot be written as JSON, which would be a bug in the kind.
    pub(crate) fn new(kind: &'static PartKindRegistration, stored: &Value) -> Result<Self, OpError> {
        let schema = serde_json::to_value((kind.schema)())
            .map_err(|e| OpError::internal(format!("writing the schema of kind `{}` failed: {e}", (kind.meta)().id)))?;
        let defaults = (kind.defaults)();
        let current = (kind.check)(stored);
        let mut entries = Vec::new();
        if let Ok(normalised) = &current {
            members(&schema, Some(normalised), "", None, &mut entries);
        } else {
            let mut overlay = defaults.clone();
            assign(&mut overlay, stored.clone());
            members(&schema, Some(&overlay), "", None, &mut entries);
        }
        let index = entries.iter().enumerate().map(|(i, p)| (p.path.clone(), i)).collect();
        let base = current.as_ref().unwrap_or(&defaults);
        let lean = without_key_lists(base, &entries);
        let named = named_entries(&lean, &entries);
        Ok(Self {
            kind,
            schema,
            defaults,
            current,
            stored: stored.clone(),
            entries,
            index,
            named,
            lean,
            ranges: OnceCell::new(),
        })
    }

    /// The values to show as current: the normalised ones, or the stored ones when they do not pass the check.
    pub(crate) fn shown(&self) -> &Value {
        self.current.as_ref().unwrap_or(&self.stored)
    }

    /// The node at `path`, if there is one.
    pub(crate) fn entry(&self, path: &str) -> Option<&Param> {
        self.index.get(path).and_then(|&i| self.entries.get(i))
    }

    /// The node `path` names.
    ///
    /// # Errors
    /// `invalid` (field `path`) for a path that names nothing: the first step that does not exist is reported with the
    /// nearest existing sibling and the list of them, and a step below a value says what that value is.
    pub(crate) fn resolve(&self, path: &str) -> Result<&Param, OpError> {
        if path.split('/').any(str::is_empty) {
            return Err(OpError::invalid("path", format!("`{path}` is not a parameter path: it has an empty step"))
                .hint("write the steps joined by `/`, for example `cranium/half_width`; part.describe lists them"));
        }
        let mut so_far = String::new();
        let mut found = None;
        for step in path.split('/') {
            let parent = so_far.clone();
            if !so_far.is_empty() {
                so_far.push('/');
            }
            so_far.push_str(step);
            found = Some(self.entry(&so_far).ok_or_else(|| self.unknown(&parent, step, path))?);
        }
        found.ok_or_else(|| OpError::invalid("path", "the path names no parameter"))
    }

    /// The error for the step `step` below `parent` not existing, in the path `whole`.
    fn unknown(&self, parent: &str, step: &str, whole: &str) -> OpError {
        let join = |name: &str| if parent.is_empty() { name.to_owned() } else { format!("{parent}/{name}") };
        if let Some(holder) = self.entry(parent).filter(|h| !h.expanded) {
            let more =
                if holder.ty == Ty::Map { "; to add an entry, set the map itself to an object holding it" } else { "" };
            return OpError::invalid(
                "path",
                format!("`{parent}` is a {} and has no `{step}` inside it", holder.ty.name()),
            )
            .hint(format!("set `{parent}` itself: it takes {}{more}", self.takes(holder)));
        }
        let siblings: Vec<&str> = self.entries.iter().filter_map(|p| directly_below(&p.path, parent)).collect();
        let place = if parent.is_empty() { "of this kind".to_owned() } else { format!("under `{parent}`") };
        let suggestion = nearest(step, siblings.iter().copied())
            .map(|s| format!("did you mean `{}`? ", join(s)))
            .unwrap_or_default();
        let missing = join(step);
        let message = if missing == whole {
            format!("there is no parameter `{whole}`")
        } else {
            format!("there is no parameter `{whole}`: `{missing}` does not exist")
        };
        OpError::invalid("path", message).hint(format!("{suggestion}the parameters {place} are: {}", list(siblings)))
    }

    /// The parameters probes start from: the normalised ones, or the defaults when the stored ones fail the check.
    fn base(&self) -> &Value {
        self.current.as_ref().unwrap_or(&self.defaults)
    }

    /// The tree to probe the number at `path` in, and where the number is in it. Every probe writes the same slot, so
    /// nothing needs restoring between them. The entries of the maps on the way, other than the one the path goes
    /// through and the ones a lone value names by key, and the items of the lists on the way more than one away from
    /// the item the path goes through, are left out: the check then costs what one entry costs rather than what the
    /// whole part does. If the kind refuses the smaller tree, the whole of `base` is used.
    fn scratch(&self, path: &str, prune: bool) -> (Value, String) {
        if !prune {
            return (self.base().clone(), path.to_owned());
        }
        let steps: Vec<&str> = path.split('/').collect();
        let mut mapped = Vec::with_capacity(steps.len());
        let pruned = self.spine(&self.lean, "", &steps, &mut mapped);
        if (self.kind.check)(&pruned).is_ok() { (pruned, mapped.join("/")) } else { (self.base().clone(), path.to_owned()) }
    }

    /// A copy of `node`, at `prefix`, without what `scratch` leaves out; `mapped` collects the steps of the path as
    /// they are in the copy (a list item's position moves when the items before it are left out).
    fn spine(&self, node: &Value, prefix: &str, steps: &[&str], mapped: &mut Vec<String>) -> Value {
        let Some((step, rest)) = steps.split_first() else { return node.clone() };
        let child = |name: &str| if prefix.is_empty() { name.to_owned() } else { format!("{prefix}/{name}") };
        let holder = self.entry(prefix);
        match node {
            Value::Object(fields) if holder.is_some_and(|p| p.ty == Ty::Map) => {
                let named = self.named.get(prefix).map_or(&[][..], Vec::as_slice);
                let keys = std::iter::once(*step).chain(named.iter().map(String::as_str));
                mapped.push((*step).to_owned());
                Value::Object(
                    keys.filter_map(|key| {
                        let item = fields.get(key)?;
                        let item = if key == *step { self.spine(item, &child(key), rest, mapped) } else { item.clone() };
                        Some((key.to_owned(), item))
                    })
                    .collect(),
                )
            }
            Value::Object(fields) => {
                mapped.push((*step).to_owned());
                Value::Object(
                    fields
                        .iter()
                        .map(|(key, item)| {
                            let item = if key == step { self.spine(item, &child(key), rest, mapped) } else { item.clone() };
                            (key.clone(), item)
                        })
                        .collect(),
                )
            }
            Value::Array(items) => {
                let at = step.parse::<usize>().ok().filter(|i| *i < items.len());
                let window = holder.is_some_and(|p| p.ty == Ty::List && p.fixed.is_none());
                let (lo, hi) = match at {
                    Some(i) if window => (i.saturating_sub(1), (i + 1).min(items.len() - 1)),
                    _ => (0, items.len().saturating_sub(1)),
                };
                mapped.push(at.map_or_else(|| (*step).to_owned(), |i| (i - lo).to_string()));
                Value::Array(
                    items
                        .iter()
                        .enumerate()
                        .skip(lo)
                        .take(hi + 1 - lo)
                        .map(|(i, item)| {
                            if at == Some(i) { self.spine(item, &child(&i.to_string()), rest, mapped) } else { item.clone() }
                        })
                        .collect(),
                )
            }
            other => other.clone(),
        }
    }

    /// Whether the kind accepts `value` at `path` of `probe`, which is left holding it.
    fn accepts(&self, probe: &mut Value, path: &str, value: Value) -> bool {
        set_at(probe, path, Some(value), false).is_ok() && (self.kind.check)(probe).is_ok()
    }

    /// The legal range of the number at `param`, found by probing (see the module docs). `None` for anything that is
    /// not a number or has no value to probe from.
    fn range(&self, param: &Param) -> Option<Range> {
        match self.ranges.get() {
            Some(found) => found.get(&param.path).cloned(),
            // One number (a refusal's hint) is probed in the whole part when it sits in a map entry: it is a single
            // search, and the whole part is what the entries' independence is tested against.
            None => self.range_in(param, self.template(&param.path).is_none()),
        }
    }

    /// The range of every number of the part. Each is probed in its own map entry (see [`Self::scratch`]), which assumes
    /// the entries of a map do not constrain one another. That cannot be proved of a kind's `check`, so it is tested
    /// where it would show: for every parameter that map entries share (`groups/*/length`), all the entries' values are
    /// set to their found bounds at once, and the kind must accept the whole part. A kind that refuses (a sum over the
    /// entries, an ordering between them) has that parameter probed in the whole part instead, at the cost of the
    /// whole part per probe. A rule that ties two different parameters of different entries is not found this way.
    fn ranges(&self) -> &HashMap<String, Range> {
        self.ranges.get_or_init(|| {
            let numbers = self.entries.iter().filter(|p| matches!(p.ty, Ty::Number | Ty::Integer));
            let mut found: HashMap<String, Range> =
                numbers.clone().filter_map(|p| Some((p.path.clone(), self.range_in(p, true)?))).collect();
            let mut shared: BTreeMap<String, Vec<&Param>> = BTreeMap::new();
            for param in numbers {
                if let Some(template) = self.template(&param.path) {
                    shared.entry(template).or_default().push(param);
                }
            }
            for members in shared.values().filter(|m| m.len() > 1) {
                if self.entries_agree(members, &found) {
                    continue;
                }
                for param in members {
                    match self.range_in(param, false) {
                        Some(range) => found.insert(param.path.clone(), range),
                        None => found.remove(&param.path),
                    };
                }
            }
            found
        })
    }

    /// Whether the kind accepts the whole part with every one of `members` (the same parameter in each entry of a map)
    /// at its found upper bound, and again at its lower bound. A bound the kind refuses at the edge itself (an open
    /// one) is left out of the test.
    fn entries_agree(&self, members: &[&Param], found: &HashMap<String, Range>) -> bool {
        [true, false].into_iter().all(|upper| {
            let mut probe = self.base().clone();
            for param in members {
                let Some(range) = found.get(&param.path) else { continue };
                let (edge, open) = if upper { (&range.max, range.open_max) } else { (&range.min, range.open_min) };
                if let (Some(edge), false) = (edge, open)
                    && set_at(&mut probe, &param.path, Some(Value::Number(edge.clone())), false).is_err()
                {
                    return false;
                }
            }
            (self.kind.check)(&probe).is_ok()
        })
    }

    /// `path` with the key of every map entry and the position of every list item on the way written `*`, or `None` if
    /// it goes through neither.
    fn template(&self, path: &str) -> Option<String> {
        let (mut prefix, mut out, mut keyed, mut after_map) = (String::new(), String::new(), false, false);
        for step in path.split('/') {
            if !out.is_empty() {
                out.push('/');
            }
            if after_map {
                out.push('*');
                keyed = true;
            } else {
                out.push_str(step);
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(step);
            after_map = self.entry(&prefix).is_some_and(|p| p.ty == Ty::Map || (p.ty == Ty::List && p.fixed.is_none()));
        }
        keyed.then_some(out)
    }

    /// [`Self::range`], probing the pruned tree or (`prune` false) the whole of the part.
    fn range_in(&self, param: &Param, prune: bool) -> Option<Range> {
        let here = at(self.base(), &param.path)?;
        let (mut probe, slot) = self.scratch(&param.path, prune);
        match param.ty {
            Ty::Number => Some(Range::of_floats(here.as_f64()?, |x| {
                Number::from_f64(x).is_some_and(|n| self.accepts(&mut probe, &slot, Value::Number(n)))
            })),
            Ty::Integer => Some(Range::of_integers(here.as_i64()?, |i| {
                self.accepts(&mut probe, &slot, Value::Number(Number::from(i)))
            })),
            _ => None,
        }
    }

    /// What `param` takes, as a phrase for messages and hints: its range, or its allowed names, and its default.
    pub(crate) fn takes(&self, param: &Param) -> String {
        let mut text = match &param.ty {
            Ty::Number | Ty::Integer => {
                let whole = param.ty == Ty::Integer;
                self.range(param).map_or_else(|| range_free(whole), |r| r.says(whole))
            }
            Ty::Boolean => "true or false".to_owned(),
            Ty::Text => param.pattern.as_ref().map_or_else(|| "text".to_owned(), |p| format!("text matching `{p}`")),
            Ty::Choice(names) => format!("one of {}", list(names)),
            Ty::List => "a list".to_owned(),
            Ty::Map | Ty::Section => "an object naming the fields to change".to_owned(),
            Ty::Any => "any value the kind accepts".to_owned(),
        };
        if let Some(default) = at(&self.defaults, &param.path).filter(|_| !param.expanded) {
            let default = match default {
                Value::Number(n) => plain(n, param.ty == Ty::Integer),
                other => other.to_string(),
            };
            // Writing to a String cannot fail.
            let _ = write!(text, " (default {default})");
        }
        text
    }

    /// The answer's entry for `param`.
    fn describe(&self, param: &Param) -> Value {
        let mut entry = Map::new();
        entry.insert("path".to_owned(), json!(param.path));
        entry.insert("type".to_owned(), json!(param.ty.name()));
        for (key, text) in [("description", &param.description), ("unit", &param.unit), ("pattern", &param.pattern)] {
            if let Some(text) = text {
                entry.insert(key.to_owned(), json!(text));
            }
        }
        if let Ty::Choice(names) = &param.ty {
            entry.insert("values".to_owned(), json!(names));
        }
        if param.optional {
            entry.insert("optional".to_owned(), Value::Bool(true));
        }
        if !param.expanded {
            for (key, root) in [("default", &self.defaults), ("current", self.shown())] {
                if let Some(value) = at(root, &param.path) {
                    entry.insert(key.to_owned(), value.clone());
                }
            }
            if let Some(range) = self.ranges().get(&param.path) {
                range.write(&mut entry);
            }
        }
        Value::Object(entry)
    }
}

/// `base` with every list of text stripped of the texts that are keys of a map entry (a streak's `groups` names hair
/// groups): a probe that leaves entries out would otherwise keep a reference to each, and copying and checking those
/// grows with the part. A kind that needs such a list to be non-empty refuses the stripped tree, and the probe then
/// uses the whole part.
fn without_key_lists(base: &Value, entries: &[Param]) -> Value {
    fn strip(value: &Value, keys: &HashSet<&str>) -> Value {
        match value {
            Value::Array(items) if items.iter().all(Value::is_string) => Value::Array(
                items.iter().filter(|i| i.as_str().is_none_or(|text| !keys.contains(text))).cloned().collect(),
            ),
            Value::Array(items) => Value::Array(items.iter().map(|item| strip(item, keys)).collect()),
            Value::Object(fields) => Value::Object(fields.iter().map(|(k, v)| (k.clone(), strip(v, keys))).collect()),
            other => other.clone(),
        }
    }
    let keys: HashSet<&str> = entries
        .iter()
        .filter(|p| p.ty == Ty::Map)
        .filter_map(|p| match at(base, &p.path)? {
            Value::Object(held) => Some(held.keys().map(String::as_str)),
            _ => None,
        })
        .flatten()
        .collect();
    if keys.is_empty() { base.clone() } else { strip(base, &keys) }
}

/// For every map in `entries`, the keys of its entries that some string value in `base` equals (a streak naming the
/// group it paints): the entries a probe has to keep so such references still resolve.
fn named_entries(base: &Value, entries: &[Param]) -> HashMap<String, Vec<String>> {
    fn strings<'a>(value: &'a Value, into: &mut HashSet<&'a str>) {
        match value {
            Value::String(text) => {
                into.insert(text);
            }
            Value::Array(items) => items.iter().for_each(|item| strings(item, into)),
            Value::Object(fields) => fields.values().for_each(|item| strings(item, into)),
            _ => {}
        }
    }
    let mut values = HashSet::new();
    strings(base, &mut values);
    entries
        .iter()
        .filter(|p| p.ty == Ty::Map)
        .filter_map(|p| {
            let Value::Object(held) = at(base, &p.path)? else { return None };
            let mut named: Vec<String> = held.keys().filter(|key| values.contains(key.as_str())).cloned().collect();
            named.sort_unstable();
            (!named.is_empty()).then(|| (p.path.clone(), named))
        })
        .collect()
}

/// The last step of `path` when it lies directly below `parent` (`""` is the root).
fn directly_below<'a>(path: &'a str, parent: &str) -> Option<&'a str> {
    let rest = if parent.is_empty() { path } else { path.strip_prefix(parent)?.strip_prefix('/')? };
    (!rest.contains('/')).then_some(rest)
}

// ------------------------------------------------------------------------------------------------ reading the schema

/// Lists the members of `alt`, a schema that has some (an object's properties, a map's entries, the items of a list
/// that is fixed in size or holds sections), after the parent at `path`. `described` is the description to fall back
/// on for members that have none. Returns whether it listed any.
fn members(alt: &Value, current: Option<&Value>, path: &str, described: Option<&str>, out: &mut Vec<Param>) -> bool {
    let child = |key: &str| if path.is_empty() { key.to_owned() } else { format!("{path}/{key}") };
    if let Some(properties) = alt.get("properties").and_then(Value::as_object) {
        let mut fields: Vec<(&String, &Value)> = properties.iter().collect();
        fields.sort_unstable_by_key(|(key, _)| *key);
        for (key, schema) in &fields {
            node(schema, current.and_then(|c| c.get(key.as_str())), &child(key), None, out);
        }
        return !fields.is_empty();
    }
    if let Some(values) = map_values(alt) {
        let Some(Value::Object(held)) = current else { return false };
        let mut held: Vec<(&String, &Value)> = held.iter().collect();
        held.sort_unstable_by_key(|(key, _)| *key);
        for (key, item) in &held {
            let at = out.len();
            node(values, Some(item), &child(key), None, out);
            // `null` deletes an entry.
            if let Some(entry) = out.get_mut(at) {
                entry.optional = true;
            }
        }
        return !held.is_empty();
    }
    if type_name(alt) == Some("array")
        && let (Some(items), Some(Value::Array(held))) = (alt.get("items"), current)
    {
        let fixed = alt.get("minItems").is_some() && alt.get("minItems") == alt.get("maxItems");
        if fixed || alternatives(items).iter().any(|a| a.get("properties").is_some()) {
            for (index, item) in held.iter().enumerate() {
                node(items, Some(item), &child(&index.to_string()), described, out);
            }
            return !held.is_empty();
        }
    }
    false
}

/// Lists the node `schema` describes at `path` (holding `current`, if it is set), then its members.
fn node(schema: &Value, current: Option<&Value>, path: &str, described: Option<&str>, out: &mut Vec<Param>) {
    let current = current.filter(|v| !v.is_null());
    let alternatives = alternatives(schema);
    let chosen = pick(&alternatives, current);
    let description = text(schema, "description")
        .or_else(|| chosen.and_then(|a| text(a, "description")))
        .or_else(|| described.map(str::to_owned));
    let mut children = Vec::new();
    let mut expanded = chosen.is_some_and(|alt| members(alt, current, path, description.as_deref(), &mut children));
    mark_tag(&alternatives, path, &mut children);
    // A value whose tag matches no variant (a stored `"bogus"`) has no members to list; its tag is still listed, so the
    // part can be repaired by setting it.
    if chosen.is_none()
        && current.is_some_and(Value::is_object)
        && let Some((key, names)) = tag_of(&alternatives)
    {
        children.push(tag_param(path, key, names));
        expanded = true;
    }
    out.push(Param {
        path: path.to_owned(),
        ty: ty_of(chosen.or_else(|| alternatives.first().copied()), choices(&alternatives)),
        description,
        unit: unit_of(schema),
        pattern: text(schema, "pattern"),
        optional: nullable(schema),
        tag: false,
        fixed: chosen.and_then(fixed_len),
        expanded,
    });
    out.extend(children);
}

/// The tag of a tagged enum and the names of its variants: the property that each object branch of `alternatives`
/// fixes to a constant text (the first such by name, when there are several).
fn tag_of(alternatives: &[&Value]) -> Option<(String, Vec<String>)> {
    let branches: Vec<&Map<String, Value>> = alternatives
        .iter()
        .filter(|a| type_name(a) == Some("object"))
        .filter_map(|a| a.get("properties")?.as_object())
        .collect();
    let [first, _, ..] = branches.as_slice() else { return None };
    let tags = first.iter().filter_map(|(key, p)| p.get("const")?.as_str().map(|_| key));
    let mut tagged: Vec<(&String, Vec<String>)> = tags
        .map(|key| (key, branches.iter().filter_map(|b| b.get(key)?.get("const")?.as_str().map(str::to_owned)).collect()))
        .filter(|(_, names): &(_, Vec<String>)| names.len() == branches.len())
        .collect();
    tagged.sort_unstable_by_key(|(key, _)| *key);
    tagged.into_iter().next().map(|(key, names)| (key.clone(), names))
}

/// The listing of a tag at `path`/`key` that offers every variant.
fn tag_param(path: &str, key: String, names: Vec<String>) -> Param {
    Param {
        path: if path.is_empty() { key } else { format!("{path}/{key}") },
        ty: Ty::Choice(names),
        description: Some(
            "Which variant this is; setting it switches to that variant, which starts from its own defaults.".to_owned(),
        ),
        unit: None,
        pattern: None,
        optional: false,
        tag: true,
        fixed: None,
        expanded: false,
    }
}

/// Makes the tag of a tagged enum a choice of every variant's name, not just the one the value is in: `children` are the
/// members of the branch the value holds.
fn mark_tag(alternatives: &[&Value], path: &str, children: &mut [Param]) {
    let Some((key, names)) = tag_of(alternatives) else { return };
    let tag = tag_param(path, key, names);
    if let Some(child) = children.iter_mut().find(|c| c.path == tag.path) {
        child.ty = tag.ty;
        child.tag = true;
        if child.description.is_none() {
            child.description = tag.description;
        }
    }
}

/// The length of the list `alt` describes when it fixes one (`minItems` equal to `maxItems`).
fn fixed_len(alt: &Value) -> Option<u64> {
    let (min, max) = (alt.get("minItems")?, alt.get("maxItems")?);
    (type_name(alt) == Some("array") && min == max).then(|| min.as_u64()).flatten()
}

/// The schemas `schema` may be, without `null`: itself, or the branches of its `oneOf` / `anyOf` (an enum, an option),
/// flattened.
fn alternatives(schema: &Value) -> Vec<&Value> {
    let found: Vec<&Value> = ["oneOf", "anyOf"]
        .iter()
        .filter_map(|key| schema.get(key)?.as_array())
        .flatten()
        .filter(|branch| type_name(branch) != Some("null"))
        .flat_map(|branch| alternatives(branch))
        .collect();
    if found.is_empty() { vec![schema] } else { found }
}

/// The branch of `alternatives` that `current` is in: the only one, or the one whose constant a text equals, or whose
/// required keys and constant properties (the tag of a tagged enum) an object has.
fn pick<'a>(alternatives: &[&'a Value], current: Option<&Value>) -> Option<&'a Value> {
    if let [only] = alternatives {
        return Some(only);
    }
    let held = current?;
    alternatives.iter().copied().find(|alt| holds(alt, held))
}

/// Whether the value `held` is of the branch `alt`.
fn holds(alt: &Value, held: &Value) -> bool {
    match held {
        Value::String(_) => {
            alt.get("const") == Some(held)
                || alt.get("enum").and_then(Value::as_array).is_some_and(|names| names.contains(held))
        }
        Value::Object(fields) => {
            // A name alternative (a constant) has no keys to require, so it would match every object: it is not one.
            let mut required = alt.get("required").and_then(Value::as_array).into_iter().flatten();
            let tags = alt.get("properties").and_then(Value::as_object).into_iter().flatten();
            type_name(alt) == Some("object")
                && required.all(|key| key.as_str().is_some_and(|k| fields.contains_key(k)))
                && tags
                    .filter_map(|(key, p)| Some((key, p.get("const")?)))
                    .all(|(key, tag)| fields.get(key) == Some(tag))
        }
        _ => false,
    }
}

/// The names the alternatives allow as text (`const`, or `enum`), in the schema's order.
fn choices(alternatives: &[&Value]) -> Vec<String> {
    alternatives
        .iter()
        .flat_map(|a| a.get("const").into_iter().chain(a.get("enum").and_then(Value::as_array).into_iter().flatten()))
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

/// What `alt` holds.
fn ty_of(alt: Option<&Value>, choices: Vec<String>) -> Ty {
    if !choices.is_empty() {
        return Ty::Choice(choices);
    }
    let Some(alt) = alt else { return Ty::Any };
    match type_name(alt) {
        Some("number") => Ty::Number,
        Some("integer") => Ty::Integer,
        Some("boolean") => Ty::Boolean,
        Some("string") => Ty::Text,
        Some("array") => Ty::List,
        Some("object") if map_values(alt).is_some() => Ty::Map,
        Some("object") => Ty::Section,
        _ => Ty::Any,
    }
}

/// The JSON type of `schema` (the first that is not `null` when it allows several).
fn type_name(schema: &Value) -> Option<&str> {
    match schema.get("type")? {
        Value::String(name) => Some(name),
        Value::Array(names) => names.iter().filter_map(Value::as_str).find(|name| *name != "null"),
        _ => None,
    }
}

/// The schema of a map's entries (names are free), if `alt` is a map.
fn map_values(alt: &Value) -> Option<&Value> {
    match alt.get("patternProperties").and_then(Value::as_object) {
        Some(patterns) => patterns.values().next(),
        None => alt.get("additionalProperties").filter(|v| v.is_object()),
    }
}

/// Whether `schema` allows `null`, which for a stored parameter means "not set".
fn nullable(schema: &Value) -> bool {
    let null_type = match schema.get("type") {
        Some(Value::String(name)) => name == "null",
        Some(Value::Array(names)) => names.iter().any(|n| n == "null"),
        _ => false,
    };
    null_type || ["oneOf", "anyOf"].iter().filter_map(|key| schema.get(key)?.as_array()).flatten().any(nullable)
}

/// The unit a parameter declares: `x-ui.unit` in its schema, the key the schema annotations use. No parameter
/// type declares one yet (the doc comments say "in metres" in prose); once one does, it appears in the answer.
fn unit_of(schema: &Value) -> Option<String> {
    schema.get("x-ui")?.get("unit")?.as_str().map(str::to_owned)
}

/// The text at `key` of `schema`.
fn text(schema: &Value, key: &str) -> Option<String> {
    schema.get(key).and_then(Value::as_str).map(str::to_owned)
}

// ------------------------------------------------------------------------------------------------ values at paths

/// The value at `path` below `root`.
pub(crate) fn at<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('/').try_fold(root, |node, step| match node {
        Value::Object(fields) => fields.get(step),
        Value::Array(items) => step.parse::<usize>().ok().and_then(|i| items.get(i)),
        _ => None,
    })
}

/// Writes `value` at `path` below `root`, creating the sections on the way. With `merge`, an object written over an
/// object sets the fields it names and leaves the others; otherwise, and for anything else, it replaces what was
/// there. `None` removes the key.
///
/// # Errors
/// `invalid` if a step lands on a value that cannot hold the next one, or on a list position that does not exist.
pub(crate) fn set_at(root: &mut Value, path: &str, value: Option<Value>, merge: bool) -> Result<(), OpError> {
    let steps: Vec<&str> = path.split('/').collect();
    let Some((last, parents)) = steps.split_last() else {
        return Err(OpError::invalid("path", "the path is empty"));
    };
    let above = |depth: usize| steps.get(..depth).unwrap_or_default();
    let mut node = root;
    for (depth, step) in parents.iter().enumerate() {
        node = match node {
            Value::Object(fields) => fields.entry(*step).or_insert_with(|| Value::Object(Map::new())),
            Value::Array(items) => {
                let len = items.len();
                step.parse::<usize>()
                    .ok()
                    .and_then(|i| items.get_mut(i))
                    .ok_or_else(|| blocked(above(depth), step, &format!("a list of {len}")))?
            }
            other => return Err(blocked(above(depth), step, kind_name(other))),
        };
    }
    match (node, value) {
        (Value::Object(fields), Some(value)) => match fields.get_mut(*last) {
            Some(slot) if merge => assign(slot, value),
            Some(slot) => *slot = value,
            None => {
                fields.insert((*last).to_owned(), value);
            }
        },
        (Value::Object(fields), None) => {
            fields.remove(*last);
        }
        (Value::Array(items), Some(value)) => {
            let len = items.len();
            let slot = last.parse::<usize>().ok().and_then(|i| items.get_mut(i));
            let slot = slot.ok_or_else(|| blocked(above(parents.len()), last, &format!("a list of {len}")))?;
            if merge {
                assign(slot, value);
            } else {
                *slot = value;
            }
        }
        (other, _) => return Err(blocked(above(parents.len()), last, kind_name(other))),
    }
    Ok(())
}

/// The error for a step that has nothing to land on: `holder` is what the path so far (`above`) holds.
fn blocked(above: &[&str], step: &str, holder: &str) -> OpError {
    let place = if above.is_empty() { "the parameters".to_owned() } else { format!("`{}`", above.join("/")) };
    OpError::invalid("path", format!("{place} holds {holder}, which has no `{step}` inside it"))
}

/// Sets `slot` to `value`; two objects merge key by key, anything else replaces.
fn assign(slot: &mut Value, value: Value) {
    match (slot, value) {
        (Value::Object(target), Value::Object(given)) => {
            for (key, item) in given {
                match target.get_mut(&key) {
                    Some(existing) => assign(existing, item),
                    None => {
                        target.insert(key, item);
                    }
                }
            }
        }
        (slot, value) => *slot = value,
    }
}

/// What a JSON value is, in words an agent reads.
pub(crate) fn kind_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "nothing (null)",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "an object",
    }
}

// ------------------------------------------------------------------------------------------------ probing ranges

/// The values a number accepts, as found by [`Params::range`].
#[derive(Clone, Debug, PartialEq)]
struct Range {
    min: Option<Number>,
    max: Option<Number>,
    /// The lower edge is refused ("greater than 0") rather than allowed.
    open_min: bool,
    /// The upper edge is refused.
    open_max: bool,
}

impl Range {
    /// The range around `near`, a value `accepts` takes. Each edge is the last value taken before the first refused
    /// one, so it is exact: validators compare against literal bounds.
    fn of_floats(near: f64, mut accepts: impl FnMut(f64) -> bool) -> Self {
        let mut toward = |far: f64| edge(order(near), order(far), |k| accepts(unorder(k))).map(unorder);
        let ((min, open_min), (max, open_max)) = (bound(toward(-f64::MAX)), bound(toward(f64::MAX)));
        Self { min, max, open_min, open_max }
    }

    /// The range around `near`, a whole number `accepts` takes.
    fn of_integers(near: i64, mut accepts: impl FnMut(i64) -> bool) -> Self {
        let mut toward = |far: i64| edge(near, far, &mut accepts).map(Number::from);
        let (min, max) = (toward(i64::MIN), toward(i64::MAX));
        Self { min, max, open_min: false, open_max: false }
    }

    /// Adds the bounds to a parameter's entry, named as JSON Schema names them.
    fn write(&self, entry: &mut Map<String, Value>) {
        for (bound, open, closed, opened) in [
            (&self.min, self.open_min, "minimum", "exclusiveMinimum"),
            (&self.max, self.open_max, "maximum", "exclusiveMaximum"),
        ] {
            if let Some(number) = bound {
                entry.insert((if open { opened } else { closed }).to_owned(), Value::Number(number.clone()));
            }
        }
    }

    /// The range as a phrase: `a number from 0.05 to 0.4`.
    fn says(&self, whole: bool) -> String {
        let noun = if whole { "a whole number" } else { "a number" };
        let show = |n: &Number| plain(n, whole);
        let low = |n: &Number| {
            if self.open_min { format!("greater than {}", show(n)) } else { format!("at least {}", show(n)) }
        };
        let high =
            |n: &Number| if self.open_max { format!("less than {}", show(n)) } else { format!("at most {}", show(n)) };
        match (&self.min, &self.max) {
            (Some(lo), Some(hi)) if !self.open_min && !self.open_max => {
                format!("{noun} from {} to {}", show(lo), show(hi))
            }
            (Some(lo), Some(hi)) => format!("{noun} {} and {}", low(lo), high(hi)),
            (Some(lo), None) => format!("{noun} {}", low(lo)),
            (None, Some(hi)) => format!("{noun} {}", high(hi)),
            (None, None) => range_free(whole),
        }
    }
}

/// A number as the validators' own messages write it: `2` rather than JSON's `2.0`. A whole-number parameter keeps
/// its digits as they are.
fn plain(number: &Number, whole: bool) -> String {
    number.as_f64().filter(|_| !whole).map_or_else(|| number.to_string(), |x| x.to_string())
}

/// How a number with no bounds is said.
fn range_free(whole: bool) -> String {
    format!("any {}", if whole { "whole number" } else { "number" })
}

/// The bound an edge found by [`edge`] stands for, and whether the edge itself is refused. A rule like "greater than 0"
/// leaves no smallest accepted float, only the smallest positive one: that reads as an open bound at zero.
fn bound(edge: Option<f64>) -> (Option<Number>, bool) {
    match edge.map(|e| (e.classify(), e)) {
        Some((FpCategory::Subnormal, _)) => (Number::from_f64(0.0), true),
        Some((FpCategory::Zero, _)) => (Number::from_f64(0.0), false),
        Some((_, e)) => (Number::from_f64(e), false),
        None => (None, false),
    }
}

/// Searches from `near`, a value `accepts` takes, towards `far`: `None` if `far` is taken too (no bound that way),
/// otherwise the last value taken before the first refused one. Assumes what is taken is one stretch between them,
/// which a bound on the parameter alone is.
fn edge(near: i64, far: i64, mut accepts: impl FnMut(i64) -> bool) -> Option<i64> {
    if accepts(far) {
        return None;
    }
    let (mut taken, mut refused) = (near, far);
    while taken.abs_diff(refused) > 1 {
        // The midpoint in 128 bits: the sum of two ends of the i64 range does not fit in an i64.
        let Ok(mid) = i64::try_from(i128::midpoint(i128::from(taken), i128::from(refused))) else { break };
        if accepts(mid) {
            taken = mid;
        } else {
            refused = mid;
        }
    }
    Some(taken)
}

/// Floats as integers that sort the same way, so that bisecting the integers bisects the floats between two values
/// exactly, in at most 64 steps. `-0.0` sits just below `0.0`.
fn order(x: f64) -> i64 {
    let bits = i64::from_ne_bytes(x.to_bits().to_ne_bytes());
    if bits < 0 { bits ^ i64::MAX } else { bits }
}

/// The inverse of [`order`].
fn unorder(key: i64) -> f64 {
    let bits = if key < 0 { key ^ i64::MAX } else { key };
    f64::from_bits(u64::from_ne_bytes(bits.to_ne_bytes()))
}

/// The candidate closest to `wanted` by edit distance; the first wins a tie.
pub(crate) fn nearest<'a>(wanted: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    candidates.into_iter().min_by_key(|candidate| edit_distance(wanted, candidate))
}

/// The Levenshtein distance between `a` and `b`, one row of the table at a time.
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = i;
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (diagonal + usize::from(ca != *cb)).min(above + 1).min(row[j] + 1);
            diagonal = above;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_keep_their_order_as_keys_and_round_trip() {
        let values = [-f64::MAX, -1e9, -1.5, -f64::MIN_POSITIVE, -0.0, 0.0, 5e-324, 0.05, 1.0, 1e300, f64::MAX];
        for pair in values.windows(2) {
            assert!(order(pair[0]) < order(pair[1]), "{} should sort below {}", pair[0], pair[1]);
        }
        for value in values {
            assert_eq!(unorder(order(value)).to_bits(), value.to_bits());
        }
    }

    #[test]
    fn the_edge_search_finds_the_exact_last_accepted_value_and_none_when_unbounded() {
        let found = edge(order(0.19), order(f64::MAX), |k| unorder(k) <= 0.4).map(unorder);
        assert_eq!(found, Some(0.4), "the bound is the literal, bit for bit");
        assert_eq!(edge(order(0.19), order(f64::MAX), |_| true), None);
        assert_eq!(edge(7, i64::MAX, |i| i <= 255), Some(255));
        assert_eq!(edge(7, i64::MIN, |i| i >= 0), Some(0));
    }

    #[test]
    fn ranges_are_exact_and_say_what_they_mean() {
        let range = Range::of_floats(0.2, |x| (0.05..=0.4).contains(&x));
        assert_eq!(range.says(false), "a number from 0.05 to 0.4");
        let open = Range::of_floats(1.0, |x| x > 0.0);
        assert_eq!(open.says(false), "a number greater than 0");
        assert!(open.open_min && open.max.is_none());
        let whole = Range::of_integers(3, |i| (2..=5).contains(&i));
        assert_eq!(whole.says(true), "a whole number from 2 to 5");
        assert_eq!(Range::of_floats(1.0, |_| true).says(false), "any number");
        let mut entry = Map::new();
        open.write(&mut entry);
        assert_eq!(Value::Object(entry), json!({"exclusiveMinimum": 0.0}));
    }

    #[test]
    fn the_nearest_name_wins_and_the_first_takes_a_tie() {
        assert_eq!(edit_distance("widht", "width"), 2);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(nearest("half_widt", ["half_height_up", "half_width", "cheek_fullness"]), Some("half_width"));
        assert_eq!(nearest("ab", ["xb", "ax"]), Some("xb"));
        assert_eq!(nearest("ab", []), None);
    }

    #[test]
    fn values_are_read_and_written_by_path_creating_sections_and_merging_objects() {
        let mut params = json!({"a": {"b": [1, {"c": 2}]}, "keep": 1});
        assert_eq!(at(&params, "a/b/1/c"), Some(&json!(2)));
        assert_eq!(at(&params, "a/b/5"), None);

        set_at(&mut params, "a/b/1/c", Some(json!(9)), true).unwrap();
        set_at(&mut params, "new/inside/x", Some(json!(true)), true).unwrap();
        set_at(&mut params, "a", Some(json!({"extra": 1})), true).unwrap();
        assert_eq!(params, json!({"a": {"b": [1, {"c": 9}], "extra": 1}, "keep": 1, "new": {"inside": {"x": true}}}));

        set_at(&mut params, "keep", None, true).unwrap();
        assert_eq!(at(&params, "keep"), None);
        let blocked = set_at(&mut params, "a/extra/deeper", Some(json!(1)), true).unwrap_err();
        assert!(blocked.message.contains("`a/extra` holds a number"), "{}", blocked.message);
        let missing = set_at(&mut params, "a/b/7", Some(json!(1)), true).unwrap_err();
        assert!(missing.message.contains("`a/b` holds a list of 2"), "{}", missing.message);

        set_at(&mut params, "a", Some(json!({"only": 1})), false).unwrap();
        assert_eq!(at(&params, "a"), Some(&json!({"only": 1})), "replacing keeps nothing of the old object");
    }

    #[test]
    fn probing_the_pruned_tree_gives_the_ranges_probing_the_whole_part_gives() {
        let mut probed = 0;
        for kind in omuzaki_char_api::kinds() {
            let params = Params::new(kind, &(kind.defaults)()).unwrap();
            for param in params.entries.iter().filter(|p| matches!(p.ty, Ty::Number | Ty::Integer)) {
                assert_eq!(params.range_in(param, true), params.range_in(param, false), "{} {}", (kind.meta)().id, param.path);
                probed += 1;
            }
        }
        assert!(probed > 100, "the registered kinds have many numbers; probed {probed}");
    }
}
