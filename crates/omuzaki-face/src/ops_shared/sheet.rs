//! Where the preview ops put their contact sheets: the app's asset store, as a PNG the MCP server serves under
//! `omuzaki://render/<hash>.png`. Without a store (a command line with no project) the answer says so instead.

use image::RgbaImage;
use omuzaki_ops_api::{ErrorCode, JobCtx, OpError};
use omuzaki_store_api::{Commercial, Provenance, Source, Store};
use serde_json::{Value, json};

use crate::preview::sheet::png;

/// The width of one cell of a contact sheet, pixels: big enough to see a lid or a lash, small enough for a sheet of
/// every channel to stay a few megapixels.
pub(crate) const SHEET_CELL_PX: u32 = 160;

/// Stores `sheet` and answers `{hash, uri, width, height}`, or `{note}` when the app has no asset store.
///
/// # Errors
/// `Internal` when the store refuses the write or the PNG can't be encoded.
pub(crate) fn store_sheet(job: &JobCtx<'_>, sheet: &RgbaImage) -> Result<Value, OpError> {
    let store = match job.service::<dyn Store>() {
        Ok(store) => store,
        Err(e) if e.code == ErrorCode::Unavailable => {
            return Ok(json!({ "note": "no asset store is attached, so the sheet was not kept" }));
        }
        Err(e) => return Err(e),
    };
    let bytes = png(sheet).map_err(|e| OpError::internal(format!("encoding the sheet failed: {e}")))?;
    let hash =
        store.put(&bytes, provenance()).map_err(|e| OpError::internal(format!("storing the sheet failed: {e}")))?;
    Ok(json!({
        "hash": hash.to_string(),
        "uri": format!("omuzaki://render/{hash}.png"),
        "width": sheet.width(),
        "height": sheet.height(),
    }))
}

/// A sheet is made in Omuzaki from the character: the user's, as every render is.
fn provenance() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: None,
        licence: "LicenseRef-Omuzaki-User".to_owned(),
        attribution: None,
        commercial: Commercial::Unknown,
        consent: None,
        private_test: false,
    }
}
