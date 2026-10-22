//! `rig.weights.mirror`: copy a part's brushes to the other side of the body.

use omuzaki_char_api::{CharId, PartId};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{brushes_of, store, straddling};
use crate::kit::skin::brush_mirrored;
use crate::ops::character::access;

/// Appends the mirror image of each of a part's weight brushes: the centre's x is negated and every joint is swapped for
/// its opposite (`leftUpperArm` for `rightUpperArm`, `hair_l_2` for `hair_r_2`), so a brush on the left shoulder gets a
/// twin on the right. A centre-line brush is its own mirror, and a brush whose mirror is already in the list adds
/// nothing, so running it again changes nothing.
///
/// The output counts what was `added` and how many brushes were `skipped` because their mirror was already there. It also
/// lists as `straddling` the indices of the brushes that cross the centre plane with a different mirror image.
///
/// The skin is symmetric, on the centre plane too. A brush and its exact mirror image are applied together when the
/// character builds (`apply_brushes`), both working from the weights the vertex had before either, so a plane vertex
/// favours neither side. Other brushes still apply in list order. The brushes that cross the plane are listed as
/// `straddling` here and in `rig.weights.report`, for information.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "body"}
    ],
    "x-omuzaki-fixture" = "character-brush"
))]
pub struct WeightsMirror {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part whose brushes are mirrored. Must exist.
    pub part: PartId,
}

impl Op for WeightsMirror {
    fn meta() -> OpMeta {
        OpMeta {
            name: "rig.weights.mirror",
            summary: "Append the mirror image of a part's weight brushes, with the left and right joints swapped.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        access::part(&def, &self.character, &self.part, "part")?;
        let mut brushes = brushes_of(&def, &self.part);
        let originals = brushes.len();
        let mut added = 0_usize;
        for index in 0..originals {
            let image = brush_mirrored(&brushes[index]);
            if !brushes.contains(&image) {
                brushes.push(image);
                added += 1;
            }
        }
        let changes = if added == 0 { Vec::new() } else { store(ctx, &self.character, &self.part, &brushes)? };
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "added": added,
            "skipped": originals - added,
            "brushes": brushes.len(),
            "straddling": straddling(&brushes),
        });
        Ok(access::edit_plan(changes, summary))
    }
}

register_op!(WeightsMirror);
