//! `character.set_age` (#2615): age as one control that drives every part.

use omuzaki_char_api::CharId;
use omuzaki_char_api::design::{AGE_RANGE, design_library};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{load, store};
use crate::ops::character::access;

/// Sets a character's age in years, one control for every part, through the anatomy: a child's full cheeks, small jaw
/// and nose; then fat pads that deflate and slide toward the jowl, bone that recedes round the eye and the jaw, a
/// lengthening nose, and folds cut into the skin (the nasolabial fold, marionette lines, crow's feet, the forehead);
/// with the skin's cavities, spots and flush and the hair's grey, all re-derived at build time and added to whatever
/// the anatomy, the dials and the layers already say. Leave `age` out to remove it (the
/// template's own look).
///
/// Works in every look: the shape moves the same way, and how far the folds and spots show follows the look's style
/// (a cel look keeps only the strongest).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "age": 8.0},
        {"character": "kit", "age": 68.0},
        {"character": "kit"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct CharacterSetAge {
    /// The character.
    pub character: CharId,
    /// The age in years, 3 to 100. Left out: no age (the template's own look).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(range(min = 3.0, max = 100.0))]
    pub age: Option<f64>,
}

impl Op for CharacterSetAge {
    fn meta() -> OpMeta {
        OpMeta {
            name: "character.set_age",
            summary: "Set a character's age in years: one control that reshapes the face and body and ages the skin and hair.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (_, mut design) = load(ctx, &self.character)?;
        let (lo, hi) = AGE_RANGE;
        if let Some(age) = self.age
            && !(age.is_finite() && (lo..=hi).contains(&age))
        {
            return Err(OpError::invalid("age", format!("{age} is outside {lo} to {hi} years"))
                .hint("give an age from 3 to 100, or leave `age` out to remove it"));
        }
        design.age = self.age;
        let changes = store(ctx, &self.character, &design)?;
        let moved: Vec<String> = match (self.age, design_library()) {
            (Some(age), Ok(lib)) => lib
                .structures
                .iter()
                .filter(|s| s.at_age(age).abs() > 0.05)
                .map(|s| format!("{} {:+.1}", s.id, s.at_age(age)))
                .chain(lib.dials.iter().filter(|d| d.at_age(age).abs() > 0.02).map(|d| format!("{} {:+.2}", d.id, d.at_age(age))))
                .collect(),
            _ => Vec::new(),
        };
        let layers = self.age.and_then(|age| design_library().ok().map(|l| l.age.at(age)));
        let summary = json!({
            "character": self.character, "age": self.age, "anatomy": moved,
            "skin": layers.map(|l| json!({"grey": l.grey, "wrinkles": l.wrinkles, "spots": l.spots, "flush": l.flush})),
            "changed": !changes.is_empty(),
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(CharacterSetAge);
