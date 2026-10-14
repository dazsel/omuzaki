//! Paths into a character's face: `face/eyes/iris/width` names a key below the face. The default profile says which keys
//! exist, what each one holds and where a list ends, so an op refuses an unknown key before anything is written and
//! suggests the key the agent most likely meant.

use omuzaki_anim_api::{CHANNELS, Channel};
use omuzaki_core::text::nearest;
use omuzaki_face_api::{FaceMark, FaceProfile};
use omuzaki_ops_api::OpError;
use serde_json::Value;

use super::edit::to_json;

/// The sections of the face the style ops edit, in the order `face.describe_style` lists them.
pub(crate) const SECTIONS: [&str; 6] = ["eyes", "brows", "mouth", "nose", "cheeks", "marks"];

/// The extra hint for a position in the marks list: the only way to add one.
const MARKS_MORE: &str = "; `face.add_mark` appends one at its end";

/// The default face as JSON: what every key the face has holds when the file leaves it out.
pub(crate) struct Template {
    /// The whole default [`FaceProfile`].
    pub(crate) profile: Value,
    /// The default [`FaceMark`], which every entry of `marks` is checked against.
    pub(crate) mark: Value,
}

/// The default face, serialised once per op call.
///
/// # Errors
/// `Internal` if the default face can't be serialised, which would be a bug in `omuzaki-face-api`.
pub(crate) fn template() -> Result<Template, OpError> {
    Ok(Template { profile: to_json(&FaceProfile::default())?, mark: to_json(&FaceMark::default())? })
}

/// The face section called `name`, when the face has one.
///
/// # Errors
/// `Invalid` naming the section, with the nearest section as the suggestion and the full list in the hint.
pub(crate) fn section(name: &str) -> Result<&'static str, OpError> {
    if let Some(found) = SECTIONS.iter().copied().find(|s| *s == name) {
        return Ok(found);
    }
    let suggestion = nearest(name, SECTIONS).map(|s| format!("did you mean `face/{s}`? ")).unwrap_or_default();
    Err(OpError::invalid(format!("face/{name}"), format!("the face has no section '{name}'"))
        .hint(format!("{suggestion}the sections are {}", SECTIONS.join(", "))))
}

/// The keys below the face that `raw` names: `face/eyes/iris/width`, or `eyes/iris/width` with the `face` left out.
///
/// # Errors
/// `Invalid` for an empty step, a missing section, or a section the face doesn't have.
pub(crate) fn steps(raw: &str) -> Result<Vec<String>, OpError> {
    let mut steps: Vec<&str> = raw.split('/').collect();
    if steps.first() == Some(&"face") {
        steps.remove(0);
    }
    if steps.is_empty() {
        return Err(OpError::invalid("face", "the path names no section of the face").hint(format!(
            "name a section, for example face/eyes/iris/width; the sections are {}",
            SECTIONS.join(", ")
        )));
    }
    if steps.iter().any(|s| s.is_empty()) {
        return Err(OpError::invalid(raw, format!("'{raw}' has an empty step"))
            .hint("write the path as face/<section>/<key>, for example face/eyes/iris/width"));
    }
    if let Some(first) = steps.first() {
        section(first)?;
    }
    Ok(steps.into_iter().map(str::to_owned).collect())
}

/// The canonical form of `keys` below the face, as an agent would write it: `face/eyes/iris/width`.
pub(crate) fn canonical(keys: &[String]) -> String {
    std::iter::once("face").chain(keys.iter().map(String::as_str)).collect::<Vec<_>>().join("/")
}

/// The default node each key of `keys` leads to: `chain[d]` is what `keys[d]` holds by default. A list position counts
/// from zero; `marks` may hold `marks_len` entries, and each one is checked against a default mark.
///
/// # Errors
/// `Invalid` for a key the profile doesn't have (with the nearest key), a position past the end of a list, and a step
/// into a value that holds no keys.
pub(crate) fn chain<'t>(template: &'t Template, keys: &[String], marks_len: usize) -> Result<Vec<&'t Value>, OpError> {
    let mut node = &template.profile;
    let mut chain = Vec::with_capacity(keys.len());
    for depth in 0..keys.len() {
        let key = &keys[depth];
        let parent = canonical(&keys[..depth]);
        let here = canonical(&keys[..=depth]);
        node = match node {
            Value::Object(map) => map.get(key).ok_or_else(|| unknown_key(&parent, key, map.keys()))?,
            Value::Array(_) if depth == 1 && keys[0] == "marks" => {
                position(key, marks_len, &here, MARKS_MORE)?;
                &template.mark
            }
            Value::Array(items) => {
                let at = position(key, items.len(), &here, "")?;
                items.get(at).ok_or_else(|| OpError::internal("a checked list position is missing"))?
            }
            _ => {
                return Err(OpError::invalid(
                    here,
                    format!("{parent} holds a value, not a section, so it has no '{key}'"),
                )
                .hint(format!("set {parent} itself, or a field that it holds")));
            }
        };
        chain.push(node);
    }
    Ok(chain)
}

/// The position `key` names in a list of `len` entries. `more` is added to the hint, for lists with their own way to grow.
///
/// # Errors
/// `Invalid` when `key` isn't a number or is past the end of the list.
fn position(key: &str, len: usize, at: &str, more: &str) -> Result<usize, OpError> {
    let range = if len == 0 { "the list is empty".to_owned() } else { format!("the positions are 0 to {}", len - 1) };
    let hint = format!("{range}{more}");
    let position: usize =
        key.parse().map_err(|_| OpError::invalid(at, format!("'{key}' is not a list position")).hint(hint.clone()))?;
    if position < len {
        return Ok(position);
    }
    Err(OpError::invalid(at, format!("there is no entry {position} here (the list holds {len})")).hint(hint))
}

/// The error for a key that doesn't exist under `parent`, suggesting the nearest key the parent does have.
pub(crate) fn unknown_key<'a>(parent: &str, key: &str, known: impl Iterator<Item = &'a String>) -> OpError {
    let known: Vec<&str> = known.map(String::as_str).collect();
    let suggestion =
        nearest(key, known.iter().copied()).map(|s| format!("did you mean `{parent}/{s}`? ")).unwrap_or_default();
    OpError::invalid(format!("{parent}/{key}"), format!("there is no key '{key}' under {parent}"))
        .hint(format!("{suggestion}the keys here are {}", known.join(", ")))
}

/// The bus channel called `name` (`jawOpen`), or the error for a name that isn't one: it names the rest-layer path and
/// suggests the nearest channel name.
///
/// # Errors
/// `Invalid` at `face/rest/<name>`, with the nearest channel name as the suggestion.
pub(crate) fn channel(name: &str) -> Result<Channel, OpError> {
    Channel::from_name(name).ok_or_else(|| {
        let suggestion = nearest(name, CHANNELS.iter().map(|c| c.name))
            .map(|s| format!("did you mean `face/rest/{s}`? "))
            .unwrap_or_default();
        OpError::invalid(format!("face/rest/{name}"), format!("there is no channel '{name}'")).hint(format!(
            "{suggestion}channels are named as on the animation bus, for example jawOpen or eyeBlinkLeft"
        ))
    })
}

/// What kind of JSON value `value` is, in words an agent reads: a number, text, true or false, a list, or a section.
pub(crate) fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "nothing (null)",
        Value::Bool(_) => "true or false",
        Value::Number(_) => "a number",
        Value::String(_) => "text",
        Value::Array(_) => "a list",
        Value::Object(_) => "a section",
    }
}
