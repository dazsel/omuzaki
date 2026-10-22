//! `render.contact_sheet`: stored renders laid out in a grid as one PNG (spec v3 §11), so a person or an agent sees a
//! set of previews (variants, expressions, before and after) at a glance.

use omuzaki_core::glam::UVec2;
use omuzaki_ops_api::{CostHint, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::picture::{canvas, hash_of, load, paste, store};

/// The most pictures one sheet takes.
pub const MAX_IMAGES: usize = 64;
/// The widest gap between cells, in pixels.
pub const MAX_GUTTER: u32 = 256;

fn default_gutter() -> u32 {
    8
}

/// Lays stored renders out in a grid and stores the sheet as a PNG.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"images": ["omuzaki://render/e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855.png", "omuzaki://render/e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855.png"]},
        {"images": ["e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"], "columns": 3, "gutter": 0}
    ]
))]
pub struct ContactSheet {
    /// The renders, by their `omuzaki://render/<hash>.png` URIs or hashes, in reading order: 1 to 64.
    pub images: Vec<String>,
    /// Pictures per row; by default the nearest to a square grid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>,
    /// The transparent gap between cells and around the sheet, in pixels, 0 to 256 (8 by default).
    #[serde(default = "default_gutter")]
    pub gutter: u32,
}

/// The stored sheet and its grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContactSheetResult {
    /// `omuzaki://render/<hash>.png` of the sheet.
    pub uri: String,
    /// Its content hash.
    pub hash: String,
    /// Its width and height in pixels.
    pub size: [u32; 2],
    /// Pictures per row.
    pub columns: u32,
    /// Rows.
    pub rows: u32,
    /// Each cell's width and height: the largest picture's. Smaller pictures are centred in theirs.
    pub cell: [u32; 2],
}

impl ContactSheet {
    fn check(&self) -> Result<(u32, u32), OpError> {
        if self.images.is_empty() || self.images.len() > MAX_IMAGES {
            return Err(OpError::invalid("images", format!("{} pictures; a sheet takes 1 to {MAX_IMAGES}", self.images.len())));
        }
        for (i, image) in self.images.iter().enumerate() {
            hash_of(&format!("images/{i}"), image)?;
        }
        if self.gutter > MAX_GUTTER {
            return Err(OpError::invalid("gutter", format!("a gutter of {} pixels is over {MAX_GUTTER}", self.gutter)));
        }
        let n = u32::try_from(self.images.len()).unwrap_or(u32::MAX);
        let columns = match self.columns {
            Some(0) => return Err(OpError::invalid("columns", "a sheet has at least one column")),
            Some(c) => c.min(n),
            None => (1..=n).find(|c| c * c >= n).unwrap_or(n),
        };
        Ok((columns, n.div_ceil(columns)))
    }
}

impl Op for ContactSheet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.contact_sheet",
            summary: "Lay stored renders (their omuzaki://render/ URIs) out in a grid and store the sheet as a PNG; \
                      returns its URI.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // It changes nothing in the document (the PNG goes to the content store), so its empty patch is undone
            // exactly and nothing enters the history.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.check()?;
        Ok(Plan { cost: Some(CostHint { seconds: Some(0.3), gpu_mib: Some(0), usd: None }), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let (columns, rows) = self.check()?;
        let mut pictures = Vec::with_capacity(self.images.len());
        for (i, image) in self.images.iter().enumerate() {
            #[allow(clippy::cast_precision_loss, reason = "at most 64 pictures")]
            job.checkpoint("reading", 0.8 * i as f32 / self.images.len() as f32)?;
            pictures.push(load(job, &format!("images/{i}"), image)?);
        }
        let cell = pictures.iter().fold(UVec2::ZERO, |most, p| most.max(p.size));
        let g = self.gutter;
        let size = UVec2::new(columns * (cell.x + g) + g, rows * (cell.y + g) + g);
        let mut sheet = canvas("images", size)?;
        for (i, picture) in (0_u32..).zip(&pictures) {
            let corner = UVec2::new(g + (i % columns) * (cell.x + g), g + (i / columns) * (cell.y + g));
            paste(&mut sheet, picture, corner + (cell - picture.size) / 2);
        }
        let stored = store(job, &sheet)?;
        job.checkpoint("done", 1.0)?;
        let result = ContactSheetResult {
            uri: stored.uri,
            hash: stored.hash.to_string(),
            size: [size.x, size.y],
            columns,
            rows,
            cell: [cell.x, cell.y],
        };
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the sheet: {e}")))
    }
}

register_op!(ContactSheet);

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(n: usize, columns: Option<u32>) -> ContactSheet {
        let hash = omuzaki_store_api::ContentHash::of(b"x").to_string();
        ContactSheet { images: vec![hash; n], columns, gutter: 8 }
    }

    #[test]
    fn the_grid_is_as_square_as_the_count_allows_unless_columns_are_given() {
        assert_eq!(sheet(1, None).check().unwrap(), (1, 1));
        assert_eq!(sheet(4, None).check().unwrap(), (2, 2));
        assert_eq!(sheet(5, None).check().unwrap(), (3, 2));
        assert_eq!(sheet(5, Some(5)).check().unwrap(), (5, 1));
        assert_eq!(sheet(3, Some(10)).check().unwrap(), (3, 1), "no empty columns");
    }

    #[test]
    fn an_empty_or_oversized_sheet_is_refused() {
        assert!(sheet(0, None).check().is_err());
        assert!(sheet(MAX_IMAGES + 1, None).check().is_err());
        assert!(sheet(2, Some(0)).check().is_err());
        assert!(ContactSheet { gutter: MAX_GUTTER + 1, ..sheet(1, None) }.check().is_err());
        assert!(ContactSheet { images: vec!["not a render".into()], ..sheet(1, None) }.check().is_err());
    }
}
