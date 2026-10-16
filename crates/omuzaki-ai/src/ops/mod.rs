//! The `model.*` ops (design `docs/design/ai-api.md` section 11, ADR 0016 rule 5): they live with the registry and reach
//! it as the `dyn ModelRegistry` service the app wires, so they never see a session, a stream or a local path.
//!
//! | Op | Kind | Scope | Who |
//! |---|---|---|---|
//! | `model.list` | query | `read` | anyone |
//! | `model.status` | query | `read` | anyone |
//! | `model.add` | job | `edit:library`, and `fs` for the file's root | anyone; only a person may accept the notice while adding |
//! | `model.select` | edit | `perform` | anyone; an agent's switch to a model the person hasn't accepted is `needs_person` |
//! | `model.remove` | edit | `edit:library` | anyone |
//! | `model.acknowledge` | edit | `edit:library` | a person only |
//!
//! Each op registers itself next to its type. `select`, `remove` and `acknowledge` change the registry (not the
//! document), so a dry run resolves the names and changes nothing; the registry's own rules (the requester rule, a
//! model's status) are checked by the real call. There is nothing to undo.

pub mod acknowledge;
pub mod add;
pub mod list;
pub mod remove;
pub mod select;
pub mod status;

use omuzaki_ai_api::{ModelRef, ModelRegistry, RegistryError, Requester, SlotRef};
use omuzaki_ops_api::{Actor, OpError};

/// Who asks for a switch: a person, or anything else (an agent, a plugin, an automation).
fn requester(actor: &Actor) -> Requester {
    match actor {
        Actor::Person { .. } => Requester::Person,
        _ => Requester::Agent,
    }
}

/// The slot named `text` (`id@version`), or an error that names the closest registered one.
fn slot(field: &str, text: &str, registry: &dyn ModelRegistry) -> Result<SlotRef, OpError> {
    let known: Vec<String> = registry.slots().into_iter().map(|s| s.info.slot.to_string()).collect();
    match text.parse::<SlotRef>() {
        Ok(slot) if known.contains(&slot.to_string()) => Ok(slot),
        _ => Err(not_found(field, &format!("there is no slot {text:?}"), text, &known, "model.list lists the slots")),
    }
}

/// The model named `text`: its id, `id@version`, or `id@version#digest` (enough of the digest to tell it apart).
/// Several matches are an error that lists them, so an agent never picks one by accident.
fn model(field: &str, text: &str, registry: &dyn ModelRegistry) -> Result<ModelRef, OpError> {
    let (name, digest) = text.split_once('#').map_or((text, None), |(n, d)| (n, Some(d)));
    let (id, version) = name.split_once('@').map_or((name, None), |(i, v)| (i, Some(v)));
    let models = registry.models(None);
    let matches: Vec<&ModelRef> = models
        .iter()
        .map(|e| &e.model)
        .filter(|m| m.id.as_str() == id)
        .filter(|m| version.is_none_or(|v| m.version.to_string() == v))
        .filter(|m| digest.is_none_or(|d| m.digest.to_string().starts_with(d)))
        .collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => {
            let ids: Vec<String> = models.iter().map(|e| e.model.id.to_string()).collect();
            Err(not_found(field, &format!("there is no model {text:?}"), id, &ids, "model.list lists the models"))
        }
        many => {
            let names: Vec<String> = many.iter().map(|m| full_name(m)).collect();
            Err(OpError::invalid(field, format!("{text:?} names {} models", many.len()))
                .hint(format!("name one exactly: {}", names.join(", "))))
        }
    }
}

/// How `model.list` names a model, and how the ops take it back: `id@version#digest`.
pub fn full_name(model: &ModelRef) -> String {
    format!("{}@{}#{}", model.id, model.version, model.digest)
}

/// A not-found error on `field`, with the closest of `known` to `wanted` as a hint.
fn not_found(field: &str, message: &str, wanted: &str, known: &[String], fallback: &str) -> OpError {
    let mut error = OpError::not_found(message);
    error.field = Some(field.to_owned());
    match omuzaki_core::text::nearest(wanted, known.iter().map(String::as_str)) {
        Some(near) => error.hint(format!("did you mean {near:?}? {fallback}")),
        None => error.hint(fallback.to_owned()),
    }
}

/// The op error for a registry error: what to fix is in the registry's words.
fn error(e: &RegistryError) -> OpError {
    let message = e.to_string();
    match e {
        RegistryError::UnknownSlot(_) | RegistryError::UnknownModel(_) => OpError::not_found(message),
        RegistryError::NeedsPerson(_) => OpError::needs_person(message),
        RegistryError::WrongSlot { .. } | RegistryError::WrongMode(_) | RegistryError::Manifest(_) => {
            OpError::invalid("model", message)
        }
        RegistryError::Io(_) => OpError::internal(message),
        _ => OpError::unsupported(message),
    }
}
