//! The merge base against a content oracle (`docs/design/branches.md` §12.2).
//!
//! Random sequences of fork, edit, checkpoint, full merge, pick, undo of a merge, restore and recovery run over two to
//! five branches. Every edit adds a unique token. The oracle keeps, for each state, the explicit set of points it
//! includes: its own branch's points (a branch counts as everything of itself), what it inherited at its fork, and the
//! closure of every merge whose record is still in its document. After each merge it checks:
//!
//! 1. **Never too new:** the base is a state both sides include (in the oracle's sets), or there is none.
//! 2. **Loadable:** a base is a point the store still has.
//! 3. **No silent loss:** a token theirs holds that the result lacks was in a state both sides include (so ours
//!    removed it after both had it).
//!
//! Seeds are fixed and printed on failure. The mutation checks run the same sequences through deliberately wrong
//! variants (the record read from the newest point instead of the live document; missing candidates kept) and require
//! that each fails on some seed; the third (newest by time preferred over inclusion) is a fixed scenario, because every
//! candidate it could choose is still a common state.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_history_api::branch::{BranchId, Contains, Point, PointId, PointKind, Version};
use omuzaki_history_api::merge_base::{merge_base, merged_record};
use omuzaki_history_api::{Lsn, SnapshotId, WallTime};
use omuzaki_ops_api::Actor;
use omuzaki_store_api::ContentHash;

/// A small deterministic generator (xorshift64*), so a seed names a run.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n.max(1)).unwrap_or(1)).unwrap_or(0)
    }
}

/// A document: its tokens and its `merges` record.
#[derive(Clone, Debug, Default)]
struct Doc {
    tokens: BTreeSet<u64>,
    record: Contains,
}

/// One branch's live state and what the oracle knows about it.
struct Branch {
    id: BranchId,
    live: Doc,
    inherits: Contains,
    /// The explicit points inherited at the fork: the fork point and everything it includes.
    inherited: BTreeSet<PointId>,
    /// Every point of this branch, oldest first.
    points: Vec<PointId>,
    /// The documents before each merge still undoable, newest last.
    undo: Vec<Doc>,
}

/// A stored point, with the explicit set of points its state includes.
struct Stored {
    point: Point,
    doc: Doc,
    includes: BTreeSet<PointId>,
}

/// Which variant of the algorithm a run uses.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum Variant {
    Real,
    /// Reads ours' vector from the newest point of ours instead of the live document.
    RecordFromHead,
    /// Keeps a candidate whose point the store lost.
    KeepsMissing,
}

struct World {
    branches: Vec<Branch>,
    store: BTreeMap<PointId, Stored>,
    /// Points garbage-collected from the store (their ids still appear in vectors).
    lost: BTreeSet<PointId>,
    next_token: u64,
    clock: u64,
    variant: Variant,
    /// Merges that ran, and those of them that had a base.
    merges: (u32, u32),
}

impl World {
    fn new(variant: Variant) -> Self {
        let main = Branch {
            id: BranchId::main(),
            live: Doc::default(),
            inherits: Contains::new(),
            inherited: BTreeSet::new(),
            points: Vec::new(),
            undo: Vec::new(),
        };
        let mut world =
            Self { branches: vec![main], store: BTreeMap::new(), lost: BTreeSet::new(), next_token: 0, clock: 0, variant, merges: (0, 0) };
        world.checkpoint(0);
        world
    }

    fn token(&mut self) -> u64 {
        self.next_token += 1;
        self.next_token
    }

    /// The points state `b`'s live document includes, explicitly.
    fn live_includes(&self, b: usize) -> BTreeSet<PointId> {
        let branch = &self.branches[b];
        let mut set: BTreeSet<PointId> = branch.points.iter().copied().collect();
        set.extend(branch.inherited.iter().copied());
        for (_, version) in branch.live.record.iter() {
            set.extend(self.closure(&version.point));
        }
        set
    }

    /// A point and everything it includes.
    fn closure(&self, id: &PointId) -> BTreeSet<PointId> {
        let mut set = BTreeSet::from([*id]);
        if let Some(stored) = self.store.get(id) {
            set.extend(stored.includes.iter().copied());
        }
        set
    }

    /// A point of the live state of `b`; returns the head when nothing changed.
    fn checkpoint(&mut self, b: usize) -> PointId {
        let includes = self.live_includes(b);
        let branch = &self.branches[b];
        if let Some(head) = branch.points.last() {
            let stored = &self.store[head];
            if stored.doc.tokens == branch.live.tokens && stored.point.contains == branch.live.record.join(&branch.inherits)
            {
                return *head;
            }
        }
        self.clock += 1;
        let seq = u64::try_from(branch.points.len()).unwrap_or(0) + 1;
        let snapshot = SnapshotId(ContentHash::of(format!("{}-{seq}", branch.id).as_bytes()));
        let point = Point::new(branch.id.clone(), seq, snapshot, Lsn(seq), PointKind::Checkpoint, Actor::System, WallTime(self.clock))
            .with_contains(branch.live.record.join(&branch.inherits));
        let id = point.id().unwrap_or_else(|e| panic!("{e}"));
        let doc = branch.live.clone();
        self.store.insert(id, Stored { point, doc, includes });
        self.branches[b].points.push(id);
        id
    }

    fn fork(&mut self, source: usize, rng: &mut Rng) {
        let fork = self.checkpoint(source);
        let stored = &self.store[&fork];
        let id = BranchId::new(format!("{:016x}", rng.next())).unwrap_or_default();
        let inherits = stored.point.contains.clone().with(stored.point.branch.clone(), Version::new(stored.point.seq, fork));
        let inherited = self.closure(&fork);
        let live = Doc { tokens: stored.doc.tokens.clone(), record: self.branches[source].live.record.clone() };
        self.branches.push(Branch { id, live, inherits, inherited, points: Vec::new(), undo: Vec::new() });
        let b = self.branches.len() - 1;
        self.checkpoint(b);
    }

    /// The vector of ours the algorithm is given.
    fn ours_vector(&self, o: usize) -> Contains {
        let branch = &self.branches[o];
        match (self.variant, branch.points.last()) {
            (Variant::RecordFromHead, Some(head)) => self.store[head].point.contains.clone(),
            _ => branch.live.record.join(&branch.inherits),
        }
    }

    fn base(&self, o: usize, theirs: &Point) -> Option<PointId> {
        let ours = &self.branches[o].id;
        let vector = self.ours_vector(o);
        let read = |id: &PointId| {
            if self.lost.contains(id) && self.variant != Variant::KeepsMissing {
                return None;
            }
            self.store.get(id).map(|s| s.point.clone())
        };
        merge_base(ours, &vector, theirs, &read)
    }

    /// Merges the head of `t` into `o`, fully or as a pick, and checks the three properties.
    fn merge(&mut self, o: usize, t: usize, full: bool, rng: &mut Rng) -> Result<(), String> {
        let theirs_id = self.checkpoint(t);
        let theirs = self.store[&theirs_id].point.clone();
        let theirs_doc = self.store[&theirs_id].doc.clone();
        let base = self.base(o, &theirs);
        self.merges.0 += 1;
        self.merges.1 += u32::from(base.is_some());
        // 1. never too new
        let common: BTreeSet<PointId> =
            self.live_includes(o).intersection(&self.closure(&theirs_id)).copied().collect();
        if let Some(base) = base {
            if !common.contains(&base) {
                return Err(format!("the base {base} is not a state both sides include"));
            }
            // 2. loadable
            if self.lost.contains(&base) {
                return Err(format!("the base {base} is a point the store lost"));
            }
        }
        let base_tokens = base.map(|b| self.store[&b].doc.tokens.clone()).unwrap_or_default();
        let ours = self.branches[o].live.clone();
        let mut result = Doc { tokens: BTreeSet::new(), record: ours.record.clone() };
        let all: BTreeSet<u64> = ours.tokens.union(&theirs_doc.tokens).copied().collect();
        for token in all {
            let (in_o, in_t, in_b) =
                (ours.tokens.contains(&token), theirs_doc.tokens.contains(&token), base_tokens.contains(&token));
            let keep = match (in_o, in_t) {
                (true, true) => true,
                (true, false) => !in_b || !full, // theirs removed it: a pick leaves ours as it is
                (false, true) => !in_b && (full || rng.below(2) == 0), // a pick takes some of theirs' additions
                (false, false) => false,
            };
            if keep {
                result.tokens.insert(token);
            }
        }
        if full {
            result.record = merged_record(&self.branches[o].id, &ours.record, &theirs);
            // 3. no silent loss
            for token in theirs_doc.tokens.difference(&result.tokens) {
                let shared = common.iter().any(|s| self.store.get(s).is_some_and(|p| p.doc.tokens.contains(token)));
                if !shared {
                    return Err(format!("token {token} of theirs was lost: no common state held it"));
                }
            }
        }
        let branch = &mut self.branches[o];
        branch.undo.push(std::mem::replace(&mut branch.live, result));
        Ok(())
    }

    fn step(&mut self, rng: &mut Rng) -> Result<(), String> {
        let n = self.branches.len();
        let b = rng.below(n);
        match rng.below(10) {
            0 if n < 5 => self.fork(b, rng),
            1..=3 => {
                let token = self.token();
                self.branches[b].live.tokens.insert(token);
                self.branches[b].undo.clear();
            }
            4 => drop(self.checkpoint(b)),
            5 | 6 if n > 1 => {
                let t = (b + 1 + rng.below(n - 1)) % n;
                let full = rng.below(3) != 0;
                self.merge(b, t, full, rng)?;
            }
            7 => {
                // Undo the newest merge, record and all.
                if let Some(before) = self.branches[b].undo.pop() {
                    self.branches[b].live = before;
                }
            }
            8 => {
                // Restore to one of the branch's own points: its tree and its record come back.
                let points = &self.branches[b].points;
                let at = points[rng.below(points.len())];
                self.branches[b].live = self.store[&at].doc.clone();
                self.branches[b].undo.clear();
            }
            9 => {
                // An old point gone from the store (a purged branch's, or a folder copied without its history), or a
                // recovery (nothing committed is lost: no change). A live branch's head is in its ref and never goes.
                let heads: BTreeSet<PointId> = self.branches.iter().filter_map(|b| b.points.last().copied()).collect();
                let ids: Vec<PointId> = self.store.keys().filter(|id| !heads.contains(id)).copied().collect();
                if rng.below(4) == 0 && !ids.is_empty() {
                    self.lost.insert(ids[rng.below(ids.len())]);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn run(seed: u64, variant: Variant) -> Result<(u32, u32), String> {
    let mut rng = Rng::new(seed);
    let mut world = World::new(variant);
    for step in 0..120 {
        world.step(&mut rng).map_err(|e| format!("seed {seed}, step {step}: {e}"))?;
    }
    Ok(world.merges)
}

const SEEDS: u64 = 400;

#[test]
fn the_base_is_never_too_new_and_nothing_of_theirs_is_lost_silently() {
    let (mut merges, mut with_base) = (0, 0);
    for seed in 0..SEEDS {
        let (m, b) = run(seed, Variant::Real).unwrap_or_else(|failure| panic!("{failure}"));
        (merges, with_base) = (merges + m, with_base + b);
    }
    // The sequences exercise the base, not only unrelated merges.
    assert!(merges > 5_000 && with_base * 10 > merges * 9, "{merges} merges, {with_base} with a base");
}

#[test]
fn reading_the_record_from_the_newest_point_is_caught() {
    let caught = (0..SEEDS).find(|seed| run(*seed, Variant::RecordFromHead).is_err());
    assert!(caught.is_some(), "no seed caught the record read from the newest point");
}

#[test]
fn keeping_a_candidate_whose_point_is_missing_is_caught() {
    let caught = (0..SEEDS).find(|seed| run(*seed, Variant::KeepsMissing).is_err());
    assert!(caught.is_some(), "no seed caught a missing candidate being kept");
}

#[test]
fn the_key_case_an_undone_merge_is_offered_again() {
    // Branch, edit both sides, merge, undo the merge, merge again: the branch's changes come back (design 12.3).
    let mut world = World::new(Variant::Real);
    let mut rng = Rng::new(7);
    world.fork(0, &mut rng);
    let theirs_token = world.token();
    world.branches[1].live.tokens.insert(theirs_token);
    let ours_token = world.token();
    world.branches[0].live.tokens.insert(ours_token);
    world.merge(0, 1, true, &mut rng).unwrap_or_else(|e| panic!("{e}"));
    assert!(world.branches[0].live.tokens.contains(&theirs_token));
    // A checkpoint while merged, then the undo: the record goes with the merge.
    world.checkpoint(0);
    let before = world.branches[0].undo.pop().unwrap_or_else(|| panic!("an undo"));
    world.branches[0].live = before;
    assert!(!world.branches[0].live.tokens.contains(&theirs_token));
    world.merge(0, 1, true, &mut rng).unwrap_or_else(|e| panic!("{e}"));
    assert!(world.branches[0].live.tokens.contains(&theirs_token), "offered again, not dropped as removed");

    let mut wrong = World::new(Variant::RecordFromHead);
    wrong.fork(0, &mut rng);
    wrong.branches[1].live.tokens.insert(theirs_token);
    wrong.merge(0, 1, true, &mut rng).unwrap_or_else(|e| panic!("{e}"));
    wrong.checkpoint(0);
    let before = wrong.branches[0].undo.pop().unwrap_or_else(|| panic!("an undo"));
    wrong.branches[0].live = before;
    assert!(wrong.merge(0, 1, true, &mut rng).is_err(), "the newest point's record would drop theirs' change");
}
