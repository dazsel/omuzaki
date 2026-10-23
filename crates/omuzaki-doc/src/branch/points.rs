//! Points as objects in the snapshot store (design 4.2): canonical JSON, whose BLAKE3 hash is the point id.

use omuzaki_history_api::branch::{BranchError, Point, PointId};
use omuzaki_store_api::{Store, StoreError};

use crate::snapshot::internal;

/// Writes `point` into `store` as canonical JSON and reads it back through `get` (which re-hashes) before returning its
/// id, so a ref written after this never names a point the store doesn't hold whole.
///
/// # Errors
/// [`BranchError::Io`] when the store can't hold it or what it holds doesn't read back as the same point.
pub fn write_point(store: &dyn Store, point: &Point) -> Result<PointId, BranchError> {
    let json = point.canonical_json()?;
    let hash = store.put(json.as_bytes(), internal()).map_err(|e| BranchError::Io(format!("a point: {e}")))?;
    let id = PointId(hash);
    if point.id()? != id {
        return Err(BranchError::Io(format!("point {id} was stored under another hash than its content's")));
    }
    if read_point(store, &id)? != *point {
        return Err(BranchError::Io(format!("point {id} doesn't read back as written")));
    }
    Ok(id)
}

/// The point `id` from `store`, checked against its id.
///
/// # Errors
/// [`BranchError::PointNotFound`] when the store doesn't hold it; [`BranchError::Io`] when it is damaged or isn't a
/// point.
pub fn read_point(store: &dyn Store, id: &PointId) -> Result<Point, BranchError> {
    let bytes = match store.get(&id.0) {
        Ok(bytes) => bytes,
        Err(StoreError::Missing(_)) => return Err(BranchError::PointNotFound(*id)),
        Err(e) => return Err(BranchError::Io(format!("point {id}: {e}"))),
    };
    let point: Point = serde_json::from_slice(&bytes).map_err(|e| BranchError::Io(format!("point {id}: {e}")))?;
    if point.id()? != *id {
        return Err(BranchError::Io(format!("point {id} doesn't match its id")));
    }
    Ok(point)
}
