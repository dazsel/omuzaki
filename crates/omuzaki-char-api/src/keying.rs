//! The compile-time guard for the caching builder's keys (ADR 0054 §4).
//!
//! A cached part output is valid only while everything the part can read is unchanged, so the builder keys each part on
//! those inputs. The inputs are fields of [`CharacterDef`], [`BuildOptions`], [`PartDef`] and [`BuildCtx`], and a field
//! added to one of them must not be silently left out of a key. The functions here destructure those structs
//! *exhaustively* (no `..`), next to how each field is keyed or why it is excluded, so a new field fails to compile in
//! this crate, which defines the types (where an exhaustive pattern of a `#[non_exhaustive]` struct still compiles),
//! until its author decides. The builder hashes only what these functions return, so it cannot key on anything else.
//!
//! The key values are the builder's business and have not changed: this module decides *which inputs* are keyed, not how
//! they are hashed.

use omuzaki_doc_api::Value;
use omuzaki_face_api::FaceProfile;

use crate::build::BuildOptions;
use crate::character::{Attach, CharacterDef, PartDef};
use crate::design::DesignDef;
use crate::ids::{BodyTemplateId, PartId, PartKindId};
use crate::kind::{BuildCtx, PartCategory, Quality};

/// The inputs every part's key shares: the parts read all of them.
#[derive(Debug, Clone, Copy)]
pub struct CommonKeyInputs<'a> {
    /// The body template.
    pub template: &'a BodyTemplateId,
    /// The character seed (it seeds each part's random numbers together with the part id).
    pub seed: u32,
    /// The level of detail.
    pub quality: Quality,
    /// Whether the build is for export.
    pub rig_export: bool,
    /// The face design.
    pub face: &'a FaceProfile,
}

/// What of the character and the options goes into every part's key.
///
/// Exhaustive on purpose (no `..`): a field added to the definition or the options fails to compile here until it is
/// keyed or excluded with a reason. `design` is keyed per part, through [`design_key_inputs`]. Excluded: `name` and
/// `skin_tone` (no part sees them), `weight_brushes` (applied by
/// the assembler, after the part is built), `variant` (recorded in the result; the caller applied it to `def`). The
/// palette and the parts are keyed per part, through [`part_key_inputs`] and the palette slots the parameters name.
pub fn common_key_inputs<'a>(def: &'a CharacterDef, options: &'a BuildOptions) -> CommonKeyInputs<'a> {
    let CharacterDef {
        name: _,
        template,
        seed,
        palette: _,
        skin_tone: _,
        parts: _,
        face,
        weight_brushes: _,
        design: _,
        #[cfg(feature = "growth-probe")]
            growth_probe: _, // excluded: nothing reads it
    } = def;
    let BuildOptions { variant: _, quality, rig_export } = options;
    CommonKeyInputs { template, seed: *seed, quality: *quality, rig_export: *rig_export, face }
}

/// The inputs of one part's key that come from its definition.
#[derive(Debug, Clone, Copy)]
pub struct PartKeyInputs<'a> {
    /// The part's kind.
    pub kind: &'a PartKindId,
    /// The parameters as stored.
    pub params: &'a Value,
    /// Where the part is fixed, if anywhere.
    pub attach: &'a Option<Attach>,
}

/// What of a part's definition goes into its key.
///
/// Exhaustive for the same reason as [`common_key_inputs`]. `parent`, `enabled` and `layer` only decide whether and when
/// the part builds, which the plan and the chain of earlier publishers cover. `attach` reaches the part through
/// [`BuildCtx::attach`], so it is keyed: editing it rebuilds the part and everything chained after it.
pub fn part_key_inputs(part_def: &PartDef) -> PartKeyInputs<'_> {
    let PartDef { kind, params, parent: _, enabled: _, layer: _, attach } = part_def;
    PartKeyInputs { kind, params, attach }
}

/// A compile-time guard that is never called: it names every [`BuildCtx`] field next to what covers it in the key, so
/// a field added to the context fails to compile here until someone decides how it is keyed.
#[allow(dead_code)] // its only job is to fail to compile
fn every_context_field_is_keyed_or_excluded(ctx: &BuildCtx<'_>) {
    let BuildCtx {
        part: _,       // keyed: the part's id
        template: _,   // keyed: `CommonKeyInputs::template`
        palette: _,    // keyed: the values of the slots the parameters name (contract 1)
        face: _,       // keyed: `CommonKeyInputs::face`
        rng: _,        // keyed: depends on the seed (`CommonKeyInputs::seed`) and the part's id
        published: _,  // keyed: the chain over the geometry keys of the earlier publishers
        quality: _,    // keyed: `CommonKeyInputs::quality`
        rig_export: _, // keyed: `CommonKeyInputs::rig_export`
        attach: _,     // keyed: `PartKeyInputs::attach`
        services: _,   // excluded (ADR 0012); the builder's `Stored::usable_with` checks what the part used
        #[cfg(feature = "growth-probe")]
            growth_probe: _, // excluded: nothing reads it
    } = ctx;
}

/// The inputs of one part's key that come from the design layer ([`DesignDef`]): what [`build_part`](crate::build_part)
/// applies to its output. Its layers are not among them: the assembler runs them after the part is built, as it applies
/// the weight brushes.
#[derive(Debug, Clone, Copy)]
pub struct DesignKeyInputs<'a> {
    /// The age, for the body and the head (its dials move them).
    pub age: Option<f64>,
    /// The dials, for the body and the head.
    pub dials: Option<&'a std::collections::BTreeMap<crate::ids::DialId, crate::design::DialSetting>>,
    /// The part's own sculpt strokes.
    pub strokes: &'a [crate::design::SculptStroke],
}

/// What of the design layer goes into the key of `part`, a part of `category`.
pub fn design_key_inputs<'a>(def: &'a CharacterDef, part: &PartId, category: PartCategory) -> DesignKeyInputs<'a> {
    // Exhaustive, like the others: `sculpt` is read per part below, `layers` run in the assembler.
    let DesignDef { age, dials, sculpt: _, layers: _ } = &*def.design;
    let shaped = matches!(category, PartCategory::Body | PartCategory::Head);
    DesignKeyInputs {
        age: if shaped { *age } else { None },
        dials: shaped.then_some(dials),
        strokes: def.design.strokes(part),
    }
}
