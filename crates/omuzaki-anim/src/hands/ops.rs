//! The query op `hand.list_shapes`: what shapes exist, so an agent or a cue editor can pick one by name.
//!
//! Applying a shape is not an op: it happens every frame inside the animator or a cue, and changes the pose, not the
//! document.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::library::HandLibrary;
use super::shape::{Finger, HandShape};

/// List the hand shapes (open, fist, point, peace, heart, grip, thumbs-up, rest) with their finger angles.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListShapes {}

/// Angles are shown to a hundredth of a degree: the library is written in whole degrees, and the radians they are
/// stored as would otherwise print as `79.99999`.
fn degrees(radians: f32) -> f64 {
    (f64::from(radians).to_degrees() * 100.0).round() / 100.0
}

fn describe(shape: &HandShape) -> Value {
    let curls: serde_json::Map<String, Value> = Finger::ALL
        .into_iter()
        .map(|f| {
            let c = shape.curl(f);
            let angles = json!({ "spread": degrees(c.spread), "mcp": degrees(c.mcp), "pip": degrees(c.pip), "dip": degrees(c.dip) });
            (f.name().to_owned(), angles)
        })
        .collect();
    json!({ "name": shape.id, "summary": shape.summary, "curls_deg": curls })
}

impl Op for ListShapes {
    fn meta() -> OpMeta {
        OpMeta {
            name: "hand.list_shapes",
            summary: "List the hand shapes (open, fist, point, ...) with their finger angles in degrees.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let library = HandLibrary::builtin().map_err(|e| OpError::internal(e.to_string()))?;
        Ok(Plan::answer(json!({ "shapes": library.iter().map(describe).collect::<Vec<_>>() })))
    }
}

omuzaki_ops_api::register_op!(ListShapes);
