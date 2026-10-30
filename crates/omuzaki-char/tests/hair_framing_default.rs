//! The default framing locks frame the face on every template's real head: close to the head down to the jaw line, in
//! front of the ears, and never flaring out sideways past the head or hanging far below it.
//!
//! The locks are compiled inside a probe part (a hair part whose generator measures what the real generator made against
//! the head surface the head part published), so the surface is the template's own, not a stand-in.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason

use std::sync::Mutex;

use omuzaki_char::hair::compile::{CompiledGroup, Dispatch, GroupGenerator, HairGenCtx};
use omuzaki_char::hair::part::build_with;
use omuzaki_char_api::{
    BodyTemplateId, BuildCtx, BuildError, BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef,
    Dynamics, FramingGen, Generator, GroupId, GuidePoint, HairGroup, HairParams, PartCategory, PartDef, PartId,
    PartKind, PartKindId, PartMeta, PartOutput, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use serde_json::json;

/// What the probe saw: per rest point of every framing lock, its height over the head's surface, its place relative to
/// the head, and the head's own sizes.
#[derive(Clone, Copy, Debug)]
struct Sample {
    /// Distance outside the head surface along the direction from the centre, metres (negative: inside).
    outside: f32,
    /// Height over the head centre.
    y: f32,
    /// Sideways distance from the head centre.
    x: f32,
    /// The head's half-width.
    half_width: f32,
    /// Depth of the head's underside below its centre.
    underside: f32,
}

static SAMPLES: Mutex<Vec<Sample>> = Mutex::new(Vec::new());
/// The tests share the probe's samples, so one builds at a time.
static SERIAL: Mutex<()> = Mutex::new(());

/// The real generators, measuring every framing group they compile.
struct Probe;

impl GroupGenerator for Probe {
    fn compile(&self, generator: &Generator, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
        let group = Dispatch.compile(generator, ctx);
        if matches!(generator, Generator::Framing(_)) {
            let head = ctx.head;
            let (centre, half_width) = (head.centre(), head.radius(Vec3::X));
            let underside = head.radius(Vec3::NEG_Y);
            let mut samples = SAMPLES.lock().unwrap();
            for p in group.clumps.iter().flat_map(|c| &c.rest) {
                let d = *p - centre;
                samples.push(Sample {
                    outside: d.length() - head.radius(d.normalize()),
                    y: d.y,
                    x: d.x.abs(),
                    half_width,
                    underside,
                });
            }
        }
        group
    }

    fn clump_count(&self, generator: &Generator) -> usize {
        Dispatch.clump_count(generator)
    }
}

struct FramingProbe;

impl PartKind for FramingProbe {
    type Params = HairParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "framing-probe",
            summary: "measures the framing locks",
            category: PartCategory::Hair,
            requires: &["head"],
        }
    }

    fn build(params: &HairParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        build_with(params, ctx, &Probe)
    }
}

omuzaki_char_api::register_part_kind!(FramingProbe);

fn part(kind: &str, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The real body and head with a probe hair part of `framing` alone.
fn character(template: &str, framing: &FramingGen) -> CharacterDef {
    let mut def = mock_character().unwrap();
    def.template = BodyTemplateId::new(template).unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", json!({})));
    let mut hair = HairParams { groups: std::collections::BTreeMap::default(), ..HairParams::default() };
    hair.groups.insert(
        GroupId::new("b-framing").unwrap(),
        HairGroup {
            enabled: true,
            mirror: false,
            dynamics: Dynamics::default(),
            generator: Generator::Framing(framing.clone()),
            lock: omuzaki_char_api::LockShape::default(),
        },
    );
    def.parts.insert(PartId::new("hair").unwrap(), part("framing-probe", serde_json::to_value(hair).unwrap()));
    def
}

fn samples_on(template: &str, framing: &FramingGen) -> Vec<Sample> {
    omuzaki_char::link();
    let _one_at_a_time = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    SAMPLES.lock().unwrap().clear();
    RegistryBuilder
        .build(
            &CharId::new("t").unwrap(),
            &character(template, framing),
            &BuildOptions::default(),
            &BuildServices::default(),
        )
        .unwrap_or_else(|e| panic!("{template} doesn't build: {e}"));
    std::mem::take(&mut *SAMPLES.lock().unwrap())
}

/// The closest the lock may sit to the head (a rounding below the surface) and the farthest, metres.
const NEAR: f32 = -0.002;
const FAR: f32 = 0.07;

/// Why `samples` do not frame the face, if they do not: a point inside the head, flaring out past it, or standing far off
/// it; or too little of the lock on the head above the jaw line.
fn flaw(samples: &[Sample]) -> Option<String> {
    if samples.is_empty() {
        return Some("the probe saw no locks".to_owned());
    }
    let mut above_jaw = 0;
    for s in samples {
        if s.outside < NEAR {
            return Some(format!("a lock point is {} m inside the head", -s.outside));
        }
        if s.x > s.half_width + FAR {
            return Some(format!("a lock point is {} m out, past the head's {}", s.x, s.half_width));
        }
        // Hugging the surface, jaw line and below: the tail goes round the jaw, not out into empty space.
        if s.outside > FAR {
            return Some(format!("a lock point at height {} stands {} m off the head", s.y, s.outside));
        }
        if s.y >= -s.underside {
            above_jaw += 1;
        }
    }
    (above_jaw <= samples.len() / 2).then(|| "most of the lock should be on the head".to_owned())
}

#[test]
fn the_default_locks_stay_close_to_the_head_down_to_the_jaw_line_on_every_template() {
    for template in ["soft-sd", "mini", "standard", "tall"] {
        let flaw = flaw(&samples_on(template, &FramingGen::default()));
        assert_eq!(flaw, None, "{template}");
    }
}

#[test]
fn the_first_defaults_did_not() {
    // The measure is not vacuous: the first defaults (metres for one head, a tail of free points beside and below it)
    // flare out and hang off the head on the narrower templates.
    let first = FramingGen {
        edge: vec![[0.12, 0.012], [0.0, 0.08], [-0.058, 0.136], [-0.12, 0.17], [-0.2, 0.192]],
        tail: vec![GuidePoint::Head([0.2, -0.18, 0.04]), GuidePoint::Head([0.22, -0.3, 0.02])],
        outline_radius: None,
        ..FramingGen::default()
    };
    assert!(flaw(&samples_on("standard", &first)).is_some());
    assert!(flaw(&samples_on("tall", &first)).is_some());
}

#[test]
fn the_default_locks_scale_with_the_head() {
    // The outline is metres drawn for one head: on a head half as wide the same file reaches half as far.
    let soft = samples_on("soft-sd", &FramingGen::default());
    let tall = samples_on("tall", &FramingGen::default());
    let reach = |s: &[Sample]| s.iter().map(|p| p.x / p.half_width).fold(0.0_f32, f32::max);
    assert!((reach(&soft) - reach(&tall)).abs() < 0.15, "{} against {}", reach(&soft), reach(&tall));
}

#[test]
fn a_file_without_an_outline_radius_is_taken_in_metres() {
    let old: FramingGen = serde_json::from_value(json!({ "side": "both", "count": 2 })).unwrap();
    assert_eq!(old.outline_radius, None);
    let current = serde_json::to_value(FramingGen::default()).unwrap();
    assert!(current.get("outline_radius").is_some());
}
