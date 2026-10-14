//! Recipe expansion (design D3): bindings and parameters in, concrete step requests out. Pure.

use std::collections::BTreeMap;

use omuzaki_char_api::{BodyTemplateId, CharId, PartId};
use serde_json::Value;

use crate::manifest::{ItemStep, OpStep, Step, When};
use crate::placeholder::{Classified, Placeholder, classify};
use crate::{ItemId, ItemManifest, Target, VersionReq};

const RESERVED_SCENE: &str = match Target::Scene.reserved() {
    Some(why) => why,
    None => "the scene target is reserved",
};

/// Parameter values by key, as the caller gave them.
pub type Params = BTreeMap<String, Value>;

/// What the caller binds the placeholders to (design D3).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Bindings {
    /// `$character`: the target character.
    pub character: Option<CharId>,
    /// `$parts.<key>`: the final id of each part the item creates (and, in a bundle, of its nested items' parts).
    pub parts: BTreeMap<String, PartId>,
    /// `$target`: the existing part the item changes.
    pub target: Option<PartId>,
    /// `$name`: the member name (the caller's `as`, else the manifest's `name`).
    pub name: Option<String>,
    /// `$template`: the target character's body template, read-only, for `when` guards.
    pub template: Option<BodyTemplateId>,
}

/// One expanded step, ready to plan.
#[derive(Clone, Debug, PartialEq)]
pub enum StepRequest {
    /// Run an op.
    Op {
        /// Position in the manifest's `[[step]]` list, for errors (`step/3`).
        index: usize,
        /// The op name.
        op: String,
        /// Its arguments with every placeholder replaced.
        args: Value,
    },
    /// Apply a nested item (a bundle step).
    Item {
        /// Position in the manifest's `[[step]]` list.
        index: usize,
        /// The nested item.
        item: ItemId,
        /// The range to resolve it in.
        version: VersionReq,
        /// Its parameters with placeholders replaced.
        params: Params,
        /// Its `target` and `as` bindings, resolved to concrete strings.
        bind: BTreeMap<String, String>,
    },
}

/// Why a recipe couldn't be expanded.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum RecipeError {
    /// The target (or `$scene`) is not available yet.
    #[error("{0}")]
    Reserved(&'static str),
    /// A placeholder is misused or a step is malformed.
    #[error("{field}: {message}")]
    Invalid {
        /// Where, `step/2/args/character`.
        field: String,
        /// What is wrong.
        message: String,
    },
    /// A placeholder names something the caller did not bind.
    #[error("{field}: `{placeholder}` is not bound; {hint}")]
    Unbound {
        /// Where.
        field: String,
        /// The placeholder as written.
        placeholder: String,
        /// What the caller should pass.
        hint: String,
    },
    /// A step reaches outside the bound target (safety rule 2).
    #[error("{field}: must be `{expected}`: an item may only change the target it is applied to")]
    NotTheTarget {
        /// Where.
        field: String,
        /// The only allowed value.
        expected: &'static str,
    },
    /// A parameter value is undeclared or invalid.
    #[error("params/{key}: {message}")]
    Param {
        /// The parameter.
        key: String,
        /// What is wrong.
        message: String,
    },
}

fn invalid(field: &str, message: impl Into<String>) -> RecipeError {
    RecipeError::Invalid { field: field.to_owned(), message: message.into() }
}

/// Safety rule 2: arguments named `character` or `scene` must be exactly the bound placeholder. Returns the first
/// offending argument name and the only value it may have, so `check` and `expand` agree.
pub(crate) fn confinement_problem(args: &Value) -> Option<(&'static str, &'static str)> {
    [("character", "$character"), ("scene", "$scene")]
        .into_iter()
        .find(|(key, expected)| args.get(key).is_some_and(|v| v.as_str() != Some(expected)))
}

struct Context<'a> {
    manifest: &'a ItemManifest,
    bindings: &'a Bindings,
    params: Params,
}

impl Context<'_> {
    fn replace(&self, field: &str, ph: &Placeholder) -> Result<Value, RecipeError> {
        let unbound = |placeholder: String, hint: &str| RecipeError::Unbound {
            field: field.to_owned(),
            placeholder,
            hint: hint.to_owned(),
        };
        match ph {
            Placeholder::Character => self
                .bindings
                .character
                .as_ref()
                .map(|c| Value::from(c.as_str()))
                .ok_or_else(|| unbound("$character".into(), "pass the character to apply to")),
            Placeholder::Scene => Err(RecipeError::Reserved(RESERVED_SCENE)),
            Placeholder::Target => self
                .bindings
                .target
                .as_ref()
                .map(|p| Value::from(p.as_str()))
                .ok_or_else(|| unbound("$target".into(), "pass the part to change")),
            Placeholder::Name => self
                .bindings
                .name
                .as_deref()
                .or(self.manifest.name.as_deref())
                .map(Value::from)
                .ok_or_else(|| unbound("$name".into(), "pass `as`, the member name")),
            Placeholder::Template => self
                .bindings
                .template
                .as_ref()
                .map(|t| Value::from(t.as_str()))
                .ok_or_else(|| unbound("$template".into(), "pass the target's body template")),
            Placeholder::Part(key) => self
                .bindings
                .parts
                .get(key)
                .map(|p| Value::from(p.as_str()))
                .ok_or_else(|| unbound(format!("$parts.{key}"), "bind the id of the part it creates")),
            Placeholder::Param(key) => self
                .params
                .get(key)
                .cloned()
                .ok_or_else(|| unbound(format!("$params.{key}"), "declare the parameter in `params`")),
            Placeholder::Asset(key) => self
                .manifest
                .assets
                .get(key)
                .map(|h| Value::from(h.to_string()))
                .ok_or_else(|| unbound(format!("$assets.{key}"), "declare the asset in `assets`")),
        }
    }

    /// Replaces every whole-value placeholder in `value`, with its type; `$$` becomes a literal `$`.
    fn substitute(&self, field: &str, value: &Value) -> Result<Value, RecipeError> {
        match value {
            Value::String(s) => match classify(s).map_err(|m| invalid(field, m.message()))? {
                Classified::Literal(text) => Ok(Value::String(text)),
                Classified::Placeholder(ph) => self.replace(field, &ph),
            },
            Value::Array(items) => items
                .iter()
                .enumerate()
                .map(|(i, v)| self.substitute(&format!("{field}/{i}"), v))
                .collect::<Result<Vec<_>, _>>()
                .map(Value::Array),
            Value::Object(map) => map
                .iter()
                .map(|(k, v)| Ok((k.clone(), self.substitute(&format!("{field}/{k}"), v)?)))
                .collect::<Result<serde_json::Map<_, _>, _>>()
                .map(Value::Object),
            other => Ok(other.clone()),
        }
    }

    /// Whether a step's `when` guard lets it run on the bound template.
    fn runs(&self, base: &str, when: Option<&When>) -> Result<bool, RecipeError> {
        let Some(when) = when else { return Ok(true) };
        let template = self.replace(&format!("{base}/when"), &Placeholder::Template)?;
        Ok(when.templates.iter().any(|t| t == "*" || Some(t.as_str()) == template.as_str()))
    }

    fn op_step(&self, index: usize, step: &OpStep) -> Result<StepRequest, RecipeError> {
        let base = format!("step/{index}");
        if let Some((key, expected)) = confinement_problem(&step.args) {
            return Err(RecipeError::NotTheTarget { field: format!("{base}/args/{key}"), expected });
        }
        Ok(StepRequest::Op { index, op: step.op.clone(), args: self.substitute(&format!("{base}/args"), &step.args)? })
    }

    fn item_step(&self, index: usize, step: &ItemStep) -> Result<StepRequest, RecipeError> {
        let base = format!("step/{index}");
        let item = ItemId::new(step.item.as_str()).map_err(|e| invalid(&format!("{base}/item"), e.to_string()))?;
        let version =
            VersionReq::parse(&step.version).map_err(|e| invalid(&format!("{base}/version"), e.to_string()))?;
        let params = step
            .params
            .iter()
            .map(|(k, v)| Ok((k.clone(), self.substitute(&format!("{base}/params/{k}"), v)?)))
            .collect::<Result<Params, RecipeError>>()?;
        let bind = step
            .bind
            .iter()
            .map(|(k, v)| {
                let field = format!("{base}/bind/{k}");
                match self.substitute(&field, &Value::String(v.clone()))? {
                    Value::String(s) => Ok((k.clone(), s)),
                    _ => Err(invalid(&field, "must resolve to text")),
                }
            })
            .collect::<Result<_, RecipeError>>()?;
        Ok(StepRequest::Item { index, item, version, params, bind })
    }
}

/// The parameters the recipe sees: the caller's values over the declared defaults, each validated.
fn resolve_params(manifest: &ItemManifest, given: &Params) -> Result<Params, RecipeError> {
    if let Some(key) = given.keys().find(|k| !manifest.params.contains_key(*k)) {
        let declared: Vec<&str> = manifest.params.keys().map(String::as_str).collect();
        return Err(RecipeError::Param {
            key: key.clone(),
            message: format!("not declared by this item (it has {declared:?})"),
        });
    }
    manifest
        .params
        .iter()
        .map(|(key, spec)| {
            let value = given.get(key).unwrap_or(&spec.default);
            spec.validate(value).map_err(|message| RecipeError::Param { key: key.clone(), message })?;
            Ok((key.clone(), value.clone()))
        })
        .collect()
}

/// Expands the recipe: every whole-value placeholder in every step replaced by its typed value, steps whose
/// `when` guard excludes the bound template left out. Pure: the same inputs give the same requests.
///
/// Safety rule 2 holds here too: an argument named `character` that is not exactly `$character` is refused, so
/// a recipe can only ever name the bound target.
///
/// # Errors
/// [`RecipeError`] naming the field, parameter or placeholder at fault.
pub fn expand(manifest: &ItemManifest, bindings: &Bindings, params: &Params) -> Result<Vec<StepRequest>, RecipeError> {
    if let Some(why) = manifest.target.reserved() {
        return Err(RecipeError::Reserved(why));
    }
    let cx = Context { manifest, bindings, params: resolve_params(manifest, params)? };
    let mut requests = Vec::new();
    for (index, step) in manifest.steps.iter().enumerate() {
        let when = match step {
            Step::Op(s) => s.when.as_ref(),
            Step::Item(s) => s.when.as_ref(),
        };
        if !cx.runs(&format!("step/{index}"), when)? {
            continue;
        }
        requests.push(match step {
            Step::Op(s) => cx.op_step(index, s)?,
            Step::Item(s) => cx.item_step(index, s)?,
        });
    }
    Ok(requests)
}
