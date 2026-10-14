//! Curtain generator (guides, wave and curl, strip columns); stub, not implemented yet.
//!
//! The dispatch ([`crate::hair::compile`]) already calls [`compile`] and [`clump_count`]; until the generator is
//! written it makes no clumps. Keep the two in step: the build refuses a generator whose count differs, because a
//! recolour relies on the count to find each clump's vertices.

use omuzaki_char_api::CurtainGen;

use crate::hair::compile::{CompiledGroup, GroupGeometry, HairGenCtx};

/// Compiles the group: no clumps yet.
pub fn compile(g: &CurtainGen, _ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    CompiledGroup::empty(GroupGeometry::Strip { columns: g.columns, u: Vec::new() })
}

/// How many clumps [`compile`] makes, from the parameters alone.
pub fn clump_count(_g: &CurtainGen) -> usize {
    0
}
