//! The ear cover as a fact (`EarCoverFact`): the `head` part publishes the disc each shell ear hides, at real size, and a
//! part built after it reads both discs from `ctx.published`. Human and elf ears publish one disc per side; animal ears
//! and no ears publish none. On a scaled character the discs scale with the head, and stay on the scaled anchors.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_char::head::ears::fact::EarCoverFact;
use omuzaki_char::head::ears::{self, Frame, Side};
use omuzaki_char::template::registry;
use omuzaki_char::template::skeleton::TemplateFit;
use omuzaki_char_api::{
    AnchorId, BodyFit, BodyShape, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId,
    CharacterBuilder, CharacterDef, EarKind, Ears, FitFact, HandParams, HeadParams, PartCategory, PartDef, PartId,
    PartKind, PartKindId, PartMeta, PartOutput, Proportions, Publishes, RegistryBuilder, StatureFact,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::{Check, Validate, narrow};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The disc's radius at size 1, metres (`HALF_HEIGHT` in `ears::mod`).
const RADIUS_AT_SIZE_ONE: f32 = 0.043;

/// The body the head sits on: a named template's fit and stature, as the real body kind publishes them.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct BodyParams {
    template: String,
}

impl Default for BodyParams {
    fn default() -> Self {
        Self { template: "standard".to_owned() }
    }
}

impl Validate for BodyParams {
    fn check(&self, _: &mut Check) {}
}

struct Body;

impl PartKind for Body {
    type Params = BodyParams;

    fn meta() -> PartMeta {
        PartMeta { id: "test-cover-body", summary: "a stand-in body", category: PartCategory::Body, requires: &[] }
    }

    fn build(p: &BodyParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let t = registry::find(&p.template).ok_or_else(|| ctx.fail("no such template"))?;
        let shape = BodyShape::default();
        let layout = t.layout(&Proportions::default(), &shape, &HandParams::default());
        let stature = t.stature(&layout).ok_or_else(|| ctx.fail("no stature"))?;
        let fit: Arc<dyn BodyFit> = Arc::new(TemplateFit::new(layout));
        Ok(PartOutput::new().with_publishes(Publishes::default().with::<FitFact>(fit).with::<StatureFact>(stature)))
    }
}

omuzaki_char_api::register_part_kind!(Body);

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct NoParams {}

impl Validate for NoParams {
    fn check(&self, _: &mut Check) {}
}

/// A hair-category stand-in that reads the covers and reports each one as a warning, which is what the test reads back.
struct Reader;

impl PartKind for Reader {
    type Params = NoParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-cover-reader",
            summary: "reports the ear covers it reads",
            category: PartCategory::Hair,
            requires: &["head"],
        }
    }

    fn build(_: &NoParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        // Shortest round-trip floats, so the test reads back the exact values.
        let warnings = ctx
            .published
            .get::<EarCoverFact>()
            .iter()
            .map(|((_, side), c)| {
                let (p, n) = (c.centre, c.normal);
                format!("{side:?} {} {} {} {} {} {} {}", p.x, p.y, p.z, n.x, n.y, n.z, c.radius)
            })
            .collect();
        Ok(PartOutput::new().with_warnings(warnings))
    }
}

omuzaki_char_api::register_part_kind!(Reader);

fn pid(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

/// The mock character with the stand-in body on `template`, the head with `ears` and the reader after it.
fn built_with(template: &str, ears: &Ears) -> BuiltCharacter {
    let part = |kind: &str, params: Value| PartDef {
        kind: PartKindId::new(kind).unwrap(),
        parent: None,
        enabled: true,
        layer: 0,
        attach: None,
        params,
    };
    let mut def: CharacterDef = mock_character().unwrap();
    def.parts.clear();
    def.parts.insert(pid("body"), part("test-cover-body", json!({ "template": template })));
    let head = HeadParams { ears: ears.clone(), ..HeadParams::default() };
    def.parts.insert(pid("head"), part("head", serde_json::to_value(head).unwrap()));
    def.parts.insert(pid("hair"), part("test-cover-reader", json!({})));
    let id = CharId::new("t").unwrap();
    RegistryBuilder.build(&id, &def, &BuildOptions::default(), &BuildServices::default()).unwrap()
}

/// One cover the reader made: its side, centre, normal and radius.
struct Read {
    side: String,
    centre: Vec3,
    normal: Vec3,
    radius: f32,
}

/// The covers the reader read, in the order it read them.
fn read_covers(built: &BuiltCharacter) -> Vec<Read> {
    let reader = built.report.parts.iter().find(|p| p.part == pid("hair")).expect("the reader was built");
    reader
        .warnings
        .iter()
        .map(|line| {
            let mut words = line.split(' ');
            let side = words.next().unwrap().to_owned();
            let f: Vec<f32> = words.map(|w| w.parse().unwrap()).collect();
            Read { side, centre: Vec3::new(f[0], f[1], f[2]), normal: Vec3::new(f[3], f[4], f[5]), radius: f[6] }
        })
        .collect()
}

/// What each side's cover should be, checked against the scaled anchor: the centre sits on it, the normal is the ear's
/// own, and the radius is the ear's at size, scaled by the character's stature like the head.
fn check_covers(built: &BuiltCharacter, ears: &Ears) {
    let s = built.stature.expect("the body published its stature").scale;
    let covers = read_covers(built);
    assert_eq!(covers.len(), 2, "one disc per side");
    for (cover, side) in covers.iter().zip(Side::BOTH) {
        assert_eq!(cover.side, format!("{side:?}"), "left first");
        let name: AnchorId = ears::anchor_name(side).parse().unwrap();
        let anchor = built.anchors[&(pid("head"), name)].position;
        assert!(
            (cover.centre - anchor).length() < 1e-5,
            "{side:?} centre {:?} off its anchor {anchor:?}",
            cover.centre
        );
        let frame = Frame::of(ears, side).normal.as_vec3();
        assert!((cover.normal - frame).length() < 1e-6, "{side:?} normal is the ear's, not scaled");
        let radius = RADIUS_AT_SIZE_ONE * narrow(ears.size) * s;
        assert!((cover.radius - radius).abs() < 1e-6, "{side:?} radius {} is not {radius}", cover.radius);
    }
}

#[test]
fn a_human_ear_publishes_its_disc_on_each_side_at_real_size_on_standard_and_tall() {
    for template in ["standard", "tall"] {
        let ears = Ears::default();
        let built = built_with(template, &ears);
        check_covers(&built, &ears);
    }
}

#[test]
fn the_discs_scale_with_the_stature_on_tall_as_the_anchors_do() {
    let (standard, tall) = (built_with("standard", &Ears::default()), built_with("tall", &Ears::default()));
    let (s_std, s_tall) = (standard.stature.unwrap().scale, tall.stature.unwrap().scale);
    assert!((s_tall - s_std).abs() > 1e-3, "the two templates have different scales: {s_std} and {s_tall}");
    let (r_std, r_tall) = (read_covers(&standard)[0].radius, read_covers(&tall)[0].radius);
    assert!((r_tall / r_std - s_tall / s_std).abs() < 1e-5, "the radius follows the head: {r_std} and {r_tall}");
}

#[test]
fn an_elf_ear_publishes_its_disc_on_each_side_too() {
    let ears = Ears { kind: EarKind::Elf, ..Ears::default() };
    check_covers(&built_with("tall", &ears), &ears);
}

#[test]
fn animal_ears_and_no_ears_publish_no_cover() {
    for kind in [EarKind::Animal, EarKind::None] {
        let built = built_with("tall", &Ears { kind, ..Ears::default() });
        assert!(read_covers(&built).is_empty(), "{kind:?} stands on top of the head, not on its side");
    }
}
