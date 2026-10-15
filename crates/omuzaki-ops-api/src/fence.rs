//! Fences: the document paths an agent session claims (design `agent-sessions.md` §3.1, §5.3).
//!
//! Every comparison is segment-wise ([`DocPath::starts_with`]), so `a/bc` is not under `a/b`. An empty fence claims
//! nothing and does not bind the session; the helpers below keep that one rule in one place.

use omuzaki_doc_api::DocPath;

use crate::OpError;

/// Puts a fence in canonical form: sorted, without duplicates and without any path that lies under another one in the
/// set (the shorter path already claims it).
///
/// # Errors
/// `Invalid` (field `fence`) if a path is the root: a fence of the whole project is an empty fence.
pub fn normalize_fence(mut paths: Vec<DocPath>) -> Result<Vec<DocPath>, OpError> {
    if paths.iter().any(|path| path.segments().is_empty()) {
        return Err(OpError::invalid("fence", "a fence cannot name the root path")
            .hint("a fence of the whole project is an empty fence"));
    }
    // Sorted segment-wise, a path is followed directly by everything under it, so comparing with the last path kept
    // is enough to drop the covered ones.
    paths.sort();
    let mut kept: Vec<DocPath> = Vec::with_capacity(paths.len());
    for path in paths {
        if !kept.last().is_some_and(|last| path.starts_with(last)) {
            kept.push(path);
        }
    }
    Ok(kept)
}

/// Whether `fence` allows an edit at `path`: the path is at or under one of its entries. An empty fence covers
/// everything.
pub fn fence_covers(fence: &[DocPath], path: &DocPath) -> bool {
    fence.is_empty() || fence.iter().any(|entry| path.starts_with(entry))
}

/// The first pair of entries, one from each fence, where one is at or under the other. An empty fence claims nothing,
/// so it overlaps nothing.
pub fn fence_overlap<'a>(a: &'a [DocPath], b: &'a [DocPath]) -> Option<(&'a DocPath, &'a DocPath)> {
    a.iter().find_map(|x| b.iter().find(|y| x.starts_with(y) || y.starts_with(x)).map(|y| (x, y)))
}

/// Whether everything `inner` claims lies within `outer`. An empty `inner` claims the whole project, so only an empty
/// `outer` holds it.
pub fn fence_within(inner: &[DocPath], outer: &[DocPath]) -> bool {
    if inner.is_empty() {
        return outer.is_empty();
    }
    inner.iter().all(|path| fence_covers(outer, path))
}
