//! What the studio opens on: the project's character, or the sample character made for it when the project has none
//! (`shell.md` section 14, step 0: "the original sample character is already idling").
//!
//! The sample is made with the same `character.create` op an agent or the CLI would run, as Omuzaki itself
//! ([`Actor::System`]), so it is in the op log but not in the person's undo stack: Ctrl+Z never takes the character
//! away from under a first-time user.

use omuzaki_char_api::{CharId, CharacterDef, paths};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner, Services};
use serde_json::json;

use crate::wiring::{Document, runner_on};

/// The sample character's id.
pub const SAMPLE_ID: &str = "kit";
/// The sample character's display name.
pub const SAMPLE_NAME: &str = "Kit";
/// The body template the sample is made from: the kit's chibi default.
pub const SAMPLE_TEMPLATE: &str = "soft-sd";

/// The characters in the document, in id order.
///
/// # Errors
/// The runner's own error when the document can't be read.
pub fn characters(runner: &dyn Runner) -> Result<Vec<CharId>, OpError> {
    Ok(ids_in(runner.get(&characters_root(), &Grants::person())?.as_ref()))
}

fn characters_root() -> DocPath {
    DocPath::root().join("characters")
}

/// The character ids under the `characters` node, in id order.
fn ids_in(node: Option<&Value>) -> Vec<CharId> {
    let Some(Value::Object(entries)) = node else { return Vec::new() };
    let mut ids: Vec<CharId> = entries.keys().filter_map(|key| CharId::new(key).ok()).collect();
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    ids
}

/// The character the live stage is built on, read from `document` before the app's executor takes it (the performer
/// is built on that character's bind pose): the one [`open_character`] will open, which is the document's first, or
/// the sample exactly as `open_character` will make it, made here on a scratch document.
///
/// # Errors
/// [`OpError`] when the character doesn't read as one, or the sample can't be made.
pub fn character_to_stage(document: &Document) -> Result<(CharId, CharacterDef), OpError> {
    let read = |id: CharId, node: Option<Value>| match node {
        Some(node) => CharacterDef::from_node(&node)
            .map(|def| (id.clone(), def))
            .map_err(|e| OpError::invalid("character", format!("character '{id}' doesn't read as a character: {e}"))),
        None => Err(OpError::internal(format!("character '{id}' is listed but not in the document"))),
    };
    if let Some(first) = ids_in(document.get(&characters_root())).into_iter().next() {
        let node = document.get(&paths::character(&first)).cloned();
        return read(first, node);
    }
    let scratch = runner_on(Document::empty(), Services::default())
        .map_err(|e| OpError::internal(format!("the ops did not start: {e}")))?;
    let id = open_character(scratch.as_ref())?;
    let node = scratch.get(&paths::character(&id), &Grants::person())?;
    read(id, node)
}

/// The character the studio opens: the document's first, or the sample, made now when there is none.
///
/// # Errors
/// The runner's error when the document can't be read or `character.create` fails.
pub fn open_character(runner: &dyn Runner) -> Result<CharId, OpError> {
    if let Some(first) = characters(runner)?.into_iter().next() {
        return Ok(first);
    }
    let id = CharId::new(SAMPLE_ID).map_err(|e| OpError::internal(format!("the sample id is not an id: {e}")))?;
    let request = Request {
        op: "character.create".into(),
        args: json!({"id": SAMPLE_ID, "name": SAMPLE_NAME, "template": SAMPLE_TEMPLATE}),
        mode: Mode::Apply,
        actor: Actor::System,
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    };
    runner.run(request)?;
    if runner.get(&paths::character(&id), &Grants::person())?.is_none() {
        return Err(OpError::internal("character.create succeeded but the sample is not in the document"));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner() -> std::sync::Arc<dyn Runner> {
        omuzaki_char::link();
        runner_on(Document::empty(), Services::default()).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn an_empty_project_opens_on_the_sample_which_the_person_cannot_undo_away() {
        let runner = runner();
        let id = open_character(runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(id.as_str(), SAMPLE_ID);
        assert_eq!(characters(runner.as_ref()).unwrap_or_else(|e| panic!("{e}")), vec![id.clone()]);
        let you = Actor::Person { name: "you".into() };
        assert_eq!(runner.undo(&you, &Grants::person()).unwrap_or_else(|e| panic!("{e}")), None);
        assert!(runner.get(&paths::character(&id), &Grants::person()).unwrap_or_else(|e| panic!("{e}")).is_some());
    }

    #[test]
    fn a_project_with_a_character_opens_on_it_and_makes_none() {
        let runner = runner();
        let first = open_character(runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
        let again = open_character(runner.as_ref()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(first, again);
        assert_eq!(characters(runner.as_ref()).unwrap_or_else(|e| panic!("{e}")).len(), 1);
    }

    #[test]
    fn the_stage_gets_the_character_the_studio_opens_on() {
        omuzaki_char::link();
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let project = || Document::project(dir.path()).unwrap_or_else(|e| panic!("{e}"));
        // A new project: the sample, as the studio will make it.
        let (id, staged) = character_to_stage(&project()).unwrap_or_else(|e| panic!("{e}"));
        let runner = runner_on(project(), Services::default()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(open_character(runner.as_ref()).unwrap_or_else(|e| panic!("{e}")), id);
        let made = runner.get(&paths::character(&id), &Grants::person()).unwrap_or_else(|e| panic!("{e}"));
        let made =
            CharacterDef::from_node(&made.unwrap_or_else(|| panic!("no sample"))).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(staged, made);
        // A project that has a character: that one, as it was left.
        let you = Actor::Person { name: "you".into() };
        let set = Request {
            op: "character.rename".into(),
            args: json!({"character": id.as_str(), "name": "Renamed"}),
            mode: Mode::Apply,
            actor: you,
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        runner.run(set).unwrap_or_else(|e| panic!("{e}"));
        drop(runner);
        let (again, staged) = character_to_stage(&project()).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(again, id);
        assert_eq!(staged.name, "Renamed");
    }
}
