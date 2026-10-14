//! Bangs generator (fringe layers, sweep, tip hook, wisps); stub, not implemented yet.
//!
//! The dispatch ([`crate::hair::compile`]) already calls [`compile`] and [`clump_count`]; until the generator is
//! written it makes no clumps. Keep the two in step: the build refuses a generator whose count differs, because a
//! recolour relies on the count to find each clump's vertices.

use omuzaki_char_api::BangsGen;

use crate::hair::compile::{CompiledGroup, GroupGeometry, HairGenCtx};

/// Compiles the group: no clumps yet.
pub fn compile(_g: &BangsGen, _ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    CompiledGroup::empty(GroupGeometry::Tube)
}

/// How many clumps [`compile`] makes, from the parameters alone.
pub fn clump_count(_g: &BangsGen) -> usize {
    0
}
