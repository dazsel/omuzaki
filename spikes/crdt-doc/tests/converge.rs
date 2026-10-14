//! acceptance: three peers, random edits, random delivery order, convergence over many runs.
use crdt_doc_spike::synth::{Rng, genesis, random_patch};
use crdt_doc_spike::{Change, Patch, Peer, Refused, conflicts, lookup};
use proptest::prelude::*;
use serde_json::{Value, json};

fn three_peers(files: usize) -> Vec<Peer> {
    let mut base = Peer::new(1);
    let genesis = base.apply(&genesis(files)).unwrap_or_default();
    let peers = vec![base, Peer::new(2), Peer::new(3)];
    peers[1..].iter().for_each(|p| p.import(&genesis));
    peers
}

/// Runs one random session; returns (peers, number of conflicts found).
fn session(seed: u64, rounds: usize) -> (Vec<Peer>, usize) {
    let mut r = Rng(seed | 1);
    let mut peers = three_peers(4);
    let mut inflight: Vec<(usize, Vec<u8>)> = Vec::new();
    for _ in 0..rounds {
        let p = r.below(3);
        let patch = random_patch(&mut r, &peers[p].value());
        inflight.push((p, peers[p].apply(&patch).unwrap_or_default()));
        // Deliver a random update to a random peer (reordered, duplicated, and sometimes before its parents).
        for _ in 0..r.below(3) {
            let (from, b) = inflight[r.below(inflight.len())].clone();
            let to = (from + 1 + r.below(2)) % 3;
            peers[to].import(&b);
        }
    }
    for p in &peers {
        for (_, b) in &inflight {
            p.import(b);
        }
    }
    let all: Vec<_> = peers.iter().flat_map(|p| p.log.clone()).collect();
    (peers, conflicts(&all).len())
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 1000, ..ProptestConfig::default() })]
    #[test]
    fn three_peers_converge(seed in any::<u64>(), rounds in 10usize..60) {
        let (peers, _) = session(seed, rounds);
        let v0 = peers[0].value();
        prop_assert_eq!(&v0, &peers[1].value());
        prop_assert_eq!(&v0, &peers[2].value());
    }
}

#[test]
fn some_runs_really_conflict() {
    let n: usize = (0..50).map(|s| session(s * 7919 + 1, 40).1).sum();
    assert!(n > 0, "the generator never produced concurrent edits to one unit");
}

#[test]
fn two_peers_set_one_field() {
    let mut p = three_peers(1);
    let f: Vec<String> = ["files", "f000", "transform", "x"].map(String::from).to_vec();
    let (a, b) = (json!(11), json!(22));
    let mk = |after: &Value| Patch {
        changes: vec![Change { path: f.clone(), before: Some(json!(0)), after: Some(after.clone()) }],
    };
    let ua = p[1].apply(&mk(&a)).unwrap_or_default();
    let ub = p[2].apply(&mk(&b)).unwrap_or_default();
    p[1].import(&ub);
    p[2].import(&ua);
    let (end_a, end_b) = (lookup(&p[1].value(), &f).cloned(), lookup(&p[2].value(), &f).cloned());
    println!("peer 2 set 11, peer 3 set 22; they end with {end_a:?} and {end_b:?}");
    assert_eq!(end_a, end_b, "converged to one value");
    assert!(end_a == Some(a.clone()) || end_a == Some(b.clone()));
    let all: Vec<_> = p.iter().flat_map(|x| x.log.clone()).collect();
    assert_eq!(conflicts(&all).len(), 1, "the log still shows two concurrent writes, so a conflict item can be built");
    // The loser's own undo expects its value back; the document holds the winner's, so the undo is refused.
    let (loser, mine) = if end_a == Some(a.clone()) { (2, &b) } else { (1, &a) };
    let undo = mk(mine).inverse();
    assert_eq!(p[loser].apply(&undo), Err(Refused::Conflict(f)));
}

/// One peer applies up to 30 seeded random patches; returns it, its starting value and the patches that applied.
fn solo(seed: u64) -> (Peer, Value, Vec<Patch>) {
    let (mut p, mut r, mut done) = (three_peers(2).remove(0), Rng(seed), Vec::new());
    let start = p.value();
    for _ in 0..30 {
        let q = random_patch(&mut r, &p.value());
        if p.apply(&q).is_ok() {
            done.push(q);
        }
    }
    (p, start, done)
}

#[test]
fn inverse_restores_exactly() {
    let (mut p, start, done) = solo(9);
    assert!(done.iter().rev().all(|q| p.apply(&q.inverse()).is_ok()));
    assert_eq!(p.value(), start);
}

#[test]
fn same_ops_same_bytes() {
    let snapshot = |s| solo(s).0.doc.export(loro::ExportMode::Snapshot).unwrap_or_default();
    assert_eq!(snapshot(5), snapshot(5));
}

#[test]
fn concurrent_object_creation_keeps_one_subtree() {
    let mut p = three_peers(1);
    let at: Vec<String> = ["files", "f000", "extra0"].map(String::from).to_vec();
    let mk = |v| Patch { changes: vec![Change { path: at.clone(), before: None, after: Some(v) }] };
    let (x, y) = (json!({"a": 1}), json!({"b": 2}));
    let (ux, uy) = (p[1].apply(&mk(x.clone())).unwrap_or_default(), p[2].apply(&mk(y.clone())).unwrap_or_default());
    p[1].import(&uy);
    p[2].import(&ux);
    println!("both created extra0: {:?} / {:?}", lookup(&p[1].value(), &at).cloned(), lookup(&p[2].value(), &at).cloned());
    assert_eq!(lookup(&p[1].value(), &at).cloned(), lookup(&p[2].value(), &at).cloned());
    assert!(lookup(&p[1].value(), &at).cloned() == Some(x) || lookup(&p[1].value(), &at).cloned() == Some(y), "one whole subtree wins, the other is dropped");
}

#[test]
fn a_patch_is_validated_in_order_and_refused_whole() {
    let mut p = three_peers(1).remove(0);
    let (before, f) = (p.value(), |s: &str| ["files", "f000", s].map(String::from).to_vec());
    let (x, tags) = (f("transform"), f("tags"));
    let change = |path: Vec<String>, b: Option<Value>, a: Option<Value>| Change { path, before: b, after: a };
    let at = |i: &str| [tags.clone(), vec![i.into()]].concat();
    // The second change deletes `transform` with its original `before`, but the first already changed it.
    let stale = Patch { changes: vec![change([x.clone(), vec!["x".into()]].concat(), Some(json!(0)), Some(json!(5))), change(x.clone(), lookup(&p.value(), &x).cloned(), None)] };
    assert_eq!(p.apply(&stale), Err(Refused::Conflict(x)));
    // Append and remove on an array are refused, not silently dropped.
    assert!(matches!(p.apply(&Patch { changes: vec![change(at("3"), None, Some(json!(1)))] }), Err(Refused::Unsupported(_))));
    assert!(matches!(p.apply(&Patch { changes: vec![change(at("0"), Some(json!("a")), None)] }), Err(Refused::Unsupported(_))));
    assert_eq!(p.value(), before, "nothing was written");
    // A replaced object that is then edited and deleted in one patch must not take its parent's keys with it.
    let (o, v) = (f("obj"), |x: i64| json!({"x": x}));
    let setup = Patch { changes: vec![change(o.clone(), None, Some(v(0))), change(f("x"), None, Some(json!(99)))] };
    assert!(p.apply(&setup).is_ok());
    let nested = Patch { changes: vec![
        change(o.clone(), Some(v(0)), Some(v(1))),
        change([o.clone(), vec!["x".into()]].concat(), Some(json!(1)), Some(json!(2))),
        change(o, Some(v(2)), None),
    ] };
    let (mid, applied) = (p.value(), p.apply(&nested).is_ok());
    assert!(applied && p.apply(&nested.inverse()).is_ok());
    assert_eq!(p.value(), mid, "the inverse restores the whole document, including the parent's `x`");
}
