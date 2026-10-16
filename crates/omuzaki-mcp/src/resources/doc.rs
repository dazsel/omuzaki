//! The built-in provider: the document itself, as `omuzaki://project` and `omuzaki://character/<id>`.

use omuzaki_core::id::Slug;
use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{Grants, OpError, Runner};
use rmcp::model::{Resource, ResourceContents, ResourceTemplate};
use serde_json::{Map, Value, json};

use super::{ResourceProvider, json_contents};

/// The project summary's URI.
pub(crate) const PROJECT_URI: &str = "omuzaki://project";
const CHARACTER_PREFIX: &str = "omuzaki://character/";
const CHARACTERS: &str = "characters";
const JSON: &str = "application/json";

/// Serves the project summary and each character's stored node.
#[derive(Debug)]
pub(crate) struct DocProvider;

impl ResourceProvider for DocProvider {
    fn templates(&self) -> Vec<ResourceTemplate> {
        vec![
            ResourceTemplate::new(format!("{CHARACTER_PREFIX}{{id}}"), "character")
                .with_title("Character")
                .with_description(
                    "Everything stored for one character, as the document holds it. `id` is the character's slug, as \
                     listed in omuzaki://project.",
                )
                .with_mime_type(JSON),
        ]
    }

    fn list(&self, runner: &dyn Runner, grants: &Grants) -> Result<Vec<Resource>, OpError> {
        let project = Resource::new(PROJECT_URI, "project")
            .with_title("Project")
            .with_description(
                "A summary of the project: what each top-level section of the document holds, by id. Read a \
                 character for its full contents.",
            )
            .with_mime_type(JSON);
        let characters = child_ids(runner, grants, CHARACTERS)?.into_iter().map(|id| {
            Resource::new(format!("{CHARACTER_PREFIX}{id}"), format!("character {id}"))
                .with_title(format!("Character {id}"))
                .with_mime_type(JSON)
        });
        Ok(std::iter::once(project).chain(characters).collect())
    }

    fn serves(&self, uri: &str) -> bool {
        uri == PROJECT_URI || uri.starts_with(CHARACTER_PREFIX)
    }

    fn read(&self, runner: &dyn Runner, grants: &Grants, uri: &str) -> Result<Vec<ResourceContents>, OpError> {
        let body = match uri.strip_prefix(CHARACTER_PREFIX) {
            Some(id) => character(runner, grants, id)?,
            None => project(runner, grants)?,
        };
        json_contents(uri, body)
    }

    /// `characters/<id>/..` is that character's node; any change at all shows in the project summary. A change above
    /// the characters (the root, or `characters` itself) names no id, so it touches every character.
    fn touched(&self, paths: &[DocPath]) -> Vec<String> {
        let mut uris = Vec::new();
        if !paths.is_empty() {
            uris.push(PROJECT_URI.to_owned());
        }
        for path in paths {
            if let [section, id, ..] = path.segments()
                && section == CHARACTERS
            {
                uris.push(format!("{CHARACTER_PREFIX}{id}"));
            } else if path.segments().is_empty() || path.segments() == [CHARACTERS] {
                uris.push(CHARACTER_PREFIX.to_owned());
            }
        }
        uris
    }
}

/// The keys of the object at `path` (empty if there is none), sorted.
fn child_ids(runner: &dyn Runner, grants: &Grants, path: &str) -> Result<Vec<String>, OpError> {
    let found = runner.get(&root_child(path)?, grants)?;
    Ok(found.as_ref().and_then(Value::as_object).map(|o| o.keys().cloned().collect()).unwrap_or_default())
}

fn root_child(segment: &str) -> Result<DocPath, OpError> {
    DocPath::parse(segment).map_err(|e| OpError::internal(e.to_string()))
}

/// For each top-level key: its child ids if it is an object (so `characters` lists the characters), else its value.
/// Generic on purpose: a new section of the document shows up without editing a list here, and the whole document is
/// never dumped.
fn project(runner: &dyn Runner, grants: &Grants) -> Result<Value, OpError> {
    let root = runner.get(&DocPath::root(), grants)?.unwrap_or(Value::Null);
    let sections: Map<String, Value> = root
        .as_object()
        .into_iter()
        .flatten()
        .map(|(key, value)| {
            let summary = value
                .as_object()
                .map_or_else(|| value.clone(), |children| json!({"ids": children.keys().collect::<Vec<_>>()}));
            (key.clone(), summary)
        })
        .collect();
    Ok(json!({
        "sections": sections,
        "note": "Read omuzaki://character/<id> for everything stored for a character, and omuzaki://log for what changed.",
    }))
}

/// The node at `characters/<id>`, whole. An id that isn't a slug can't name a character (and a `/` would reach into
/// its sub-paths, which this resource doesn't offer), so both it and a missing id are `not_found`.
fn character(runner: &dyn Runner, grants: &Grants, id: &str) -> Result<Value, OpError> {
    let found = match Slug::new(id) {
        Ok(slug) => runner.get(&root_child(CHARACTERS)?.join(slug.as_str()), grants)?,
        Err(_) => None,
    };
    if let Some(node) = found {
        return Ok(node);
    }
    // Only a miss pays to list the ids that exist.
    let known = child_ids(runner, grants, CHARACTERS)?;
    let ids = if known.is_empty() { "none".to_owned() } else { known.join(", ") };
    Err(OpError::not_found(format!("no character {id:?}; the characters are: {ids}"))
        .hint("read omuzaki://project to list them"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(s: &str) -> DocPath {
        DocPath::parse(s).unwrap()
    }

    #[test]
    fn a_change_inside_a_character_touches_it_and_the_project() {
        let touched = DocProvider.touched(&[path("characters/mira/face/blend")]);
        assert_eq!(touched, [PROJECT_URI, "omuzaki://character/mira"]);
    }

    #[test]
    fn a_change_above_the_characters_touches_every_character() {
        assert_eq!(DocProvider.touched(&[path("name")]), [PROJECT_URI]);
        assert_eq!(DocProvider.touched(&[path("characters")]), [PROJECT_URI, CHARACTER_PREFIX]);
        assert_eq!(DocProvider.touched(&[DocPath::root()]), [PROJECT_URI, CHARACTER_PREFIX]);
        assert_eq!(DocProvider.touched(&[]), Vec::<String>::new());
    }
}
