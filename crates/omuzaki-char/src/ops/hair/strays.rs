//! The hair's loose strands (#2516): flyaways off the outer locks and baby hairs along the front hairline.
//!
//! | Op | Does |
//! |---|---|
//! | `hair.strays.set` | how many locks shed a flyaway, how far they stray and how frizzy they are, and the baby hairs |
//!
//! It writes the part's `strays` back as one change (`part::changes`), so undo restores it exactly.

use omuzaki_char_api::{CharId, HairStrays, PartId};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::part;
use crate::ops::character::access as character;

/// Sets the hair's loose strands: `share` (`0` to `1`, the share of outer locks that shed a flyaway; `0` none), `stray`
/// (metres, `0.002` to `0.04`, how far a flyaway strays from its lock by its tip), `frizz` (`0` to `1`, kinks along
/// each), `baby` (`0` to `200` baby hairs along the scalp shell's front hairline) and `baby_length` (metres, `0.003`
/// to `0.03`). Give any of them; the rest keep their values. `reset` puts back the defaults (a few flyaways, no baby
/// hairs).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "share": 0.3, "frizz": 0.5},
        {"character": "kit", "part": "hair", "baby": 40, "baby_length": 0.012}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairStraysSet {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The share of outer locks that shed a flyaway.
    #[serde(default)]
    pub share: Option<f64>,
    /// How far a flyaway strays by its tip, metres.
    #[serde(default)]
    pub stray: Option<f64>,
    /// How frizzy the strands are.
    #[serde(default)]
    pub frizz: Option<f64>,
    /// How many baby hairs.
    #[serde(default)]
    pub baby: Option<u32>,
    /// How long the baby hairs are, metres.
    #[serde(default)]
    pub baby_length: Option<f64>,
    /// Put back the defaults.
    #[serde(default)]
    pub reset: bool,
}

impl Op for HairStraysSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.strays.set",
            summary: "Set the hair's loose strands: the flyaways off the outer locks (share, stray, frizz) and the \
                      baby hairs along the front hairline",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let given = self.share.is_some()
            || self.stray.is_some()
            || self.frizz.is_some()
            || self.baby.is_some()
            || self.baby_length.is_some();
        if given == self.reset {
            return Err(OpError::invalid(
                "share",
                if self.reset { "reset takes nothing else" } else { "give share, stray, frizz, baby, baby_length or reset" },
            )
            .hint("for example {\"share\": 0.3, \"frizz\": 0.5}"));
        }
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let before = after.strays.clone();
        if self.reset {
            after.strays = HairStrays::default();
        } else {
            let s = &mut after.strays;
            s.share = self.share.unwrap_or(s.share);
            s.stray = self.stray.unwrap_or(s.stray);
            s.frizz = self.frizz.unwrap_or(s.frizz);
            s.baby = self.baby.unwrap_or(s.baby);
            s.baby_length = self.baby_length.unwrap_or(s.baby_length);
        }
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "before": before,
            "after": after.strays,
        });
        Ok(character::edit_plan(part::changes(ctx, &found, &after, &["strays"], "share")?, summary))
    }
}

register_op!(HairStraysSet);
