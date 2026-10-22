//! `face.expression.range_test` (FE-04): paints every expression of a character at emote and stream sizes, flags the
//! pairs that read the same and the expressions that read as the resting face, counts how many read as distinct, and
//! stores one contact sheet per size.
//!
//! It is a job, like the channel sweep: `plan` checks the arguments and that the face reads, `work` paints off the
//! document lock and polls for a cancel between expressions. The measure is [`crate::preview::range`]'s.

use image::imageops::{FilterType, resize};
use omuzaki_char_api::CharId;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ops_shared::sheet::store_sheet;
use crate::ops_shared::subject::subject;
use crate::preview::range::{DEFAULT_SIMILAR, DEFAULT_SIZES, PAINT_WIDTH_PX, RangeOptions, compare, paint_all};
use crate::preview::sheet::{Cell, compose};

/// The fewest expressions that must read as distinct at the smallest size (spec §32: an emote must still tell six moods
/// apart).
pub const MIN_DISTINCT: usize = 6;
/// The smallest and largest size, pixels.
const MIN_SIZE: u32 = 16;
/// See [`MIN_SIZE`].
const MAX_SIZE: u32 = PAINT_WIDTH_PX;
/// The most sizes one call compares.
const MAX_SIZES: usize = 8;
/// A sheet cell is shown at least this wide: a small size is blown up with whole pixels, so it shows what the viewer
/// gets and stays visible.
const SHEET_MIN_CELL_PX: u32 = 160;
/// Contact sheet columns.
const SHEET_COLUMNS: u32 = 6;

/// Paints every expression of a character at emote and stream sizes and flags the pairs that read the same.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"character": "kit", "sizes": [28, 56]}], "x-omuzaki-fixture" = "character"))]
pub struct RangeTest {
    /// The character whose expressions to test.
    pub character: CharId,
    /// Face widths to compare at, pixels (16 to 1024): an emote is 28, a small stream face 56, a large one 112.
    #[serde(default = "default_sizes")]
    pub sizes: Vec<u32>,
    /// Two expressions read the same when the lower of their eyes' and mouths' structural similarity (SSIM, 1 = the
    /// same picture) is above this.
    #[serde(default = "default_similar")]
    pub similar_threshold: f64,
    /// Store one contact sheet per size (needs the app's asset store).
    #[serde(default = "yes")]
    pub sheet: bool,
}

fn default_sizes() -> Vec<u32> {
    DEFAULT_SIZES.to_vec()
}

fn default_similar() -> f64 {
    DEFAULT_SIMILAR
}

fn yes() -> bool {
    true
}

impl RangeTest {
    fn options(&self) -> Result<RangeOptions, OpError> {
        if self.sizes.is_empty() || self.sizes.len() > MAX_SIZES {
            return Err(OpError::invalid("sizes", format!("give 1 to {MAX_SIZES} sizes, not {}", self.sizes.len())));
        }
        for (i, size) in self.sizes.iter().enumerate() {
            if !(MIN_SIZE..=MAX_SIZE).contains(size) {
                return Err(OpError::invalid(
                    format!("sizes/{i}"),
                    format!("{size} is out of range: use {MIN_SIZE} to {MAX_SIZE}"),
                ));
            }
        }
        if !(self.similar_threshold.is_finite() && (0.0..=1.0).contains(&self.similar_threshold)) {
            return Err(OpError::invalid(
                "similar_threshold",
                format!("similar_threshold = {} must be from 0 to 1", self.similar_threshold),
            )
            .hint(format!("{DEFAULT_SIMILAR} is the default; higher flags fewer pairs")));
        }
        let mut sizes = self.sizes.clone();
        sizes.sort_unstable();
        sizes.dedup();
        Ok(RangeOptions { sizes, similar: self.similar_threshold })
    }
}

impl Op for RangeTest {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.expression.range_test",
            summary: "Paint every expression of a character at emote and stream sizes, flag pairs that read the same \
                      and count how many read as distinct, with contact sheets",
            kind: OpKind::Job,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let options = self.options()?;
        let face = subject(ctx.doc, &self.character)?;
        if face.profile.expressions.0.is_empty() {
            return Err(OpError::invalid("character", format!("'{}' has no expressions to test", self.character))
                .hint("`face.expression.set` adds one"));
        }
        Ok(Plan::answer(json!({
            "character": self.character.to_string(),
            "expressions": face.profile.expressions.0.len(),
            "sizes": options.sizes,
            "similar_threshold": options.similar,
            "dry_run": ctx.dry_run,
        })))
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        job.checkpoint("reading the face", 0.0)?;
        let options = self.options()?;
        let face = job.read(|doc| subject(doc, &self.character))?;
        job.checkpoint("painting", 0.05)?;
        let painted = paint_all(&face, &options, &|| job.is_cancelled())
            .map_err(|e| OpError::internal(format!("painting the face failed: {e}")))?
            .ok_or_else(|| OpError::cancelled(job.id()))?;
        job.checkpoint("comparing", 0.7)?;
        let report = compare(&painted, &options, &face);
        let smallest = report.sizes.first().map_or(0, |s| s.distinct);
        let mut answer = json!({
            "character": self.character.to_string(),
            "expressions": painted.len() - 1,
            "similar_threshold": options.similar,
            "sizes": report.sizes,
            "similar": report.similar,
            "neutral": report.neutral,
            "enough_distinct": smallest >= MIN_DISTINCT,
            "min_distinct": MIN_DISTINCT,
        });
        if self.sheet {
            job.checkpoint("making the contact sheets", 0.8)?;
            let mut sheets = Vec::with_capacity(options.sizes.len());
            for (s, &size) in options.sizes.iter().enumerate() {
                let flagged = |name: &str| report.similar.iter().any(|p| p.size == size && (p.a == name || p.b == name));
                let cells: Vec<Cell> = painted
                    .iter()
                    .filter_map(|p| {
                        let crop = p.crops.get(s)?;
                        let factor = SHEET_MIN_CELL_PX.div_ceil(crop.width().max(1)).max(1);
                        let picture = if factor > 1 {
                            resize(crop, crop.width() * factor, crop.height() * factor, FilterType::Nearest)
                        } else {
                            crop.clone()
                        };
                        Some(Cell { picture, label: p.name.clone(), flagged: flagged(&p.name) })
                    })
                    .collect();
                let mut stored = store_sheet(job, &compose(&cells, SHEET_COLUMNS))?;
                stored["size"] = json!(size);
                sheets.push(stored);
            }
            answer["sheets"] = Value::Array(sheets);
        }
        Ok(answer)
    }
}

register_op!(RangeTest);
