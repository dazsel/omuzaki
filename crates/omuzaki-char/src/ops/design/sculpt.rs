//! Sculpting (#2613): `part.sculpt.stroke`, `part.sculpt.clear` and `part.sculpt.list`.
//!
//! A stroke is stored in the part's own coordinates (a body region's length and angle, the head's proportions, or the
//! part's box), not as moved vertices, so it survives every slider and template change: the part rebuilds and the
//! stroke is placed again on its new surface. Each stroke is one op, so undo takes back one stroke.

use omuzaki_char_api::design::{Brush, PartPoint, SculptStroke, locate};
use omuzaki_char_api::{BuildOptions, CharId, PartId};
use omuzaki_core::glam::Vec3;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{load, store};
use crate::ops::character::access;
use crate::ops::weights::geometry_services;

/// Where a stroke (or a decal) goes: a bind-space point in metres (a click in the viewport), or a point already in the
/// part's coordinates.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum StrokeAt {
    /// A bind-space point `[x, y, z]` in metres (x the character's left, y up, z forward): it is located on the part
    /// and stored in the part's coordinates.
    Point([f32; 3]),
    /// A point in the part's coordinates, stored as given.
    Part(PartPoint),
}

/// `at` in the part's coordinates, locating a bind-space point on `part` as the character stands.
///
/// # Errors
/// `invalid` (field `at`) if the point can't be located (the part doesn't build), or the part point is invalid.
pub(crate) fn part_point(ctx: &OpCtx<'_>, character: &CharId, part: &PartId, at: &StrokeAt) -> Result<PartPoint, OpError> {
    let point = match at {
        StrokeAt::Part(p) => *p,
        StrokeAt::Point(p) => {
            if !Vec3::from(*p).is_finite() || p.iter().any(|v| v.abs() > 10.0) {
                return Err(OpError::invalid("at", "the point must be three numbers from -10 to 10 metres")
                    .hint("give a point on or near the part, in metres, as the viewport shows it"));
            }
            let def = access::load(ctx, character)?;
            locate(character, &def, part, Vec3::from(*p), &BuildOptions::default(), &geometry_services()).map_err(|e| {
                OpError::invalid("at", format!("the point can't be placed on `{part}`: {e}"))
                    .hint("give the point as a part point instead ({\"on\": \"head\", \"dir\": [x, y, z]})")
            })?
        }
    };
    point.check().map_err(|e| OpError::invalid(format!("at/{}", e.split(':').next().unwrap_or("")), e))?;
    Ok(point)
}

/// Sculpts a part with one brush stroke: `grab` (move along `direction`), `inflate` (out along the normal, in if
/// negative), `crease` (a groove, a ridge if negative), `flatten`, `pinch` or `smooth` (which relaxes the strokes
/// before it). `radius` and displacement `strength` are fractions of the local scale (a limb's radius, the head's), so
/// the stroke keeps its size on every template; `symmetric` mirrors it onto the other side.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "head", "brush": "inflate", "at": {"on": "head", "dir": [0.45, -0.25, 0.85]}, "radius": 0.25, "strength": 0.04, "symmetric": true},
        {"character": "kit", "part": "body", "brush": "inflate", "at": {"on": "body", "region": "leg_left", "t": 0.7, "angle": 3.14}, "radius": 0.9, "strength": 0.12},
        {"character": "kit", "part": "head", "brush": "grab", "at": [0.0, 1.0, 0.1], "radius": 0.3, "strength": 0.03, "direction": [0.0, 0.0, 1.0]}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct PartSculptStroke {
    /// The character.
    pub character: CharId,
    /// The part to sculpt (any part; a simulated mesh, such as loose hair, keeps its shape).
    pub part: PartId,
    /// The brush.
    pub brush: Brush,
    /// Where: a bind-space point in metres, or a part point.
    pub at: StrokeAt,
    /// The radius, as a fraction of the local scale (`0` to `4`).
    pub radius: f32,
    /// How hard: a displacement as a fraction of the local scale (`-1` to `1`) for grab, inflate and crease; `0` to `1`
    /// for flatten, pinch and smooth.
    pub strength: f32,
    /// For `grab`: the direction to move, in bind space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direction: Option<[f32; 3]>,
    /// Mirror the stroke onto the other side as well.
    #[serde(default)]
    pub symmetric: bool,
}

impl Op for PartSculptStroke {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.sculpt.stroke",
            summary: "Sculpt a part with one brush stroke (grab, inflate, crease, flatten, pinch, smooth) that survives every slider and template change.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (def, mut design) = load(ctx, &self.character)?;
        access::part(&def, &self.character, &self.part, "part")?;
        let at = part_point(ctx, &self.character, &self.part, &self.at)?;
        let stroke = SculptStroke {
            brush: self.brush,
            at,
            radius: self.radius,
            strength: self.strength,
            direction: self.direction,
            symmetric: self.symmetric,
        };
        stroke.check().map_err(|e| {
            let field = e.split(':').next().unwrap_or("strength").to_owned();
            OpError::invalid(field, e).hint("radius is 0 to 4, strength ±1 (0 to 1 for flatten, pinch and smooth); grab needs a direction")
        })?;
        let strokes = design.sculpt.entry(self.part.clone()).or_default();
        strokes.push(stroke.clone());
        let count = strokes.len();
        let changes = store(ctx, &self.character, &design)?;
        let summary = json!({ "character": self.character, "part": self.part, "stroke": stroke, "strokes": count });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(PartSculptStroke);

/// Removes sculpt strokes: the stroke at `index` of a part, every stroke of a part, or (with no part) every stroke of
/// the character.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "head", "index": 0},
        {"character": "kit", "part": "head"},
        {"character": "kit"}
    ],
    "x-omuzaki-fixture" = "character-design"
))]
pub struct PartSculptClear {
    /// The character.
    pub character: CharId,
    /// The part whose strokes are removed. Left out: every part's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<PartId>,
    /// The stroke to remove, counted from 0 in the order they were made. Left out: all of the part's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub index: Option<usize>,
}

impl Op for PartSculptClear {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.sculpt.clear",
            summary: "Remove one sculpt stroke, a part's strokes, or every stroke of a character.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (def, mut design) = load(ctx, &self.character)?;
        let before: usize = design.sculpt.values().map(Vec::len).sum();
        match (&self.part, self.index) {
            (None, Some(_)) => {
                return Err(OpError::invalid("index", "an index needs the part it counts in").hint("name the part"));
            }
            (None, None) => design.sculpt.clear(),
            (Some(part), index) => {
                access::part(&def, &self.character, part, "part")?;
                let strokes = design.sculpt.entry(part.clone()).or_default();
                match index {
                    None => strokes.clear(),
                    Some(i) if i < strokes.len() => {
                        strokes.remove(i);
                    }
                    Some(i) => {
                        return Err(OpError::invalid("index", format!("`{part}` has {} stroke(s), so there is no stroke {i}", strokes.len()))
                            .hint("part.sculpt.list numbers them"));
                    }
                }
                if strokes.is_empty() {
                    design.sculpt.remove(part);
                }
            }
        }
        let after: usize = design.sculpt.values().map(Vec::len).sum();
        let changes = store(ctx, &self.character, &design)?;
        Ok(access::edit_plan(changes, json!({ "character": self.character, "removed": before - after, "strokes": after })))
    }
}

register_op!(PartSculptClear);

/// Lists a character's sculpt strokes by part, in the order they were made. Writes nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "part": "head"}
    ],
    "x-omuzaki-fixture" = "character-design"
))]
pub struct PartSculptList {
    /// The character.
    pub character: CharId,
    /// Only this part's strokes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part: Option<PartId>,
}

impl Op for PartSculptList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "part.sculpt.list",
            summary: "List a character's sculpt strokes by part, numbered in the order they were made.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let (def, design) = load(ctx, &self.character)?;
        if let Some(part) = &self.part {
            access::part(&def, &self.character, part, "part")?;
        }
        let parts: serde_json::Map<String, serde_json::Value> = design
            .sculpt
            .iter()
            .filter(|(p, _)| self.part.as_ref().is_none_or(|q| q == *p))
            .map(|(p, s)| (p.to_string(), json!(s)))
            .collect();
        Ok(Plan::answer(json!({ "character": self.character, "parts": parts })))
    }
}

register_op!(PartSculptList);
