//! The merge base (`docs/design/branches.md` §7.3, ADR 0025): which state both sides of a merge are known to include,
//! found from version vectors rather than from the point graph, so a merge that was undone, restored past or rolled
//! back is forgotten with the record that the document itself carries.
//!
//! **Why never too new.** A base can hide theirs' changes only if it claims ours received something it never did.
//! Every candidate is a version both vectors name: something both sides received. The worst an old candidate does is
//! add a conflict; it never loses a change silently. The property test in `tests/merge_base.rs` checks that against a
//! content oracle.

use crate::branch::{BranchId, Contains, Point, PointId, Version};

/// One candidate: a version of `branch` both sides include, with its point when the store still has it.
struct Candidate {
    branch: BranchId,
    version: Version,
    point: Point,
}

impl Candidate {
    /// Whether this state includes `other` (`c ⊒ d`): `other` is on this branch at a lower or equal number, or this
    /// point's `contains` holds it.
    fn includes(&self, other: &Candidate) -> bool {
        (self.branch == other.branch && other.version.seq <= self.version.seq)
            || self.point.contains.includes(&other.branch, &other.version)
    }
}

/// The base for merging `theirs` (a point of another branch) into the live document of branch `ours`, whose version
/// vector is `ours_contains`: the document's `merges` record joined with the branch's `inherits` (design 7.3 step 1).
/// `points` reads a point from the store; a candidate it can't find (collected, or the project was copied without
/// `.omuzaki/`) is dropped.
///
/// The steps of design 7.3: ours' own branch counts as "everything", theirs' vector is `theirs.contains` with theirs'
/// branch at `theirs`; a candidate is, for each branch both vectors name, its point at the lower of the two numbers;
/// the candidate that includes all the others wins; if none does (a criss-cross: both merged each other at once), the
/// newest by time, then the smallest id. `None` when there is no candidate: the merge has no base, and every
/// difference is a choice.
pub fn merge_base(
    ours: &BranchId,
    ours_contains: &Contains,
    theirs: &Point,
    points: &dyn Fn(&PointId) -> Option<Point>,
) -> Option<PointId> {
    let theirs_id = theirs.id().ok()?;
    let theirs_vector = theirs.contains.clone().with(theirs.branch.clone(), Version::new(theirs.seq, theirs_id));
    let read = |id: &PointId| if *id == theirs_id { Some(theirs.clone()) } else { points(id) };
    let mut candidates: Vec<Candidate> = theirs_vector
        .iter()
        .filter_map(|(branch, theirs_version)| {
            let version = if branch == ours {
                *theirs_version
            } else {
                let ours_version = ours_contains.get(branch)?;
                if ours_version.seq <= theirs_version.seq { *ours_version } else { *theirs_version }
            };
            let point = read(&version.point)?;
            Some(Candidate { branch: branch.clone(), version, point })
        })
        .collect();
    let best = candidates.iter().position(|c| candidates.iter().all(|other| c.includes(other)));
    let winner = match best {
        Some(i) => candidates.swap_remove(i),
        None => candidates
            .into_iter()
            .max_by(|a, b| a.point.at.cmp(&b.point.at).then_with(|| b.version.point.cmp(&a.version.point)))?,
    };
    Some(winner.version.point)
}

/// The record a full merge of `theirs` writes into ours' `merges.toml` (design 7.3): what ours had merged, joined with
/// what theirs contains and with theirs itself, without ours' own branch.
pub fn merged_record(ours: &BranchId, ours_merges: &Contains, theirs: &Point) -> Contains {
    let Ok(theirs_id) = theirs.id() else { return ours_merges.clone() };
    let joined = ours_merges.join(&theirs.contains).with(theirs.branch.clone(), Version::new(theirs.seq, theirs_id));
    joined
        .iter()
        .filter(|(branch, _)| *branch != ours)
        .fold(Contains::new(), |record, (branch, version)| record.with(branch.clone(), *version))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use omuzaki_ops_api::Actor;
    use omuzaki_store_api::ContentHash;

    use super::*;
    use crate::branch::PointKind;
    use crate::{Lsn, SnapshotId, WallTime};

    fn hex(n: u8) -> BranchId {
        BranchId::new(format!("{n:016x}")).unwrap_or_default()
    }

    /// Points by id, made in order with increasing times.
    #[derive(Default)]
    struct Store(BTreeMap<PointId, Point>);

    impl Store {
        fn point(&mut self, branch: &BranchId, seq: u64, contains: Contains) -> (Point, PointId) {
            let at = u64::try_from(self.0.len()).unwrap_or(0) + 1;
            self.point_at(branch, seq, contains, at)
        }

        fn point_at(&mut self, branch: &BranchId, seq: u64, contains: Contains, at: u64) -> (Point, PointId) {
            let at = WallTime(at);
            let snapshot = SnapshotId(ContentHash::of(format!("{branch}{seq}").as_bytes()));
            let point = Point::new(branch.clone(), seq, snapshot, Lsn(seq), PointKind::Checkpoint, Actor::System, at)
                .with_contains(contains);
            let id = point.id().unwrap_or_else(|e| panic!("{e}"));
            self.0.insert(id, point.clone());
            (point, id)
        }

        fn get(&self) -> impl Fn(&PointId) -> Option<Point> + '_ {
            |id| self.0.get(id).cloned()
        }
    }

    fn v(point: &Point, id: PointId) -> Version {
        Version::new(point.seq, id)
    }

    #[test]
    fn fast_forward_the_fork_point_is_the_base() {
        let (main, side) = (BranchId::main(), hex(1));
        let mut store = Store::default();
        let (fork, fork_id) = store.point(&main, 1, Contains::new());
        let inherits = Contains::new().with(main.clone(), v(&fork, fork_id));
        let (theirs, _) = store.point(&side, 1, inherits.clone());
        // main has not moved since the fork, and merges nothing.
        assert_eq!(merge_base(&main, &Contains::new(), &theirs, &store.get()), Some(fork_id));
    }

    #[test]
    fn already_merged_the_base_is_theirs_itself() {
        let (main, side) = (BranchId::main(), hex(1));
        let mut store = Store::default();
        let (fork, fork_id) = store.point(&main, 1, Contains::new());
        let (theirs, theirs_id) = store.point(&side, 1, Contains::new().with(main.clone(), v(&fork, fork_id)));
        let record = merged_record(&main, &Contains::new(), &theirs);
        assert_eq!(record.get(&side), Some(&v(&theirs, theirs_id)));
        assert_eq!(record.get(&main), None, "ours never records itself");
        assert_eq!(merge_base(&main, &record, &theirs, &store.get()), Some(theirs_id));
    }

    #[test]
    fn unrelated_branches_have_no_base() {
        let (a, b) = (hex(1), hex(2));
        let mut store = Store::default();
        let (theirs, _) = store.point(&b, 3, Contains::new());
        assert_eq!(merge_base(&a, &Contains::new(), &theirs, &store.get()), None);
    }

    #[test]
    fn a_missing_point_is_dropped_and_with_no_other_candidate_there_is_no_base() {
        let (main, side) = (BranchId::main(), hex(1));
        let mut store = Store::default();
        let (later, later_id) = store.point(&main, 2, Contains::new());
        let (theirs, _) = store.point(&main, 3, Contains::new());
        // side received main 2; main 3 is merged into it: the base is main 2, while the store has it.
        let ours = Contains::new().with(main.clone(), v(&later, later_id));
        assert_eq!(merge_base(&side, &ours, &theirs, &store.get()), Some(later_id));
        let collected = |id: &PointId| if *id == later_id { None } else { store.0.get(id).cloned() };
        assert_eq!(merge_base(&side, &ours, &theirs, &collected), None, "every difference is then a choice");
    }

    #[test]
    fn inclusion_beats_time_and_a_criss_cross_takes_the_newest() {
        let (main, a, b) = (BranchId::main(), hex(1), hex(2));
        let mut store = Store::default();
        // main 1 is stamped later than a 1 (clocks differ between machines), but a 1 includes it.
        let (m1, m1_id) = store.point_at(&main, 1, Contains::new(), 50);
        let (a1, a1_id) = store.point_at(&a, 1, Contains::new().with(main.clone(), v(&m1, m1_id)), 10);
        // ours (b) received main 1 and a 1; theirs (main 2) received a 1 too.
        let (m2, _) = store.point_at(&main, 2, Contains::new().with(a.clone(), v(&a1, a1_id)), 60);
        let ours = Contains::new().with(main.clone(), v(&m1, m1_id)).with(a.clone(), v(&a1, a1_id));
        // Candidates: main 1 and a 1. a 1 includes main 1, so it wins though main 1 is newer by time.
        assert_eq!(merge_base(&b, &ours, &m2, &store.get()), Some(a1_id));

        // A criss-cross: merging y's second point (which includes x 1) into x, which merged y 1. The candidates x 1
        // and y 1 don't include each other, so the newer by time (y 1, made after x 1) is the base.
        let (x, y) = (hex(3), hex(4));
        let (x0, x0_id) = store.point(&x, 1, Contains::new());
        let (y0, y0_id) = store.point(&y, 1, Contains::new());
        let (y1, _) = store.point(&y, 2, Contains::new().with(x.clone(), v(&x0, x0_id)));
        let ours = Contains::new().with(y.clone(), v(&y0, y0_id));
        assert_eq!(merge_base(&x, &ours, &y1, &store.get()), Some(y0_id));
    }
}
