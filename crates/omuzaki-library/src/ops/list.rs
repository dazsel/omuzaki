//! `library.list`: browse the library a section at a time.

use omuzaki_library_api::{ItemScope, Section};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::common::{failed, library};

/// Lists library items in section order, then id, with counts per section when no section is given (design D9).
#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "section": "hair" }, { "scope": "mine", "limit": 20 }]))]
pub struct LibraryList {
    /// Only this section (`hair`, `colourways`, `outfits`...).
    #[serde(default)]
    pub section: Option<Section>,
    /// Only this scope: `first_party` (comes with Omuzaki) or `mine` (your own library).
    #[serde(default)]
    pub scope: Option<ItemScope>,
    /// How many to skip.
    #[serde(default)]
    pub offset: usize,
    /// At most this many (default and most: 100).
    #[serde(default)]
    pub limit: usize,
}

impl Op for LibraryList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "library.list",
            summary: "List library items by section, with counts per section.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let page = library(ctx)?
            .list(self.section, self.scope.as_ref(), self.offset, self.limit)
            .map_err(|e| failed(&e))?;
        Ok(Plan::answer(serde_json::to_value(page).map_err(|e| OpError::internal(e.to_string()))?))
    }
}

register_op!(LibraryList);
