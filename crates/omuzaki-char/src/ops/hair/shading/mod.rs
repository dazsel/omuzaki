//! The hair colour ops (CB-34): the shading of a hair part, its streaks, and recolouring it through the palette.
//!
//! | Op | Does |
//! |---|---|
//! | `hair.set_shading` | one field of the part's [`HairShading`]: a colour, the ring, the strand lines, the edges, keep identity, the outline, brows through, inner darkening |
//! | `hair.streak.add` | adds a streak (a share of the clumps tinted), returning its index |
//! | `hair.streak.set` | one field of a streak |
//! | `hair.streak.remove` | removes a streak |
//! | `hair.recolour` | the root, mid and tip colours at once, through the palette's `hair_root`, `hair_mid` and `hair_tip` slots by default, so a colourway swap recolours the hair too |
//!
//! Each writes the part's `shading` back as one change (`part::changes`), so undo restores it exactly. Colours reach
//! the materials as resolved colours: a palette change recolours the hair without rebuilding its geometry.

use omuzaki_char_api::{CharId, HairShading, PartId, Streak};
use omuzaki_doc_api::Value;
use omuzaki_face_api::{ColourRef, PaletteSlot};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::part::{self, HairPart};
use crate::ops::character::access as character;
use crate::palette::ops::slot_path;

/// The most streaks a hair part may have.
pub const MAX_STREAKS: usize = 16;

/// The changes that store `shading` as the part's.
fn write(ctx: &OpCtx<'_>, found: &HairPart, shading: HairShading, field: &str) -> Result<Vec<omuzaki_doc_api::Change>, OpError> {
    let mut after = found.hair.clone();
    after.shading = shading;
    part::changes(ctx, found, &after, &["shading"], field)
}

/// Sets one field of a hair part's shading: `root`, `mid`, `tip` (the gradient), `shade` (the shadowed side), `line`
/// (the strand lines), `tie`, `outline`, `ring` (the angel ring's colour), `ring_strength`, `strand_lines`,
/// `edge_darkening`, `keep_identity` (how much of the hair's colour survives coloured light), `inner_darkening`, all
/// `0` to `1`; the angel ring's shape, `ring_width` (radians, `0.01` to `0.3`), `ring_wobble` (radians, up to `0.2`)
/// and `ring_teeth` (`0` to `1`), `ring_follow` (`0` paints it at one height, `1` makes it follow each lock's strands)
/// and `ring_shift` (`-2` to `2`: where the following ring sits, lower toward 0); the strands, `normal_blend` (`0` to
/// `1`: how far the locks shade as one mass rather than as tubes of their own), `edge_feather` (`0` to `0.8`: how much the locks' sides thin out), `strands` (`8` to
/// `64` across a lock), `strand_gap` (`0` to `0.6`), `strand_grain` (`0` to `0.3`) and `tip_length` (metres, `0.002` to
/// `0.05`); `outline_width` (metres, up to 0.01) and `brows_through` (whether the brows show through the fringe).
/// A colour is `#rrggbb` or a palette slot, `@hair_root`. Streaks have their own ops (`hair.streak.*`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "field": "ring_strength", "value": 0.4}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairSetShading {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The shading field to set.
    pub field: String,
    /// The new value, of the type the field takes.
    pub value: Value,
}

impl Op for HairSetShading {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.set_shading",
            summary: "Set one field of a hair part's shading: a colour (root, mid, tip, shade, line, tie, outline, \
                      ring), ring_strength, ring_width, ring_wobble, ring_teeth, ring_follow, ring_shift, strand_lines, \
                      edge_darkening, keep_identity, inner_darkening, normal_blend, edge_feather, strands, strand_gap, \
                      strand_grain, tip_length, outline_width or brows_through",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        let before = &found.hair.shading;
        let after = part::with_field(before, &self.field, &self.value, &["streaks"])?;
        let read = |s: &HairShading| serde_json::to_value(s).ok().and_then(|v| v.get(&self.field).cloned());
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "field": self.field,
            "before": read(before),
            "after": read(&after),
        });
        Ok(character::edit_plan(write(ctx, &found, after, "value")?, summary))
    }
}

register_op!(HairSetShading);

/// Adds a coloured streak to a hair part: a `fraction` of the clumps (of `groups`, or of every group when it is empty)
/// tinted `colour` at `strength`, by `blend` (`replace` or `multiply`). Which clumps take it is chosen by `seed`, the
/// same on every build. Every field is optional: the default is 20% purple at 0.6. The answer gives the new streak's
/// index, for `hair.streak.set` and `hair.streak.remove`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "streak": {"fraction": 0.2, "colour": "#9b6ed6"}}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairStreakAdd {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The streak; fields left out take the defaults.
    #[serde(default)]
    pub streak: Value,
}

impl Op for HairStreakAdd {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.streak.add",
            summary: "Add a coloured streak to a hair part: a share of the clumps tinted a colour, chosen by a seed",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut shading = found.hair.shading.clone();
        if shading.streaks.len() >= MAX_STREAKS {
            return Err(OpError::invalid("part", format!("`{}` already has {MAX_STREAKS} streaks", self.part))
                .hint("remove one with hair.streak.remove, or change one with hair.streak.set"));
        }
        let given = if self.streak.is_null() { json!({}) } else { self.streak.clone() };
        let mut streak = serde_json::to_value(Streak::default())
            .map_err(|e| OpError::internal(format!("serialising a streak failed: {e}")))?;
        let (Some(fields), Some(given)) = (streak.as_object_mut(), given.as_object()) else {
            return Err(OpError::invalid("streak", "a streak is an object of its fields")
                .hint("for example {\"fraction\": 0.2, \"colour\": \"#9b6ed6\"}"));
        };
        for (name, value) in given {
            if !fields.contains_key(name) {
                let names: Vec<&str> = fields.keys().map(String::as_str).collect();
                return Err(OpError::invalid("streak", format!("`{name}` is not a streak field"))
                    .hint(format!("the streak fields are: {}", names.join(", "))));
            }
            fields.insert(name.clone(), value.clone());
        }
        let streak: Streak =
            serde_json::from_value(streak).map_err(|e| {
                OpError::invalid("streak", format!("not a streak: {e}")).hint("hair.streak.add takes the streak's fields")
            })?;
        shading.streaks.push(streak.clone());
        let index = shading.streaks.len() - 1;
        let summary = json!({ "character": self.character, "part": self.part, "index": index, "streak": streak });
        Ok(character::edit_plan(write(ctx, &found, shading, "streak")?, summary))
    }
}

register_op!(HairStreakAdd);

/// Sets one field of a hair part's streak `index` (from 0, in the order they were added): `fraction`, `colour`,
/// `strength`, `groups`, `seed` or `blend`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "index": 0, "field": "fraction", "value": 0.3}
    ],
    "x-omuzaki-fixture" = "hair-streaks"
))]
pub struct HairStreakSet {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// Which streak, from 0.
    pub index: usize,
    /// The streak field to set.
    pub field: String,
    /// The new value, of the type the field takes.
    pub value: Value,
}

impl Op for HairStreakSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.streak.set",
            summary: "Set one field of a hair part's streak: fraction, colour, strength, groups, seed or blend",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut shading = found.hair.shading.clone();
        let streak = streak_at(&mut shading, self.index, &self.part)?;
        let before = streak.clone();
        *streak = part::with_field(&before, &self.field, &self.value, &[])?;
        let after = streak.clone();
        let read = |s: &Streak| serde_json::to_value(s).ok().and_then(|v| v.get(&self.field).cloned());
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "index": self.index,
            "field": self.field,
            "before": read(&before),
            "after": read(&after),
        });
        Ok(character::edit_plan(write(ctx, &found, shading, "value")?, summary))
    }
}

register_op!(HairStreakSet);

/// Removes a hair part's streak `index` (from 0); the ones after it move down one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "index": 0}
    ],
    "x-omuzaki-fixture" = "hair-streaks"
))]
pub struct HairStreakRemove {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// Which streak, from 0.
    pub index: usize,
}

impl Op for HairStreakRemove {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.streak.remove",
            summary: "Remove a hair part's streak by its index",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut shading = found.hair.shading.clone();
        streak_at(&mut shading, self.index, &self.part)?;
        let removed = shading.streaks.remove(self.index);
        let summary = json!({ "character": self.character, "part": self.part, "index": self.index, "removed": removed });
        Ok(character::edit_plan(write(ctx, &found, shading, "index")?, summary))
    }
}

register_op!(HairStreakRemove);

/// Streak `index` of `shading`.
fn streak_at<'a>(shading: &'a mut HairShading, index: usize, part: &PartId) -> Result<&'a mut Streak, OpError> {
    let count = shading.streaks.len();
    shading.streaks.get_mut(index).ok_or_else(|| {
        let has = if count == 0 { "no streaks".to_owned() } else { format!("{count} streaks, numbered from 0") };
        character::not_found("index", format!("`{part}` has {has}")).hint("add one with hair.streak.add")
    })
}

/// The palette slots `hair.recolour` writes, in the order root, mid, tip.
const SLOTS: [&str; 3] = ["hair_root", "hair_mid", "hair_tip"];

/// Recolours a hair part's gradient at once: `root`, `mid` and `tip`, each `#rrggbb` (any left out keeps its colour).
/// By default the colours go into the palette's `hair_root`, `hair_mid` and `hair_tip` slots (added when missing) and
/// the part refers to them, so a colourway swap recolours the hair too; `slots: false` writes them into the part alone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "root": "#3a2a4a", "mid": "#6a5a8a", "tip": "#a898c8"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairRecolour {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The colour at the root.
    #[serde(default)]
    pub root: Option<String>,
    /// The colour in the middle.
    #[serde(default)]
    pub mid: Option<String>,
    /// The colour at the tip.
    #[serde(default)]
    pub tip: Option<String>,
    /// Whether the colours go through the palette's hair slots (the default) or into the part alone.
    #[serde(default = "yes")]
    pub slots: bool,
}

fn yes() -> bool {
    true
}

impl Op for HairRecolour {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.recolour",
            summary: "Recolour a hair part's root, mid and tip at once, through the palette's hair slots by default so \
                      a colourway swap recolours the hair too",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let given = [("root", &self.root), ("mid", &self.mid), ("tip", &self.tip)];
        if given.iter().all(|(_, c)| c.is_none()) {
            return Err(OpError::invalid("root", "give at least one of root, mid and tip")
                .hint("for example {\"root\": \"#3a2a4a\"}"));
        }
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut shading = found.hair.shading.clone();
        let mut palette_changes = Vec::new();
        let mut colours = serde_json::Map::new();
        for ((field, text), slot) in given.iter().zip(SLOTS) {
            let Some(text) = text else { continue };
            let colour: omuzaki_core::colour::Colour = text.parse().map_err(|_| {
                OpError::invalid(*field, format!("`{text}` isn't a colour"))
                    .hint("write it as #rrggbb or #rrggbbaa, e.g. #6a5a8a")
            })?;
            colours.insert((*field).to_owned(), json!(colour));
            let reference = if self.slots {
                let slot = PaletteSlot::new(slot).map_err(|e| OpError::internal(format!("a hair slot: {e}")))?;
                let path = slot_path(&self.character, &slot);
                if found.def.palette.0.get(&slot) != Some(&colour) {
                    let change = character::change(ctx, path, Some(json!(colour)));
                    if change.before.is_none() {
                        palette_changes.extend(character::create(ctx, change.path, json!(colour)));
                    } else {
                        palette_changes.push(change);
                    }
                }
                ColourRef::Slot(slot)
            } else {
                ColourRef::Fixed(colour)
            };
            match *field {
                "root" => shading.root = reference,
                "mid" => shading.mid = reference,
                _ => shading.tip = reference,
            }
        }
        let mut changes = palette_changes;
        changes.extend(write(ctx, &found, shading, "root")?);
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "colours": colours,
            "through_palette": self.slots,
        });
        Ok(character::edit_plan(changes, summary))
    }
}

register_op!(HairRecolour);
