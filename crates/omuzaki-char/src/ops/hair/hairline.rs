//! The front hairline's shape (#2517): a curve across the forehead and how the hair thins toward it.
//!
//! | Op | Does |
//! |---|---|
//! | `hair.hairline.set` | a named shape (level, rounded, widow's peak, M-shape, straight) and any of its numbers |
//!
//! The shape lives on the scalp shell ([`CapGen::hairline`]); the shell, its thinning band and the baby hairs follow it.
//! It writes the shell's `hairline` back as one change (`part::changes`), so undo restores it exactly.

use omuzaki_char_api::{CharId, Generator, GroupId, Hairline, PartId};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::part;
use crate::ops::character::access as character;

/// Shapes the front hairline across the forehead. `shape` starts from a named one: `level` (the default line with
/// rounded corners), `rounded` (an arch, lower at the temples), `widows_peak` (a point dipping toward the brows),
/// `m_shape` (temples receding up) or `straight` (a flat line with a crisp edge). Then any of its numbers, in degrees
/// of the head's polar angle: `arch_deg` (`-20` to `20`, how much lower the temples sit), `peak_deg` (`0` to `20`) and
/// `peak_width` (`0.05` to `0.8` of the forehead), `temple_recess_deg` (`0` to `25`) and `density_fade_deg` (`0` to
/// `15`, how far above the line the hair starts to thin; `0` a hard edge). Numbers without a shape change the current
/// one. The height of the middle stays the scalp shell's `hairline_front_deg`. `group` names the scalp shell; the
/// first one when left out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "hair", "shape": "widows_peak"},
        {"character": "kit", "part": "hair", "shape": "rounded", "density_fade_deg": 6.0},
        {"character": "kit", "part": "hair", "temple_recess_deg": 8.0}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct HairHairlineSet {
    /// The character the hair belongs to.
    pub character: CharId,
    /// The hair part.
    pub part: PartId,
    /// The scalp shell; the first enabled one when left out.
    #[serde(default)]
    pub group: Option<GroupId>,
    /// A named shape to start from.
    #[serde(default)]
    pub shape: Option<String>,
    /// How much lower the temples sit than the middle, degrees.
    #[serde(default)]
    pub arch_deg: Option<f64>,
    /// How far a widow's peak dips, degrees.
    #[serde(default)]
    pub peak_deg: Option<f64>,
    /// The widow's peak's half width, a share of the forehead.
    #[serde(default)]
    pub peak_width: Option<f64>,
    /// How far the temples recede, degrees.
    #[serde(default)]
    pub temple_recess_deg: Option<f64>,
    /// How far above the hairline the hair thins, degrees.
    #[serde(default)]
    pub density_fade_deg: Option<f64>,
}

impl Op for HairHairlineSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hair.hairline.set",
            summary: "Shape the front hairline across the forehead (level, rounded, widow's peak, M-shape, straight, or \
                      its numbers) and how the hair thins toward it",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let numbers = [self.arch_deg, self.peak_deg, self.peak_width, self.temple_recess_deg, self.density_fade_deg];
        if self.shape.is_none() && numbers.iter().all(Option::is_none) {
            return Err(OpError::invalid("shape", "give a shape or one of the hairline's numbers")
                .hint(format!("shapes: {}", Hairline::PRESETS.join(", "))));
        }
        let found = part::load(ctx, &self.character, &self.part)?;
        let mut after = found.hair.clone();
        let id = match &self.group {
            Some(id) => id.clone(),
            None => after
                .groups
                .iter()
                .find(|(_, g)| g.enabled && matches!(g.generator, Generator::Cap(_)))
                .map(|(id, _)| id.clone())
                .ok_or_else(|| {
                    OpError::invalid("group", "the hair has no scalp shell to shape a hairline on")
                        .hint("add a cap group first (hair.group.add with a cap generator)")
                })?,
        };
        let Some(Generator::Cap(cap)) = after.groups.get_mut(&id).map(|g| &mut g.generator) else {
            return Err(OpError::invalid("group", format!("`{id}` isn't a scalp shell (a cap group) of this hair"))
                .hint("leave group out to shape the first scalp shell"));
        };
        let before = cap.hairline;
        let mut line = match &self.shape {
            Some(name) => Hairline::preset(name).ok_or_else(|| {
                OpError::invalid("shape", format!("there is no hairline shape `{name}`"))
                    .hint(format!("shapes: {}", Hairline::PRESETS.join(", ")))
            })?,
            None => before,
        };
        line.arch_deg = self.arch_deg.unwrap_or(line.arch_deg);
        line.peak_deg = self.peak_deg.unwrap_or(line.peak_deg);
        line.peak_width = self.peak_width.unwrap_or(line.peak_width);
        line.temple_recess_deg = self.temple_recess_deg.unwrap_or(line.temple_recess_deg);
        line.density_fade_deg = self.density_fade_deg.unwrap_or(line.density_fade_deg);
        cap.hairline = line;
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "group": id,
            "before": before,
            "after": line,
        });
        let steps = ["groups", id.as_str(), "generator", "hairline"];
        Ok(character::edit_plan(part::changes(ctx, &found, &after, &steps, "shape")?, summary))
    }
}

register_op!(HairHairlineSet);
