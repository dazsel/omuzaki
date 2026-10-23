//! `library.search`: find items by text, tags and filters.

use omuzaki_library_api::{CompatTarget, Query};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::common::{facts, failed, library};

/// Searches the library (design D13): free text over titles, tags, summaries and ids in every language, with
/// `tags`, `section`, `kind` and `scope` filters. `compatible_with: {"character": "<id>"}` drops what doesn't fit that
/// character (style mismatches stay, ranked lower, with the warning); `usable_for: "publish"` drops what would block
/// publishing. Every answer carries counts per facet over all matches.
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(extend("examples" = [{ "text": "twin tails" }, { "section": "colourways", "usable_for": "publish" }]))]
pub struct LibrarySearch(pub Query);

impl Op for LibrarySearch {
    fn meta() -> OpMeta {
        OpMeta {
            name: "library.search",
            summary: "Search library items by text, tags, section and fit with a character.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let character = match &self.0.compatible_with {
            Some(CompatTarget::Character(id)) => Some(facts(ctx, id)?),
            None => None,
        };
        let page = library(ctx)?.search_for(&self.0, character.as_ref()).map_err(|e| failed(&e))?;
        Ok(Plan::answer(serde_json::to_value(page).map_err(|e| OpError::internal(e.to_string()))?))
    }
}

register_op!(LibrarySearch);
