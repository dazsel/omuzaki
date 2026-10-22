//! What the studio opens on: the project's character, or the sample character made for it when the project has none
//! (`shell.md` section 14, step 0: "the original sample character is already idling").
//!
//! The sample is made with the same `character.create` op an agent or the CLI would run, as Omuzaki itself
//! ([`Actor::System`]), so it is in the op log but not in the person's undo stack: Ctrl+Z never takes the character
//! away from under a first-time user.

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner};
use serde_json::json;

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
    let root = DocPath::root().join("characters");
    let Some(Value::Object(entries)) = runner.get(&root, &Grants::person())? else { return Ok(Vec::new()) };
    let mut ids: Vec<CharId> = entries.keys().filter_map(|key| CharId::new(key).ok()).collect();
    ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(ids)
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
    use omuzaki_ops_api::Services;

    use super::*;
    use crate::wiring::{Document, runner_on};

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
}
