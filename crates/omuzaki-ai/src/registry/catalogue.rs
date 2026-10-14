//! The catalogue: the shipped models the app registers, parsed and checked once at startup. A bad registration is
//! logged and listed as disabled with its reason; it never stops the others (or panics).

use omuzaki_ai_api::{ModelManifest, ModelRef, Origin, find_slot};

use crate::model_ref;

/// One registered shipped model.
#[derive(Clone, Debug)]
pub(super) struct Registered {
    pub manifest: ModelManifest,
    pub model: ModelRef,
    /// Why the registration is refused, if it is.
    pub problem: Option<String>,
    /// Where the registry got the manifest from.
    pub origin: Origin,
}

/// Reads every `(literal id, manifest TOML)` registration. The second value lists registrations that couldn't be
/// listed at all (their manifest doesn't parse, so there is nothing to show).
pub(super) fn read(models: impl IntoIterator<Item = (String, String)>) -> (Vec<Registered>, Vec<String>) {
    let mut out: Vec<Registered> = Vec::new();
    let mut notes = Vec::new();
    for (id, text) in models {
        let manifest = match ModelManifest::from_toml(&text) {
            Ok(manifest) => manifest,
            Err(e) => {
                notes.push(format!("the shipped model {id} was refused: {e}"));
                continue;
            }
        };
        let model = model_ref(&manifest);
        let problem = problem(&id, &manifest);
        if let Some(problem) = &problem {
            notes.push(format!("the shipped model {model} was refused: {problem}"));
        }
        if !out.iter().any(|r| r.model == model) {
            out.push(Registered { manifest, model, problem, origin: Origin::Shipped });
        }
    }
    (out, notes)
}

/// What is wrong with a registration, in one line.
fn problem(literal_id: &str, manifest: &ModelManifest) -> Option<String> {
    if manifest.id.as_str() != literal_id {
        return Some(format!("it was registered as `{literal_id}` but its manifest says `{}`", manifest.id));
    }
    if let Err(errors) = manifest.validate(Origin::Shipped) {
        return Some(errors.iter().map(ToString::to_string).collect::<Vec<_>>().join("; "));
    }
    if find_slot(&manifest.slot).is_none() {
        return Some(format!("no slot {} is registered in this app", manifest.slot));
    }
    None
}
