//! The palette gate for the stage, beside its link gate (`registrations.rs`, ADR 0015): every part kind linked into the
//! binary publishes the same facts under any palette, reads colours only through its parameters and recolours to what
//! a fresh build makes, on the kit's own characters (#1635). A kind from any crate that breaks one of these fails here,
//! named with its fact.

use omuzaki_char::builder::conformance::{Sweep, check_linked};
use omuzaki_char_api::BuildServices;
use omuzaki_face_mock::MockRasterizer;

#[test]
fn every_part_kind_linked_with_the_stage_keeps_the_palette_contracts() {
    omuzaki_stage::link(); // as every App does at startup
    let services = BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer));
    let report = check_linked(&services, Sweep::QUICK).unwrap_or_else(|e| panic!("the kit characters build: {e}"));
    assert!(report.violations.is_empty(), "{:#?}", report.violations);
    assert!(report.unprobed.is_empty(), "facts no probe compares: {:?}", report.unprobed);
    assert!(report.unreached.is_empty(), "linked kinds the check cannot build: {:#?}", report.unreached);
}
