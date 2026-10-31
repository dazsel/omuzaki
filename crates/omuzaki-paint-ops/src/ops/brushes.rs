//! `paint.brushes`: the painter's kit, so an agent knows what it can draw with.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use omuzaki_paint::{BlendMode, presets};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::query;
use crate::doc::json;

/// List the brush presets (what each is for and all its settings), the blend modes, and the kinds of marks
/// `paint.draw` takes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Brushes {}

impl Op for Brushes {
    fn meta() -> OpMeta {
        query(
            "paint.brushes",
            "List the brush presets with what each is for and its settings, the blend modes and the mark kinds.",
        )
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let brushes = presets()
            .into_iter()
            .map(|p| Ok(json!({ "name": p.name, "use_for": p.use_for, "settings": json(&p.brush)? })))
            .collect::<Result<Vec<_>, OpError>>()?;
        let blends: Vec<_> = BlendMode::ALL.iter().map(json).collect::<Result<_, _>>()?;
        Ok(Plan::answer(json!({
            "brushes": brushes,
            "blend_modes": blends,
            "marks": {
                "stroke": "a brush stroke through points [x, y] or [x, y, pressure]; brush, colour, size, opacity, settings overrides, smooth, closed, erase, alpha_lock",
                "shape": "a filled region (geometry: path with SVG data, polygon, spline, ellipse, rect) with a colour or gradient fill, feather and outline",
                "bucket": "a flood fill from a point, bounded by the line art on the sample layers, with tolerance, gap closing and grow",
                "hatch": "parallel tapered lines across a region: angle, spacing, width, taper, inset, jitter",
            },
            "coordinates": "canvas pixels from the top left; angles in degrees, clockwise on screen",
        })))
    }
}
register_op!(Brushes);
