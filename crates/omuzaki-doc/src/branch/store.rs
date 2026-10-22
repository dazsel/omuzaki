//! [`DiskBranchStore`]: the [`BranchStore`] service over the refs and the snapshot store, and what branch refs keep
//! alive in it (design 4.6).

use std::collections::{BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};

use omuzaki_doc_api::Value;
use omuzaki_history_api::SnapshotId;
use omuzaki_history_api::branch::{BranchError, BranchId, BranchInfo, BranchName, LaneState, Point, PointId};
use omuzaki_history_api::state::BranchStore;
use omuzaki_store_api::ContentHash;

use super::{Refs, read_point};
use crate::snapshot::Snapshotter;

/// How many point trees a store keeps loaded (design: an LRU of 8).
pub const TREE_CACHE: usize = 8;

/// What the host says each lane is doing, for [`BranchInfo::lane`].
type LaneOf = Arc<dyn Fn(&BranchId) -> LaneState + Send + Sync>;

/// The branches as ops read them: refs and points, never another lane's live document. One per lane, built for its
/// branch ([`this`](BranchStore::this)); every store of a project shares one [`Refs`].
pub struct DiskBranchStore {
    this: BranchId,
    refs: Arc<Refs>,
    snapshots: Arc<Snapshotter>,
    lanes: Option<LaneOf>,
    cache: Mutex<VecDeque<(PointId, Arc<Value>)>>,
}

impl fmt::Debug for DiskBranchStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DiskBranchStore").field("this", &self.this).finish_non_exhaustive()
    }
}

/// Everything branch refs keep alive in the snapshot store (design 4.6): the points reachable from every ref, live or
/// in the bin (its head and fork point, then their parents, recursively), and the snapshots and history blobs those
/// points name. The store's sweep counts them as roots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Reachable {
    /// The points.
    pub points: BTreeSet<PointId>,
    /// The snapshots they name.
    pub snapshots: BTreeSet<SnapshotId>,
    /// The history exports they name.
    pub history: BTreeSet<ContentHash>,
}

impl Reachable {
    /// Every store object named, for [`Snapshotter::sweep_keeping`].
    pub fn roots(&self) -> BTreeSet<ContentHash> {
        self.points
            .iter()
            .map(|p| p.0)
            .chain(self.snapshots.iter().map(|s| s.0))
            .chain(self.history.iter().copied())
            .collect()
    }
}

impl DiskBranchStore {
    /// The store of branch `this`'s lane over `refs` and the project's `snapshots`.
    pub fn new(this: BranchId, refs: Arc<Refs>, snapshots: Arc<Snapshotter>) -> Self {
        Self { this, refs, snapshots, lanes: None, cache: Mutex::new(VecDeque::new()) }
    }

    /// This store, filling in [`BranchInfo::lane`] from `lanes` (the host's lane manager); without, every branch reads
    /// as closed.
    #[must_use]
    pub fn with_lanes(mut self, lanes: impl Fn(&BranchId) -> LaneState + Send + Sync + 'static) -> Self {
        self.lanes = Some(Arc::new(lanes));
        self
    }

    /// The refs it reads.
    pub fn refs(&self) -> &Arc<Refs> {
        &self.refs
    }

    fn filled(&self, mut info: BranchInfo) -> BranchInfo {
        if let Some(lanes) = &self.lanes {
            info.lane = lanes(&info.id);
        }
        info
    }

    /// What the refs keep alive. A point the store no longer holds ends its chain (it can't name parents).
    ///
    /// # Errors
    /// [`BranchError::Io`] when the refs can't be listed.
    pub fn reachable(&self) -> Result<Reachable, BranchError> {
        let mut reachable = Reachable::default();
        let mut todo: Vec<PointId> = Vec::new();
        for info in self.refs.list()? {
            todo.push(info.head);
            todo.extend(info.from);
        }
        let store = self.snapshots.store().as_ref();
        while let Some(id) = todo.pop() {
            if !reachable.points.insert(id) {
                continue;
            }
            match read_point(store, &id) {
                Ok(point) => {
                    reachable.snapshots.insert(point.snapshot);
                    reachable.history.extend(point.history);
                    todo.extend(point.parents.iter().filter(|p| !reachable.points.contains(p)));
                }
                Err(error) => tracing::warn!(point = %id, %error, "a point a branch reaches doesn't read"),
            }
        }
        Ok(reachable)
    }
}

impl BranchStore for DiskBranchStore {
    fn this(&self) -> Result<BranchInfo, BranchError> {
        self.refs.read(&self.this)?.map(|info| self.filled(info)).ok_or_else(|| {
            BranchError::NotFound(format!("{} (the project has not branched yet: no point records it)", self.this))
        })
    }

    fn list(&self) -> Result<Vec<BranchInfo>, BranchError> {
        Ok(self.refs.list()?.into_iter().map(|info| self.filled(info)).collect())
    }

    fn branch(&self, name: &BranchName) -> Result<BranchInfo, BranchError> {
        self.refs.by_name(name)?.map(|info| self.filled(info)).ok_or_else(|| BranchError::NotFound(name.to_string()))
    }

    fn point(&self, id: &PointId) -> Result<Point, BranchError> {
        read_point(self.snapshots.store().as_ref(), id)
    }

    fn tree(&self, id: &PointId) -> Result<Arc<Value>, BranchError> {
        {
            let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(at) = cache.iter().position(|(point, _)| point == id) {
                let hit = cache.remove(at).ok_or_else(|| BranchError::Io("the tree cache moved".to_owned()))?;
                let tree = Arc::clone(&hit.1);
                cache.push_front(hit);
                return Ok(tree);
            }
        }
        let point = self.point(id)?;
        let project =
            self.snapshots.load(&point.snapshot).map_err(|e| BranchError::Io(format!("point {id}'s snapshot: {e}")))?;
        let tree = Arc::new(project.root().clone());
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.push_front((*id, Arc::clone(&tree)));
        cache.truncate(TREE_CACHE);
        Ok(tree)
    }
}
