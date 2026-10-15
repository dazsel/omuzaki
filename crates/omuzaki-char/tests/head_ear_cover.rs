//! The ear cover as a fact (`EarCoverFact`): the `head` part publishes the disc each shell ear hides, and a part built
//! after it reads both discs from `ctx.published`. Human and elf ears publish one disc per side; animal ears and no ears
//! publish none.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason

use std::sync::Arc;

use omuzaki_char::head::ears::fact::EarCoverFact;
use omuzaki_char::head::ears::{self, Side};
use omuzaki_char::template::Template;
use omuzaki_char::template::skeleton::TemplateFit;
use omuzaki_char::template::soft_sd::SoftSd;
use omuzaki_char_api::{
    AnchorId, BodyFit, BodyParams, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId,
    CharacterBuilder, CharacterDef, EarKind, Ears, FitFact, HeadParams, PartCategory, PartDef, PartId, PartKind,
    PartKindId, PartMeta, PartOutput, Publishes, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_face_api::{Check, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The body the head sits on: publishes the soft-sd fit, which is all the head needs of a body.
struct Body;

impl PartKind for Body {
    type Params = BodyParams;

    fn meta() -> PartMeta {
        PartMeta { id: "test-cover-body", summary: "a stand-in body", category: PartCategory::Body, requires: &[] }
    }

    fn build(p: &BodyParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let layout = SoftSd.layout(&p.proportions, &p.shape, &p.hand);
        let fit: Arc<dyn BodyFit> = Arc::new(TemplateFit::new(layout));
        Ok(PartOutput::new().with_publishes(Publishes::default().with::<FitFact>(fit)))
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
        let warnings = ctx
            .published
            .get::<EarCoverFact>()
            .iter()
            .map(|((_, side), cover)| format!("{side:?} {cover:?}"))
            .collect();
        Ok(PartOutput::new().with_warnings(warnings))
    }
}

omuzaki_char_api::register_part_kind!(Reader);

fn pid(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

/// The mock character with the stand-in body, the head with `ears` and the reader after it.
fn built_with(ears: &Ears) -> BuiltCharacter {
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
    def.parts.insert(pid("body"), part("test-cover-body", json!({})));
    let head = HeadParams { ears: ears.clone(), ..HeadParams::default() };
    def.parts.insert(pid("head"), part("head", serde_json::to_value(head).unwrap()));
    def.parts.insert(pid("hair"), part("test-cover-reader", json!({})));
    let id = CharId::new("t").unwrap();
    RegistryBuilder.build(&id, &def, &BuildOptions::default(), &BuildServices::default()).unwrap()
}

/// The warnings the reader made: one line per cover it read, in the order it read them.
fn read_covers(built: &BuiltCharacter) -> Vec<String> {
    let reader = built.report.parts.iter().find(|p| p.part == pid("hair")).expect("the reader was built");
    reader.warnings.clone()
}

/// What each side's cover should read as: the ear's own disc, rooted where its anchor sits.
fn expected_covers(built: &BuiltCharacter, ears: &Ears) -> Vec<String> {
    Side::BOTH
        .into_iter()
        .map(|side| {
            let name: AnchorId = ears::anchor_name(side).parse().unwrap();
            let root = built.anchors[&(pid("head"), name)].position;
            let cover = ears::cover(ears, side, root).expect("a shell ear covers its side");
            format!("{side:?} {cover:?}")
        })
        .collect()
}

#[test]
fn a_human_ear_publishes_its_disc_on_each_side_for_a_later_part_to_read() {
    let ears = Ears::default();
    let built = built_with(&ears);
    let read = read_covers(&built);
    assert_eq!(read.len(), 2, "one disc per side: {read:?}");
    assert!(read[0].starts_with("Left") && read[1].starts_with("Right"), "left first: {read:?}");
    assert_eq!(read, expected_covers(&built, &ears));
}

#[test]
fn an_elf_ear_publishes_its_disc_on_each_side_too() {
    let ears = Ears { kind: EarKind::Elf, ..Ears::default() };
    let built = built_with(&ears);
    assert_eq!(read_covers(&built), expected_covers(&built, &ears));
    assert_eq!(read_covers(&built).len(), 2);
}

#[test]
fn animal_ears_and_no_ears_publish_no_cover() {
    for kind in [EarKind::Animal, EarKind::None] {
        let built = built_with(&Ears { kind, ..Ears::default() });
        assert!(read_covers(&built).is_empty(), "{kind:?} stands on top of the head, not on its side");
    }
}
