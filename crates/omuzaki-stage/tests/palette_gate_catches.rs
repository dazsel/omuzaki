//! The palette gate (`palette_gate.rs`, #1635) catches a kind from outside the Character area: this binary links the
//! stage plus a test-only kind whose published fact is a channel of a palette colour, and the check names that kind and
//! its fact. It is its own binary because registrations are per binary: in `palette_gate.rs` the kind would fail the
//! real gate.

use omuzaki_char::builder::conformance::{Sweep, check_linked};
use omuzaki_char_api::{
    BuildCtx, BuildError, BuildServices, Last, PartCategory, PartKind, PartMeta, PartOutput, Publishes,
};
use omuzaki_face_api::{Check, ColourRef, Validate};
use omuzaki_face_mock::MockRasterizer;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

omuzaki_char_api::fact! {
    /// The red channel of a palette colour: a fact that depends on the palette, which the gate must refuse.
    TintFact = "t-gate-tint", Last<u8>;
}
omuzaki_char_api::register_fact!(TintFact);
omuzaki_char_api::register_fact_probe!(TintFact);

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct BadgeParams {
    tint: ColourRef,
}

impl Default for BadgeParams {
    fn default() -> Self {
        Self { tint: ColourRef::slot_or("skin", ColourRef::rgb(200, 200, 200)) }
    }
}

impl Validate for BadgeParams {
    fn check(&self, _: &mut Check) {}
}

/// Publishes its tint's red channel.
struct Badge;

impl PartKind for Badge {
    type Params = BadgeParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "t-gate-badge",
            summary: "Publishes its colour.",
            category: PartCategory::Accessory,
            requires: &[],
        }
    }

    fn build(params: &BadgeParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let tint = ctx.colour(&params.tint)?;
        Ok(PartOutput::new().with_publishes(Publishes::default().with::<TintFact>(tint.r)))
    }
}
omuzaki_char_api::register_part_kind!(Badge);

#[test]
fn the_gate_names_a_linked_kind_whose_fact_depends_on_the_palette() {
    omuzaki_stage::link();
    let services = BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer));
    let report = check_linked(&services, Sweep::QUICK).unwrap_or_else(|e| panic!("the kit characters build: {e}"));
    assert!(
        report
            .violations
            .iter()
            .any(|v| v.contains("kind 't-gate-badge'") && v.contains("fact 't-gate-tint' depends on the palette")),
        "{:#?}\nunreached: {:#?}",
        report.violations,
        report.unreached
    );
    // Only the test kind breaks the contract: the real kinds the stage links are clean.
    assert!(report.violations.iter().all(|v| v.contains("'t-gate-badge'")), "{:#?}", report.violations);
}
