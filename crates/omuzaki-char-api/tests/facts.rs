//! Facts: the merge rules, the two conflicts the builder reports, and the point of the design: a crate defines a new
//! fact and a part kind that publishes and reads it without editing `omuzaki-char-api`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_char_api::{
    Agree, Append, BodyFit, BodyRegion, BuildCtx, BuildError, BuildOptions, BuildServices, CharId, CharacterBuilder,
    CharacterDef, Fact, FitFact, JointRef, Keyed, Last, PartCategory, PartDef, PartId, PartKind, PartKindId, PartMeta,
    PartOutput, Published, Publishes, RegistryBuilder, Section, Stature, StatureFact,
};
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_face_api::{Check, FaceProfile, Palette, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

fn pid(name: &str) -> PartId {
    PartId::new(name).unwrap()
}

// ---- one fact per merge rule

omuzaki_char_api::fact! {
    /// The last word wins.
    LastFact = "test-last", Last<u32>;
}
omuzaki_char_api::fact! {
    /// Everyone must agree.
    AgreeFact = "test-agree", Agree<u32>;
}
omuzaki_char_api::fact! {
    /// Everything, in part order.
    AppendFact = "test-append", Append<&'static str>;
}
omuzaki_char_api::fact! {
    /// Everything, by publishing part and name.
    KeyedFact = "test-keyed", Keyed<&'static str, u32>;
}
omuzaki_char_api::register_fact!(LastFact);
omuzaki_char_api::register_fact!(AgreeFact);
omuzaki_char_api::register_fact!(AppendFact);
omuzaki_char_api::register_fact!(KeyedFact);

#[test]
fn nothing_published_reads_as_the_default() {
    let published = Published::default();
    assert_eq!(*published.get::<LastFact>(), None);
    assert!(published.get::<AppendFact>().is_empty() && published.get::<KeyedFact>().is_empty());
}

#[test]
fn last_keeps_the_value_of_the_last_part_to_publish() {
    let mut published = Published::default();
    published.absorb(&pid("a"), Publishes::default().with::<LastFact>(1)).unwrap();
    published.absorb(&pid("silent"), Publishes::default()).unwrap();
    published.absorb(&pid("b"), Publishes::default().with::<LastFact>(2)).unwrap();
    assert_eq!(*published.get::<LastFact>(), Some(2));
}

#[test]
fn agree_takes_an_equal_value_and_refuses_a_different_one_without_absorbing_anything() {
    let mut published = Published::default();
    published.absorb(&pid("a"), Publishes::default().with::<AgreeFact>(7)).unwrap();
    published.absorb(&pid("b"), Publishes::default().with::<AgreeFact>(7)).unwrap();
    let both = Publishes::default().with::<AppendFact>("kept?").with::<AgreeFact>(8);
    let Err(BuildError::Part { part, message }) = published.absorb(&pid("c"), both) else {
        panic!("a different second value is an error");
    };
    assert_eq!(part.as_str(), "c");
    assert!(message.contains("different value"), "{message}");
    assert_eq!(*published.get::<AgreeFact>(), Some(7));
    assert!(published.get::<AppendFact>().is_empty(), "nothing of the refused part is absorbed");
}

#[test]
fn append_collects_in_part_order_and_put_order() {
    let mut published = Published::default();
    let mut first = Publishes::default().with::<AppendFact>("a1");
    first.put::<AppendFact>("a2");
    published.absorb(&pid("a"), first).unwrap();
    published.absorb(&pid("b"), Publishes::default().with::<AppendFact>("b1")).unwrap();
    assert_eq!(published.get::<AppendFact>(), &["a1", "a2", "b1"]);
}

#[test]
fn keyed_is_per_publishing_part_and_a_key_published_again_replaces() {
    let mut published = Published::default();
    published.absorb(&pid("a"), Publishes::default().with::<KeyedFact>(("x", 1))).unwrap();
    published.absorb(&pid("b"), Publishes::default().with::<KeyedFact>(("x", 2))).unwrap();
    published.absorb(&pid("a"), Publishes::default().with::<KeyedFact>(("x", 3))).unwrap();
    let keyed = published.get::<KeyedFact>();
    assert_eq!(keyed.len(), 2);
    assert_eq!((keyed[&(pid("a"), "x")], keyed[&(pid("b"), "x")]), (3, 2));
}

#[test]
fn facts_are_listed_by_name_and_the_publishes_of_a_part_know_if_they_are_silent() {
    assert!(Publishes::default().is_empty());
    let publishes = Publishes::default().with::<LastFact>(1);
    assert!(!publishes.is_empty());
    assert_eq!(publishes.values::<LastFact>().collect::<Vec<_>>(), [&1]);
    assert_eq!(format!("{publishes:?}"), r#"{"test-last": 1}"#);
    assert_eq!(<LastFact as Fact>::NAME, "test-last");
}

// ---- the two conflicts builds report today, with their messages

#[derive(Debug)]
struct Sculpted(HumanoidPose);

impl BodyFit for Sculpted {
    fn section(&self, _: BodyRegion, _: f32) -> Option<Section> {
        None
    }
    fn joint(&self, _: &JointRef) -> Option<Vec3> {
        None
    }
    fn bind(&self) -> HumanoidPose {
        self.0
    }
}

fn posed(angle: f32) -> HumanoidPose {
    let mut pose = HumanoidPose::REST;
    pose.set(HumanoidBone::LeftUpperArm, Quat::from_rotation_z(angle));
    pose
}

#[test]
fn two_different_bind_poses_are_a_part_error_naming_the_first_part() {
    let mut published = Published::default();
    published.absorb(&pid("body"), Publishes::default().with::<FitFact>(Arc::new(Sculpted(posed(-0.5))))).unwrap();
    published.absorb(&pid("coat"), Publishes::default().with::<FitFact>(Arc::new(Sculpted(posed(-0.5))))).unwrap();
    let Err(BuildError::Part { part, message }) =
        published.absorb(&pid("cape"), Publishes::default().with::<FitFact>(Arc::new(Sculpted(posed(-0.9)))))
    else {
        panic!("a second, different bind must be refused");
    };
    assert_eq!(part.as_str(), "cape");
    assert_eq!(
        message,
        "its fit is bound in a different pose than the fit of part 'body': a character has one bind pose (decision \
         0014), so both parts must sculpt in the same one"
    );
    assert_eq!(published.get::<FitFact>().fits.len(), 2);
    assert_eq!(published.get::<FitFact>().bound_by.as_ref().map(|(p, _)| p.as_str()), Some("body"));
    assert_eq!(published.bind(), posed(-0.5));
}

#[test]
fn a_second_different_stature_and_an_unusable_one_are_part_errors() {
    let mut published = Published::default();
    let stature = Stature { scale: 2.0, height_m: 1.8 };
    published.absorb(&pid("body"), Publishes::default().with::<StatureFact>(stature)).unwrap();
    published.absorb(&pid("again"), Publishes::default().with::<StatureFact>(stature)).unwrap();
    assert_eq!(published.scale(), 2.0);

    let other = Stature { scale: 1.0, height_m: 1.8 };
    let Err(BuildError::Part { part, message }) =
        published.absorb(&pid("tall"), Publishes::default().with::<StatureFact>(other))
    else {
        panic!("a second, different stature must be refused");
    };
    assert_eq!(part.as_str(), "tall");
    assert_eq!(message, "it publishes a different stature than an earlier part: a character has one stature");

    let Err(BuildError::Part { message, .. }) = Published::default()
        .absorb(&pid("bad"), Publishes::default().with::<StatureFact>(Stature { scale: 0.0, height_m: 1.8 }))
    else {
        panic!("a stature that fails its check must be refused");
    };
    assert_eq!(message, Stature { scale: 0.0, height_m: 1.8 }.check().unwrap_err());
}

// ---- the point: a new fact and kinds that use it, defined here and nowhere in `omuzaki-char-api`

#[derive(Debug, Clone, PartialEq)]
struct Perch {
    height: f32,
}

omuzaki_char_api::fact! {
    /// Where a bird can sit, published by anything that has a ledge.
    PerchFact = "test-perch", Append<Perch>;
}
omuzaki_char_api::register_fact!(PerchFact);

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Ledge {
    height: f32,
}

impl Validate for Ledge {
    fn check(&self, c: &mut Check) {
        c.range("height", f64::from(self.height), 0.0, 3.0);
    }
}

struct LedgePart;

impl PartKind for LedgePart {
    type Params = Ledge;

    fn meta() -> PartMeta {
        PartMeta { id: "test-ledge", summary: "Publish a perch.", category: PartCategory::Accessory, requires: &[] }
    }

    fn build(params: &Ledge, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        Ok(PartOutput {
            publishes: Publishes::default().with::<PerchFact>(Perch { height: params.height }),
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(LedgePart);

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct NoParams {}

impl Validate for NoParams {
    fn check(&self, _: &mut Check) {}
}

struct BirdPart;

impl PartKind for BirdPart {
    type Params = NoParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-bird",
            summary: "Report the perches it can see.",
            category: PartCategory::Accessory,
            requires: &["test-ledge"],
        }
    }

    fn build(_: &NoParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let heights: Vec<_> = ctx.published.get::<PerchFact>().iter().map(|p| p.height).collect();
        Ok(PartOutput { warnings: vec![format!("perches at {heights:?}")], ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(BirdPart);

fn part(kind: &str, layer: i32, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer, attach: None, params }
}

#[test]
fn a_fact_and_kinds_defined_outside_the_api_crate_publish_and_read_through_a_build() {
    let def = CharacterDef {
        name: "Test".to_owned(),
        template: omuzaki_char_api::BodyTemplateId::new("soft-sd").unwrap(),
        seed: 1,
        palette: Palette::default(),
        skin_tone: None,
        parts: BTreeMap::from([
            (pid("body"), common::body_part()),
            (pid("bird"), part("test-bird", 0, json!({}))),
            (pid("low"), part("test-ledge", 0, json!({"height": 0.5}))),
            (pid("high"), part("test-ledge", 0, json!({"height": 2.0}))),
        ]),
        face: FaceProfile::default(),
        weight_brushes: BTreeMap::new(),
    };
    let built = RegistryBuilder
        .build(&CharId::new("mira").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    let bird = built.report.parts.iter().find(|p| p.part.as_str() == "bird").unwrap();
    assert_eq!(
        bird.warnings,
        ["perches at [2.0, 0.5]"],
        "the bird sees both ledges, in build order (part id within a layer)"
    );
}
