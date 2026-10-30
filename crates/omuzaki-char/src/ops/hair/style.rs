//! The hairstyle library's ops (#2486): `hair.style.list` shows the whole hairstyles, `hair.style.apply` cuts a hair
//! part in one of them.
//!
//! | Op | Does |
//! |---|---|
//! | `hair.style.list` | the styles, each with its name, what it looks like and its groups |
//! | `hair.style.apply` | replaces a hair part's groups and shared volume with a style's, keeping its colours, shading and physics |
//!
//! The apply writes the part's parameters back as one change (`part::changes`), so undo restores the style it had.

use omuzaki_char_api::{CharId, PartId};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::part;
use crate::hair::styles::{self, HairStyleError};
use crate::ops::character::access as character;
use crate::ops::character::access::not_found;
use crate::ops::part::describe::nearest;

/// The list op's name, for the hints of the apply.
const LIST: &str = "hair.style.list";

/// Lists the hairstyles of the library in id order: each one's id, name, a line saying what it looks like, and the
/// groups it is built from. Writes nothing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {}
    ],
    "x-omuzaki-fixture" = "demo"
))]
pub struct HairStyleList {}

impl Op for HairStyleList {
    fn meta() -> OpMeta {
        OpMeta {
            name: LIST,
            summary: "List the whole hairstyles (long straight, bob, twin tails, ponytail, braids, buns and more) with \
                      what each looks like.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        let listed: Vec<_> = library()?
            .iter()
            .map(|s| json!({ "id": s.id, "name": s.name, "summary": s.summary, "groups": s.groups.keys().collect::<Vec<_>>() }))
            .collect();
        Ok(Plan::answer(json!({ "styles": listed })))
    }
}

register_op!(HairStyleList);

/// Cuts a hair part in a whole hairstyle from the library: its groups and its shared parting, whorl and fullness are
/// replaced by the style's, and its colours, shading and physics are kept. `hair.style.list` shows the styles; every
/// group stays editable afterwards like any other.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "style": "bob"},
        {"character": "kit", "part": "hair", "style": "twin-tails"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairStyleApply {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The style's id, as `hair.style.list` shows it.
    pub style: String,
}

impl Op for HairStyleApply {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.style.apply",
            summary: "Cut a hair part in a whole hairstyle from the library (its groups, parting and volume), keeping \
                      its colours, shading and physics",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let style = match styles::find(&self.style) {
            Ok(style) => style,
            Err(HairStyleError::Unknown(_)) => {
                let ids: Vec<&str> = library()?.iter().map(|s| s.id.as_str()).collect();
                let near = nearest(&self.style, ids.iter().copied())
                    .map_or_else(String::new, |n| format!("did you mean `{n}`? "));
                return Err(not_found("style", format!("there is no hairstyle `{}`", self.style))
                    .hint(format!("{near}the styles are {}; {LIST} shows each", ids.join(", "))));
            }
            Err(e) => return Err(OpError::internal(e.to_string())),
        };
        let found = part::load(ctx, &self.character, &self.part)?;
        let after = style.applied_to(&found.hair);
        let changes = part::changes(ctx, &found, &after, &[], "style")?;
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "style": style.id,
            "groups": style.groups.keys().collect::<Vec<_>>(),
            "changed": !changes.is_empty(),
        });
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(HairStyleApply);

/// The library, or an internal error if it doesn't read (the crate's tests keep that from shipping).
fn library() -> Result<&'static [styles::HairStyle], OpError> {
    styles::list().map_err(|e| OpError::internal(e.to_string()))
}
