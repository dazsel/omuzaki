//! Branches on disk (`docs/design/branches.md` section 4, ADR 0025): where each branch keeps its journal, its ref and
//! the folder marker, points as objects in the snapshot store, and [`DiskBranchStore`], the [`BranchStore`] ops read.
//!
//! ```text
//! .omuzaki/
//!   journal/ log/                     the journal and archive of `main` (its branch root is `.omuzaki/` itself)
//!   snapshots/                        one store for every branch: snapshots, history blobs and points
//!   branches/
//!     folder.toml                     which branch the folder shows, and the switch marker
//!     main/ref.toml                   the ref of `main`, once the project has branched
//!     9c41d07a5e2b1f30/ref.toml       another branch's ref
//!     9c41d07a5e2b1f30/journal/ log/  its journal and archive, in the format of ADR 0021
//! ```
//!
//! A project that never branches has no `branches/` folder and behaves exactly as ADR 0021 describes.
//!
//! [`BranchStore`]: omuzaki_history_api::state::BranchStore

mod folder;
mod points;
mod refs;
mod store;

use std::path::{Path, PathBuf};

use omuzaki_history_api::branch::BranchId;

pub use folder::{FolderMarker, FolderState};
pub use points::{read_point, write_point};
pub use refs::{BIN_DAYS, Refs};
pub use store::{DiskBranchStore, Reachable, TREE_CACHE};

/// The folder inside `.omuzaki/` that holds the refs, the folder marker and the other branches' journals.
pub const BRANCHES_DIR: &str = "branches";

/// The project's private folder: `.omuzaki/`.
pub fn private_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(".omuzaki")
}

/// `.omuzaki/branches/` of the project in `project_dir`.
pub fn branches_dir(project_dir: &Path) -> PathBuf {
    private_dir(project_dir).join(BRANCHES_DIR)
}

/// The branch root of `branch` (design 4.4): `.omuzaki/` for `main`, `.omuzaki/branches/<id>/` otherwise. The
/// journal, its archive and recovery of a branch's lane all live under it.
pub fn branch_root(project_dir: &Path, branch: &BranchId) -> PathBuf {
    if branch.is_main() { private_dir(project_dir) } else { branches_dir(project_dir).join(branch.as_str()) }
}

/// A new random branch id: 16 lowercase hex characters (design 4.7).
///
/// # Errors
/// Never in practice: the id is made of hex digits.
pub fn new_branch_id() -> Result<BranchId, omuzaki_history_api::branch::BranchIdError> {
    let mut bytes = [0u8; 8];
    #[cfg(any(unix, windows))]
    let random = getrandom::fill(&mut bytes).is_ok();
    #[cfg(not(any(unix, windows)))]
    let random = false;
    if !random {
        // Without OS randomness (or on wasm) the id only has to differ from the others in this project, which the time
        // does.
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        bytes.copy_from_slice(&(nanos ^ u128::from(std::process::id())).to_le_bytes()[..8]);
    }
    BranchId::new(format!("{:016x}", u64::from_le_bytes(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_lives_in_the_private_folder_and_others_by_id() {
        let dir = Path::new("show");
        assert_eq!(branch_root(dir, &BranchId::main()), Path::new("show/.omuzaki"));
        let id = BranchId::new("9c41d07a5e2b1f30").unwrap_or_default();
        assert_eq!(branch_root(dir, &id), Path::new("show/.omuzaki/branches/9c41d07a5e2b1f30"));
    }

    #[test]
    fn new_ids_are_valid_and_differ() {
        let (a, b) = (new_branch_id(), new_branch_id());
        assert!(a.as_ref().is_ok_and(|id| !id.is_main()), "{a:?}");
        assert_ne!(a, b);
    }
}
