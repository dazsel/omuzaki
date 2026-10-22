//! The go-live notice (`docs/design/byom.md` D5, ADR 0033 decision 7, #1089): `obs.stream.start` says when a character
//! the scenes cast is an imported model whose own terms allow personal, non-profit use only. It is the second of two
//! notices (the first is at import) and, like the first, it never blocks and never changes what is sent to OBS:
//! performing is the person's call.
//!
//! The document has no "current scene" (OBS's program scene is OBS's), so every scene's cast counts: scenes in id order,
//! members in member order, each character once.

use omuzaki_char_api::CharId;
use omuzaki_doc_api::{DocPath, DocRead};
use omuzaki_io_api::{Finding, UseTerms};
use omuzaki_stage_api::CastMember;
use serde_json::Value;

/// The reserved part id of an imported model's own source row (`omuzaki_avatar_api::anchors::AVATAR_PART`).
pub(crate) const AVATAR_PART: &str = "avatar";

/// The rule the notice carries, as the import shows it.
const NON_PROFIT: &str = "rights.perform_non_profit";

/// One `rights.perform_non_profit` finding for each cast character whose `sources/avatar` row has non-profit terms,
/// located at that row (`characters/<id>/sources/avatar`). A character without the row (a kit character, or an import
/// that recorded no terms) or with terms that allow profit gives nothing.
pub(crate) fn non_profit_warnings(doc: &dyn DocRead) -> Vec<Finding> {
    let mut seen: Vec<CharId> = Vec::new();
    for scene in
        doc.get(&DocPath::root().join("scenes")).and_then(Value::as_object).into_iter().flat_map(|s| s.values())
    {
        let cast = scene.get("cast").and_then(Value::as_object).into_iter().flat_map(|c| c.values());
        for member in cast.filter_map(|m| serde_json::from_value::<CastMember>(m.clone()).ok()) {
            if !seen.contains(&member.character) {
                seen.push(member.character);
            }
        }
    }
    seen.iter()
        .flat_map(|character| {
            let row = DocPath::root().join("characters").join(character.as_str()).join("sources").join(AVATAR_PART);
            let terms = doc.get(&row).and_then(|r| r.get("terms")).cloned();
            let terms = terms.and_then(|t| serde_json::from_value::<UseTerms>(t).ok());
            terms.map(|t| t.perform_findings()).unwrap_or_default().into_iter().filter(|f| f.rule == NON_PROFIT).map(
                move |f| {
                    let message = format!("`{character}`: {}", f.message);
                    Finding { message, ..f }.at(row.to_string())
                },
            )
        })
        .collect()
}
