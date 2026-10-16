//! Fence enforcement (design `agent-sessions.md` §5.3, "Fences"; AG-04): an agent's edit must stay inside its own
//! fence, when it has one, and out of every other live session's.
//!
//! The comparisons are the contract's helpers ([`fence_covers`]), segment-wise. A claim is derived from the live
//! sessions, so ending a session frees its paths under the same lock hold, and nothing expires.

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{Actor, fence_covers};

/// Why a patch was refused: the first path that broke a fence, and whose fence it lies in (`None`: outside the
/// session's own).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FenceRefusal {
    pub(crate) path: DocPath,
    pub(crate) holder: Option<Actor>,
}

/// `Ok` when every path `patch` changes is allowed. `own` is the session's fence; `others` are the fences of the live
/// sessions that are neither this one nor its ancestors nor its descendants (a team does not clash with itself).
///
/// A changed path is refused when it lies at, under or above an entry of another session's fence (writing a parent
/// replaces what the fence claims), or when the session is fenced and its own fence does not cover the path. Another
/// session's fence is checked first, so a refusal names a holder whenever there is one.
pub(crate) fn check_fence(own: &[DocPath], others: &[(&Actor, &[DocPath])], patch: &Patch) -> Result<(), FenceRefusal> {
    for change in &patch.changes {
        let changed = &change.path;
        let held = others.iter().find(|(_, fence)| fence.iter().any(|entry| overlaps(changed, entry)));
        if let Some((holder, _)) = held {
            return Err(FenceRefusal { path: changed.clone(), holder: Some((*holder).clone()) });
        }
        if !fence_covers(own, changed) {
            return Err(FenceRefusal { path: changed.clone(), holder: None });
        }
    }
    Ok(())
}

/// Whether one of the two paths lies at or under the other.
fn overlaps(a: &DocPath, b: &DocPath) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn path(s: &str) -> DocPath {
        DocPath::parse(s).unwrap()
    }

    fn patch(paths: &[&str]) -> Patch {
        paths.iter().fold(Patch::new(), |patch, p| patch.with(path(p), None, Some(json!(1))))
    }

    #[test]
    fn edits_stay_inside_their_own_fence_and_out_of_others() {
        let b = Actor::Agent { id: "b#2".into(), model: None };
        let face = [path("characters/a/parts/face")];
        let hair = [path("characters/a/parts/hair")];
        let others = [(&b, &face[..])];
        assert_eq!(check_fence(&hair, &others, &patch(&["characters/a/parts/hair/bangs"])), Ok(()));
        let into_b = check_fence(&hair, &others, &patch(&["characters/a/parts/face/eyes"])).unwrap_err();
        assert_eq!(into_b.holder, Some(b.clone()));
        let outside = check_fence(&hair, &[], &patch(&["characters/a/parts/tail"])).unwrap_err();
        assert_eq!(outside, FenceRefusal { path: path("characters/a/parts/tail"), holder: None });
        let above = check_fence(&[], &others, &patch(&["characters/a/parts"])).unwrap_err();
        assert_eq!(above.holder, Some(b.clone()), "replacing a parent overwrites the claim");
        assert_eq!(check_fence(&[], &[], &patch(&["anything"])), Ok(()), "an unfenced agent with no one else fenced");
        assert_eq!(check_fence(&hair, &others, &patch(&["characters/a/parts/hairline"])).unwrap_err().holder, None);
    }
}
