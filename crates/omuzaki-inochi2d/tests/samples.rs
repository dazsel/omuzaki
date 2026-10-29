//! Real puppets, read from paths outside the repository (third-party art never enters it). A hand check:
//!
//! ```text
//! INOCHI2D_SAMPLES=/path/to/a.inx:/path/to/b.inp cargo test -p omuzaki-inochi2d --test samples -- --ignored --nocapture
//! ```
//!
//! Each file must import, pass the contract's checks, and write back out and read again with the same nodes,
//! parameters and pixels.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::collections::BTreeMap;

use omuzaki_inochi2d::{Limits, import, write_inp1};

#[test]
#[ignore = "needs INOCHI2D_SAMPLES: paths to real puppets, which stay outside the repository"]
fn real_puppets_import() {
    let paths = std::env::var("INOCHI2D_SAMPLES").unwrap_or_default();
    for path in paths.split(':').filter(|p| !p.is_empty()) {
        let bytes = std::fs::read(path).unwrap();
        let started = std::time::Instant::now();
        let imported = import(&bytes, &Limits::default()).unwrap_or_else(|e| panic!("{path}: {e}"));
        let elapsed = started.elapsed();
        imported.def.check().unwrap();
        let mut by_rule: BTreeMap<&str, usize> = BTreeMap::new();
        for f in &imported.report.findings {
            *by_rule.entry(f.id.as_str()).or_default() += 1;
        }
        println!("{path}: {} bytes in {elapsed:?}", bytes.len());
        println!("  mapped: {:?}", imported.report.mapped);
        println!("  canvas: {:?}", imported.def.canvas);
        println!("  findings by rule: {by_rule:?}");
        // Stored premultiplied: no colour channel above its alpha (a few may be, from rounding or stray pixels).
        let (mut over, mut total) = (0usize, 0usize);
        for t in &imported.textures {
            for px in t.rgba.as_chunks::<4>().0 {
                total += 1;
                over += usize::from(px[0].max(px[1]).max(px[2]) > px[3]);
            }
        }
        println!("  pixels with a colour above their alpha: {over} of {total}");

        let written = write_inp1(&imported.def, &imported.textures).unwrap();
        let again = import(&written, &Limits::default()).unwrap();
        assert_eq!(again.def.nodes.len(), imported.def.nodes.len() + extra_physics_nodes(&imported.def));
        assert_eq!(again.def.params.len(), imported.def.params.len());
        assert_eq!(again.textures, imported.textures);
        println!("  written back: {} bytes, read again", written.len());
    }
}

/// Pendulums the writer must give a node of their own (see `write_inp1`).
fn extra_physics_nodes(def: &omuzaki_puppet_api::PuppetDef) -> usize {
    def.physics
        .iter()
        .filter(|p| {
            let alone = def.physics.iter().filter(|q| q.anchor == p.anchor).count() == 1;
            let anchor = &def.nodes[p.anchor.get()];
            !(alone && matches!(anchor.kind, omuzaki_puppet_api::NodeKind::Group) && anchor.name == p.name)
        })
        .count()
}
