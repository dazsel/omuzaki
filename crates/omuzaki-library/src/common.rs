//! What the ops share: reaching the library, reading ids, and the JSON an item row or a record shows.

use omuzaki_char_api::CharId;
use omuzaki_library_api::{ApplicationRecord, CharacterFacts, ItemId, ItemInfo, Library, LibraryError, Version};
use omuzaki_ops_api::{OpCtx, OpError};
use serde_json::{Value, json};

use crate::HostLibrary;
use crate::apply::library_error;

/// The library service the app injected.
pub(crate) fn library<'a>(ctx: &OpCtx<'a>) -> Result<&'a dyn Library, OpError> {
    ctx.service::<dyn Library>()
}

/// The app's own library, for what only it can do (install and uninstall write the person's folder).
pub(crate) fn host<'a>(ctx: &OpCtx<'a>) -> Result<&'a HostLibrary, OpError> {
    ctx.service::<HostLibrary>()
}

pub(crate) fn item_id(field: &str, text: &str) -> Result<ItemId, OpError> {
    ItemId::new(text).map_err(|e| OpError::invalid(field, e.to_string()).hint("an item id is `publisher/name`"))
}

pub(crate) fn version(field: &str, text: &str) -> Result<Version, OpError> {
    Version::parse(text).map_err(|e| OpError::invalid(field, e.to_string()))
}

/// The facts of `character` in the document, or `NotFound`.
pub(crate) fn facts(ctx: &OpCtx<'_>, character: &CharId) -> Result<CharacterFacts, OpError> {
    CharacterFacts::read(ctx.doc, character).ok_or_else(|| {
        OpError::not_found(format!("no kit character {character} in this project"))
            .hint("list them with character.list")
    })
}

/// A library error as an op error on no field.
pub(crate) fn failed(e: &LibraryError) -> OpError {
    library_error("", e)
}

/// A row as JSON.
pub(crate) fn row(info: &ItemInfo) -> Value {
    serde_json::to_value(info).unwrap_or(Value::Null)
}

/// One line on what an item's rights mean for what the person makes with it (design D8).
pub(crate) fn rights_note(info_ok: bool, licence: &str) -> String {
    if info_ok {
        format!("{licence}: what you make with it may be published")
    } else {
        format!("{licence}: what this makes can't be published (io.check_export will refuse it); personal use is fine")
    }
}

/// A record as the ops show it.
pub(crate) fn record_json(character: &CharId, app: &str, record: &ApplicationRecord) -> Value {
    json!({
        "character": character,
        "application": app,
        "item": record.item,
        "version": record.version.to_string(),
        "hash": record.hash.to_string(),
        "from": record.from,
        "slot": record.slot,
        "created": record.created,
        "params": record.params,
    })
}
