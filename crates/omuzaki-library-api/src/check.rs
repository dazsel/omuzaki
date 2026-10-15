//! Static validation of a manifest (design D1, D3): every problem, each naming the field it is about.

use std::fmt;

use omuzaki_core::id::Slug;
use omuzaki_ops_api::{OpKind, OpMeta, Scope};
use serde_json::Value;

use crate::manifest::{Creates, ItemStep, OpStep, ParamSpec, ParamType, Step, When};
use crate::migrations::CURRENT_SCHEMA;
use crate::placeholder::{Classified, Misuse, Placeholder, classify, is_key, walk_strings};
use crate::recipe::confinement_problem;
use crate::{ItemId, ItemKind, ItemManifest, ItemRights, Target, VersionReq};

/// One thing wrong with a manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// Where, as the manifest spells it, joined by `/`: `id`, `params/tail_length/default`,
    /// `step/2/args/character`.
    pub field: String,
    /// What is wrong and what to do.
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// A dotted op name, `area.verb` or deeper (`face.expression.set`).
fn is_op_name(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    parts.len() >= 2
        && parts.iter().all(|p| {
            p.starts_with(|c: char| c.is_ascii_lowercase())
                && p.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        })
}

struct Checker<'a> {
    m: &'a ItemManifest,
    problems: Vec<Problem>,
}

impl Checker<'_> {
    fn add(&mut self, field: impl Into<String>, message: impl Into<String>) {
        self.problems.push(Problem { field: field.into(), message: message.into() });
    }

    fn slug(&mut self, field: &str, value: &str) {
        if let Err(e) = Slug::new(value) {
            self.add(field, e.to_string());
        }
    }

    fn header(&mut self) {
        let m = self.m;
        if m.schema != CURRENT_SCHEMA {
            self.add("schema", format!("this build reads schema {CURRENT_SCHEMA}"));
        }
        match m.item_id() {
            Err(e) => self.add("id", e.to_string()),
            Ok(id) if id.is_first_party() => self.first_party(),
            Ok(_) => {}
        }
        if let Err(e) = m.item_version() {
            self.add("version", e.to_string());
        }
        for (field, text) in [("title", &m.title), ("summary", &m.summary)] {
            if text.trim().is_empty() {
                self.add(field, "can't be empty");
            }
        }
        self.asset_info();
        if let Some(slot) = &m.slot {
            self.slug("slot", slot);
        }
        if let Some(name) = &m.name {
            self.slug("name", name);
        }
        for (i, tag) in m.tags.iter().enumerate() {
            if tag.is_empty() || *tag != tag.to_lowercase() || tag.contains(char::is_whitespace) {
                self.add(format!("tags/{i}"), format!("{tag:?} is not a lowercase word"));
            }
        }
    }

    /// `asset_info`, when given, describes exactly the assets the manifest declares: a client joins rights and bounds
    /// sizes from it, so a missing entry would be an unchecked asset and an extra one a description of nothing.
    fn asset_info(&mut self) {
        let m = self.m;
        if m.asset_info.is_empty() {
            return;
        }
        for key in m.assets.keys().filter(|k| !m.asset_info.contains_key(*k)) {
            self.add(format!("asset_info/{key}"), format!("`assets.{key}` has no entry; give its size and rights"));
        }
        for key in m.asset_info.keys().filter(|k| !m.assets.contains_key(*k)) {
            self.add(format!("asset_info/{key}"), format!("`{key}` is not declared in `assets`; remove the entry"));
        }
    }

    /// The extra rules for `omuzaki/` ids (design D8, D15): the seed lint relies on these.
    fn first_party(&mut self) {
        if self.m.origin.as_deref().is_none_or(|o| o.trim().is_empty()) {
            self.add("origin", "first-party items need one line saying where their values came from");
        }
        if self.m.rights != ItemRights::first_party() {
            self.add(
                "rights",
                "first-party items carry exactly ItemRights::first_party() (CC0-1.0, commercial ok, owned)",
            );
        }
        if !self.m.assets.is_empty() {
            self.add("assets", "first-party items carry no binary assets");
        }
    }

    fn placement(&mut self) {
        let m = self.m;
        match m.item_section() {
            Err(e) => self.add("section", e.to_string()),
            Ok(section) if section.target() != m.target => {
                self.add("target", format!("items in section {section} apply to the {:?} target", section.target()));
            }
            Ok(_) => {}
        }
        if let Some(why) = m.target.reserved() {
            self.add("target", why);
        }
        match (m.kind, m.creates.is_empty()) {
            (ItemKind::Part, true) => self.add("kind", "a part item must declare the parts it makes in `creates`"),
            (ItemKind::Part, false) | (_, true) => {}
            (kind, false) => {
                self.add("kind", format!("only part items declare `creates`, but this is a {kind:?} item"));
            }
        }
        for (key, creates) in &m.creates {
            self.creates(key, creates);
        }
        for (i, kind) in m.target_part.iter().enumerate() {
            self.slug(&format!("target_part/{i}"), kind);
        }
        self.compat();
    }

    fn creates(&mut self, key: &str, c: &Creates) {
        if !is_key(key) {
            self.add(format!("creates/{key}"), "keys are lowercase letters, digits, `_` and `-`");
        }
        self.slug(&format!("creates/{key}/kind"), &c.kind);
        self.slug(&format!("creates/{key}/id"), &c.id);
        if let Some(parent) = &c.parent {
            let named = parent.strip_prefix("$parts.");
            if !named.is_some_and(|k| k != key && self.m.creates.contains_key(k)) {
                self.add(format!("creates/{key}/parent"), "must be `$parts.<key>` of another part this item creates");
            }
        }
    }

    fn compat(&mut self) {
        let c = &self.m.compat;
        for (name, values) in [("templates", &c.templates), ("styles", &c.styles), ("requires", &c.requires)] {
            for (i, v) in values.iter().enumerate() {
                if v != "*" {
                    self.slug(&format!("compat/{name}/{i}"), v);
                }
            }
        }
    }

    fn params(&mut self) {
        for (key, spec) in &self.m.params {
            let field = format!("params/{key}");
            if !is_key(key) {
                self.add(&field, "keys are lowercase letters, digits, `_` and `-`");
            }
            self.param_spec(&field, spec);
        }
    }

    fn param_spec(&mut self, field: &str, spec: &ParamSpec) {
        if spec.ty == ParamType::Choice && spec.choices.is_empty() {
            self.add(format!("{field}/choices"), "a choice parameter needs its choices");
        }
        if spec.min.zip(spec.max).is_some_and(|(min, max)| min > max) {
            self.add(format!("{field}/min"), "min is above max");
        }
        if let Err(why) = spec.validate(&spec.default) {
            self.add(format!("{field}/default"), why);
        }
    }

    fn steps(&mut self) {
        if self.m.steps.is_empty() {
            self.add("step", "an item needs at least one step");
        }
        for (i, step) in self.m.steps.iter().enumerate() {
            let base = format!("step/{i}");
            match step {
                Step::Op(s) => self.op_step(&base, s),
                Step::Item(s) => self.item_step(&base, s),
            }
        }
    }

    fn when(&mut self, base: &str, when: Option<&When>) {
        let Some(when) = when else { return };
        if when.templates.is_empty() {
            self.add(format!("{base}/when/templates"), "can't be empty");
        }
        for (i, t) in when.templates.iter().enumerate().filter(|(_, t)| *t != "*") {
            self.slug(&format!("{base}/when/templates/{i}"), t);
        }
    }

    fn op_step(&mut self, base: &str, step: &OpStep) {
        if !is_op_name(&step.op) {
            self.add(format!("{base}/op"), format!("{:?} is not an op name like `part.add`", step.op));
        }
        if !step.args.is_object() {
            self.add(format!("{base}/args"), "must be a table of arguments");
        }
        if let Some((key, expected)) = confinement_problem(&step.args) {
            let message = format!("must be `{expected}`: an item may only change the target it is applied to");
            self.add(format!("{base}/args/{key}"), message);
        }
        self.placeholders(&step.args, &format!("{base}/args"));
        self.when(base, step.when.as_ref());
    }

    fn item_step(&mut self, base: &str, step: &ItemStep) {
        if self.m.kind != ItemKind::Bundle {
            self.add(format!("{base}/item"), "only bundle items apply other items");
        }
        if let Err(e) = ItemId::new(step.item.as_str()) {
            self.add(format!("{base}/item"), e.to_string());
        }
        if let Err(e) = VersionReq::parse(&step.version) {
            self.add(format!("{base}/version"), e.to_string());
        }
        for (key, value) in &step.params {
            self.placeholders(value, &format!("{base}/params/{key}"));
        }
        for (key, value) in &step.bind {
            let field = format!("{base}/bind/{key}");
            if !matches!(key.as_str(), "target" | "as") {
                self.add(&field, "a bundle step can bind `target` and `as`");
            }
            self.placeholders(&Value::String(value.clone()), &field);
        }
        self.when(base, step.when.as_ref());
    }

    fn placeholders(&mut self, value: &Value, base: &str) {
        let mut found: Vec<(String, Result<Classified, &'static str>)> = Vec::new();
        walk_strings(value, base, &mut |path, s| found.push((path.to_owned(), classify(s).map_err(Misuse::message))));
        for (path, result) in found {
            match result {
                Err(message) => self.add(path, message),
                Ok(Classified::Placeholder(ph)) => self.reference(&path, &ph),
                Ok(Classified::Literal(_)) => {}
            }
        }
    }

    /// Whether the placeholder names something this manifest declares.
    fn reference(&mut self, path: &str, ph: &Placeholder) {
        let m = self.m;
        let problem = match ph {
            Placeholder::Character if m.target != Target::Character => {
                Some("`$character` needs the character target".to_owned())
            }
            Placeholder::Scene if m.target != Target::Scene => Some("`$scene` needs the scene target".to_owned()),
            Placeholder::Target if m.target_part.is_empty() => {
                Some("`$target` needs `target_part` to say which part kinds it may be".to_owned())
            }
            Placeholder::Name if m.kind != ItemKind::Member => Some("`$name` is only for member items".to_owned()),
            // A bundle's `$parts.<key>` may name a part of a nested item, which only the registry can resolve.
            Placeholder::Part(key) if m.kind != ItemKind::Bundle && !m.creates.contains_key(key) => {
                Some(format!("`$parts.{key}` names no part: declare it in `creates.{key}`"))
            }
            Placeholder::Param(key) if !m.params.contains_key(key) => {
                Some(format!("`$params.{key}` is not declared in `params.{key}`"))
            }
            Placeholder::Asset(key) if !m.assets.contains_key(key) => {
                Some(format!("`$assets.{key}` is not declared in `assets.{key}`"))
            }
            _ => None,
        };
        if let Some(message) = problem {
            self.add(path, message);
        }
    }
}

impl ItemManifest {
    /// Every problem with the manifest as written, each naming its field. An empty list means the manifest is
    /// well-formed; whether its ops exist and are allowed is [`check_ops`], which needs the op registry.
    ///
    /// The scene target (and show and performance) is refused here until its contract exists.
    pub fn check(&self) -> Vec<Problem> {
        let mut c = Checker { m: self, problems: Vec::new() };
        c.header();
        c.placement();
        c.params();
        c.steps();
        c.problems
    }
}

/// Safety rules 1 and 3 (design D3): every op step names a registered op, of kind `Edit`, with scope
/// `edit:character` or `edit:scene`. `lookup` is the op registry (`omuzaki_ops_api::find`, or a stand-in in tests).
pub fn check_ops(manifest: &ItemManifest, lookup: impl Fn(&str) -> Option<OpMeta>) -> Vec<Problem> {
    let mut problems = Vec::new();
    for (i, step) in manifest.steps.iter().enumerate() {
        let Step::Op(step) = step else { continue };
        let field = format!("step/{i}/op");
        let message = match lookup(&step.op) {
            None => {
                Some(format!("`{}` is not a registered op in this build; the item is unavailable until it is", step.op))
            }
            Some(meta) if meta.kind != OpKind::Edit => {
                Some(format!("`{}` is a {:?}, and recipes may only call edit ops", step.op, meta.kind))
            }
            Some(meta) if !matches!(meta.scope, Scope::EditCharacter | Scope::EditScene) => Some(format!(
                "`{}` needs the {} scope; recipes are limited to edit:character and edit:scene",
                step.op, meta.scope
            )),
            Some(_) => None,
        };
        if let Some(message) = message {
            problems.push(Problem { field, message });
        }
    }
    problems
}
