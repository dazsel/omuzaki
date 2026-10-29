//! Braided tails and the ahoge (#2411): on a synthetic head for the geometry, and on the real body and head of every
//! template for the body clearance, the bands and the spring chains.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // indices are small; f32 geometry

use omuzaki_char::hair::compile::{
    CompiledGroup, Dispatch, GroupGeometry, HairGenCtx, UnitStream, compile_groups_over, stream,
};
use omuzaki_char::hair::r#gen::curtain::BodyKeepOut;
use omuzaki_char::hair::r#gen::{ahoge, tail};
use omuzaki_char_api::{
    AhogeGen, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder,
    CharacterDef, Dynamics, Generator, GroupId, HairGroup, HairParams, HairRng, HeadSurface, PartCategory, PartDef,
    PartId, PartKind, PartKindId, PartMeta, PartOutput, Quality, RegistryBuilder, RigContribution, TailBraid, TailGen,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{Palette, Validate};
use serde_json::{Value, json};

/// A sphere of radius `R` about `CENTRE`.
#[derive(Debug)]
struct Ball;

const CENTRE: Vec3 = Vec3::new(0.0, 1.5, 0.0);
const R: f32 = 0.1;

impl HeadSurface for Ball {
    fn centre(&self) -> Vec3 {
        CENTRE
    }
    fn radius(&self, _dir: Vec3) -> f32 {
        R
    }
}

/// A stream that counts its draws.
#[derive(Default)]
struct Counting(usize);

impl UnitStream for Counting {
    fn next_unit(&mut self) -> f64 {
        self.0 += 1;
        0.5
    }
}

fn compile(generator: &Generator) -> (CompiledGroup, usize) {
    let (gid, dynamics, palette) = (GroupId::new("g").unwrap(), Dynamics::default(), Palette::default());
    let mut rng = Counting::default();
    let mut ctx = HairGenCtx::new(&Ball, &palette, Quality::Live, &gid, &dynamics, &mut rng);
    let group = Dispatch.compile(generator, &mut ctx);
    (group, rng.0)
}

use omuzaki_char::hair::compile::GroupGenerator as _;

fn outside(p: Vec3) -> f32 {
    (p - CENTRE).length() - R
}

// ------------------------------------------------------------------------------------------------ the braid

#[test]
fn a_braid_makes_its_strands_tuft_and_gather_and_two_bands_drawing_nothing() {
    for g in [tail::side_braids(), tail::back_braid()] {
        let generator = Generator::Tail(g.clone());
        let (group, draws) = compile(&generator);
        assert_eq!(draws, 0);
        let b = g.braid.as_ref().unwrap();
        assert_eq!(group.clumps.len(), usize::from(b.strands) + usize::from(b.tuft_locks) + usize::from(g.gather));
        assert_eq!(group.clumps.len(), Dispatch.clump_count(&generator));
        let GroupGeometry::Tied { ties } = &group.geometry else { panic!("a braid is tied") };
        assert_eq!(ties.len(), 2, "the top band and the end band");
        assert!(ties[1].centre.y < ties[0].centre.y - 0.2, "the end band hangs well below the top: {ties:?}");
        for p in group.clumps.iter().flat_map(|c| &c.rest) {
            assert!(p.is_finite() && outside(*p) >= 0.006, "{p:?} is {} off the head", outside(*p));
        }
    }
}

#[test]
fn the_strands_cross_over_one_another_down_the_braid() {
    // The back braid hangs down the back with its face to the back: the strands swing across in x.
    let g = tail::back_braid();
    let (group, _) = compile(&Generator::Tail(g.clone()));
    let crossings = g.braid.as_ref().unwrap().crossings;
    let (a, b) = (&group.clumps[0].rest, &group.clumps[1].rest);
    let changes = a
        .windows(2)
        .zip(b.windows(2))
        .filter(|(a, b)| (a[0].x - b[0].x).signum() != (a[1].x - b[1].x).signum())
        .count();
    // Two strands of a plait swap sides twice per crossing; the ends are gathered, so a few may be lost there.
    assert!(changes as f64 >= 1.5 * crossings, "strands 0 and 1 swap sides {changes} times for {crossings} crossings");
    // Without a braid the same tail is a loose bunch, whose locks never swap sides.
    let loose = TailGen { braid: None, ..g };
    let (group, _) = compile(&Generator::Tail(loose));
    let (a, b) = (&group.clumps[0].rest, &group.clumps[1].rest);
    let loose_changes = a
        .windows(2)
        .zip(b.windows(2))
        .filter(|(a, b)| (a[0].x - b[0].x).signum() != (a[1].x - b[1].x).signum())
        .count();
    assert!(loose_changes <= 2, "loose locks swapped {loose_changes} times");
}

#[test]
fn the_braid_is_as_wide_as_asked_and_narrows_toward_its_end() {
    let g = tail::back_braid();
    let b = g.braid.clone().unwrap();
    let (group, _) = compile(&Generator::Tail(g.clone()));
    let strands = &group.clumps[..usize::from(b.strands)];
    let spread_at = |i: usize| {
        let xs: Vec<f32> = strands.iter().map(|c| c.rest[i].x).collect();
        xs.iter().copied().fold(f32::MIN, f32::max) - xs.iter().copied().fold(f32::MAX, f32::min)
    };
    let n = strands[0].rest.len();
    let near_top = (n / 6..n / 3).map(spread_at).fold(0.0, f32::max);
    let near_end = (2 * n / 3..5 * n / 6).map(spread_at).fold(0.0, f32::max);
    // The ball is the reference head, so the width is as given.
    assert!(near_top <= b.width as f32 && near_top >= 0.3 * b.width as f32, "{near_top} for a {} braid", b.width);
    assert!(near_end < near_top, "the braid narrows: {near_end} after {near_top}");
}

#[test]
fn forward_brings_the_lock_in_front() {
    let tip_z = |forward: f64| {
        let g = TailGen { forward, ..tail::side_braids() };
        let (group, _) = compile(&Generator::Tail(g));
        group.clumps[0].rest.last().unwrap().z
    };
    assert!(tip_z(0.6) > tip_z(0.0) + 0.1, "{} vs {}", tip_z(0.6), tip_z(0.0));
    assert!(tip_z(-0.6) < tip_z(0.0) - 0.1);
}

#[test]
fn the_braid_checks_name_their_field() {
    let fails = |b: TailBraid| {
        let mut c = omuzaki_face_api::Check::new();
        let generator = Generator::Tail(TailGen { braid: Some(b), ..TailGen::default() });
        c.nested("g", &HairGroup { enabled: true, mirror: false, dynamics: Dynamics::default(), generator });
        c.finish().err().map(|e| e.to_string()).unwrap_or_default()
    };
    assert!(fails(TailBraid { strands: 6, ..TailBraid::default() }).contains("braid/strands"));
    assert!(fails(TailBraid { crossings: f64::NAN, ..TailBraid::default() }).contains("braid/crossings"));
    assert!(fails(TailBraid { tuft: 0.9, ..TailBraid::default() }).contains("braid/tuft"));
    assert_eq!(fails(TailBraid::default()), "");
}

// ------------------------------------------------------------------------------------------------ the ahoge

#[test]
fn the_ahoge_stands_up_and_hooks_over_drawing_nothing() {
    for g in [ahoge::hook(), ahoge::antenna(), AhogeGen::default()] {
        let generator = Generator::Ahoge(g.clone());
        let (group, draws) = compile(&generator);
        assert_eq!(draws, 0);
        assert_eq!(group.clumps.len(), usize::from(g.locks));
        assert_eq!(group.clumps.len(), Dispatch.clump_count(&generator));
        let first = &group.clumps[0].rest;
        let top = first.iter().map(|p| p.y).fold(f32::MIN, f32::max);
        let tip = *first.last().unwrap();
        assert!(top > CENTRE.y + R + 0.04, "it stands up: the highest point is {top}");
        assert!(tip.y < top - 0.01, "it hooks over: the tip at {} under a top of {top}", tip.y);
        assert!(tip.z > first[0].z, "it curls toward the front by default");
        assert!(first.iter().all(|p| outside(*p) >= 0.006));
    }
}

#[test]
fn more_curl_hooks_further_and_toward_turns_it() {
    let tip = |g: AhogeGen| *ahoge::lock(&g, &Ball, 0).last().unwrap();
    assert!(
        tip(AhogeGen { curl_deg: 260.0, ..AhogeGen::default() }).y
            < tip(AhogeGen { curl_deg: 120.0, ..AhogeGen::default() }).y
    );
    let left = tip(AhogeGen { toward_deg: 90.0, ..AhogeGen::default() });
    assert!(left.x > 0.03 && left.z.abs() < left.x, "toward 90 curls to the character's left: {left:?}");
}

#[test]
fn a_springy_ahoge_has_a_chain_of_its_own_and_a_stiff_one_has_none() {
    let chains = ahoge::chains(&ahoge::hook(), &Ball, "hair_h_a", false).unwrap();
    assert_eq!(chains.len(), 1);
    assert_eq!(chains[0].bones.len(), ahoge::CHAIN_BONES);
    assert_eq!(ahoge::chains(&ahoge::hook(), &Ball, "hair_h_a", true).unwrap().len(), 2, "a chain per side");
    let stiff = AhogeGen { spring: false, ..ahoge::hook() };
    assert!(ahoge::chains(&stiff, &Ball, "hair_h_a", false).unwrap().is_empty());
    let mut rig = RigContribution::default();
    tail::add_chains(chains, &[], &mut rig);
    assert_eq!(rig.chains.len(), 1);
}

// ------------------------------------------------------------------------------------------------ on the real templates

const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

/// Checks what a synthetic head cannot: compiled over the real body, every braid point is clear of the body.
struct BodyProbe;

impl PartKind for BodyProbe {
    type Params = HairParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-braid-probe",
            summary: "checks a braid's rest points against the body",
            category: PartCategory::Accessory,
            requires: &["head", "body"],
        }
    }

    fn build(params: &HairParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let fit = ctx.fit()?;
        let mut forked = Rng::new(1);
        let mut s = stream(HairRng::Forked, &mut forked).map_err(|e| ctx.fail(format!("seed {e}")))?;
        let entries = compile_groups_over(
            params,
            &Dispatch,
            head.as_ref(),
            Some(fit.as_ref()),
            ctx.palette,
            Quality::Live,
            s.as_mut(),
        )
        .map_err(|e| ctx.fail(e.to_string()))?;
        let body = BodyKeepOut::new(fit.as_ref());
        for p in entries.iter().flat_map(|e| &e.group.clumps).flat_map(|c| &c.rest) {
            let out = body.push_out(*p, 0.0);
            if (out - *p).length() > 1e-4 {
                return Err(ctx.fail(format!("rest point {p:?} is inside the body (pushed to {out:?})")));
            }
        }
        Ok(PartOutput::new())
    }
}

omuzaki_char_api::register_part_kind!(BodyProbe);

fn pid(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

fn part(kind: &str, layer: i32, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer, attach: None, params }
}

fn hair(groups: &[(&str, Generator, bool)]) -> HairParams {
    let mut params = HairParams::default();
    params.groups = groups
        .iter()
        .map(|(id, generator, mirror)| {
            let group = HairGroup {
                enabled: true,
                mirror: *mirror,
                dynamics: Dynamics::default(),
                generator: generator.clone(),
            };
            (GroupId::new(*id).unwrap(), group)
        })
        .collect();
    params
}

fn styled() -> HairParams {
    hair(&[
        ("braid", Generator::Tail(tail::side_braids()), true),
        ("back", Generator::Tail(tail::back_braid()), false),
        ("ahoge", Generator::Ahoge(ahoge::antenna()), false),
    ])
}

fn build(template: &str, hair: &HairParams) -> BuiltCharacter {
    let mut def: CharacterDef = mock_character().unwrap();
    def.template = template.parse().unwrap();
    def.parts.clear();
    def.parts.insert(pid("body"), part("body", 0, json!({})));
    def.parts.insert(pid("head"), part("head", 0, json!({})));
    def.parts.insert(pid("hair"), part("hair", 20, serde_json::to_value(hair).unwrap()));
    def.parts.insert(pid("probe"), part("test-braid-probe", 30, serde_json::to_value(hair).unwrap()));
    RegistryBuilder
        .build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap_or_else(|e| panic!("{template}: {e}"))
}

#[test]
fn braids_and_an_ahoge_build_on_every_template_clear_of_the_body_with_their_chains() {
    assert!(styled().validate().is_ok(), "{:?}", styled().validate());
    for template in TEMPLATES {
        let built = build(template, &styled());
        let warnings: Vec<_> = built.report.parts.iter().flat_map(|p| p.warnings.iter()).collect();
        assert!(warnings.is_empty(), "{template}: {warnings:?}");
        built.rig.springs.validate(&built.rig.skeleton).unwrap_or_else(|e| panic!("{template}: {e}"));
        let names: Vec<&str> = built.rig.springs.chains.iter().map(|c| c.name.as_str()).collect();
        for name in ["hair_hair_braid_l", "hair_hair_braid_r", "hair_hair_back", "hair_hair_ahoge"] {
            assert!(names.contains(&name), "{template}: no chain {name} in {names:?}");
        }
        // The bands ride their braid's chain: the tie mesh is skinned to more than the head.
        let ties = built.meshes.iter().find(|m| m.mesh.name == "hair-ties").expect("a tie mesh");
        assert!(ties.mesh.joint_bones.len() > 1, "{template}: the end bands follow the braid");
    }
}
