//! Costs (reported, never gating): update bytes per op, merge time, memory, snapshot size.
use std::time::Instant;

use crdt_doc_spike::synth::{Rng, genesis, random_patch};
use crdt_doc_spike::Peer;
use loro::ExportMode;

fn rss_kb() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find_map(|l| l.strip_prefix("VmRSS:")?.trim().trim_end_matches("kB").trim().parse().ok()).unwrap_or(0)
}

fn pct(v: &mut [usize], p: usize) -> usize {
    v.sort_unstable();
    v[(v.len() - 1) * p / 100]
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1e3
}

fn edit(p: &mut Peer, r: &mut Rng, n: usize) -> Vec<Vec<u8>> {
    (0..n)
        .map(|_| {
            let q = random_patch(r, &p.value());
            p.apply(&q).unwrap_or_default()
        })
        .collect()
}

fn main() {
    let (mut r, rss0, mut a) = (Rng(0x1787), rss_kb(), Peer::new(1));
    let genesis = a.apply(&genesis(500)).unwrap_or_default();
    println!("genesis: 500 files, update {} KB, RSS +{} MB", genesis.len() / 1024, (rss_kb() - rss0) / 1024);
    let ops = edit(&mut a, &mut r, 5000);
    let mut sizes: Vec<usize> = ops.iter().map(Vec::len).collect();
    println!("5000 patches: update bytes p50 {} p95 {}", pct(&mut sizes.clone(), 50), pct(&mut sizes, 95));
    let json = serde_json::to_vec(&a.value()).unwrap_or_default().len();
    let snap = a.doc.export(ExportMode::Snapshot).unwrap_or_default();
    let state = a.doc.export(ExportMode::state_only(None)).unwrap_or_default();
    println!("RSS +{} MB; snapshot {} KB, state-only {} KB, JSON {} KB", (rss_kb() - rss0) / 1024, snap.len() / 1024, state.len() / 1024, json / 1024);
    // Two peers diverge by 300 patches each from the same base, then exchange.
    let (mut b, mut c) = (Peer::new(2), Peer::new(3));
    b.import(&snap);
    c.import(&snap);
    let vb = b.doc.oplog_vv();
    edit(&mut b, &mut r, 300);
    edit(&mut c, &mut r, 300);
    let t = Instant::now();
    c.import(&b.doc.export(ExportMode::updates(&vb)).unwrap_or_default());
    println!("merge 300 vs 300, one batch: {:.1} ms", ms(t));
}
