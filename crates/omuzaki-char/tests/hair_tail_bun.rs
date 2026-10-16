//! The tail (ponytail, twintail) and bun generators: on a synthetic head for the geometry, and on the real body and head
//! of every template for the presets, the anchors and the spring chains the hair part publishes.
//!
//! The prototype has no tail or bun, so there is nothing to compare against: the tests check what the design promises
//! (clearance, the gather at the tie, the droop, the mirror, the counts, determinism) and that each fails without it.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::float_cmp, clippy::cast_precision_loss)] // exact values that were copied, not computed; indices are small

use std::collections::BTreeMap;

use omuzaki_anim_api::{Collider, ColliderGroup, ColliderShape, HumanoidBone};
use omuzaki_char::hair::compile::{Dispatch, HairGenCtx, UnitStream, compile_groups, stream};
use omuzaki_char::hair::r#gen::{bun, tail};
use omuzaki_char_api::{
    Anchor, Attach, AttachOffset, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId,
    CharacterBuilder, CharacterDef, CollidersFact, Dynamics, Generator, GroupId, HairClump, HairGroup, HairParams,
    HairRng, HeadSurface, JointRef, PartCategory, PartDef, PartId, PartKind, PartKindId, PartMeta, PartOutput, Publishes,
    Quality, RegistryBuilder, TailGen, BunGen, Wave,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{Palette, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ------------------------------------------------------------------------------------------------ a synthetic head

/// A sphere of radius `R` about `CENTRE`.
#[derive(Debug)]
struct Ball;

const CENTRE: Vec3 = Vec3::new(0.0, 0.2, 0.0);
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
struct Counting {
    draws: usize,
}

impl UnitStream for Counting {
    fn next_unit(&mut self) -> f64 {
        self.draws += 1;
        0.5
    }
}

fn id(name: &str) -> GroupId {
    GroupId::new(name).unwrap()
}

fn compile_tail(g: &TailGen) -> (Vec<HairClump>, usize) {
    let (gid, dynamics, palette) = (id("tail"), Dynamics::default(), Palette::default());
    let mut rng = Counting::default();
    let mut ctx = HairGenCtx::new(&Ball, &palette, Quality::Live, &gid, &dynamics, &mut rng);
    (tail::compile(g, &mut ctx).clumps, rng.draws)
}

fn compile_bun(g: &BunGen) -> (Vec<HairClump>, usize) {
    let (gid, dynamics, palette) = (id("bun"), Dynamics::default(), Palette::default());
    let mut rng = Counting::default();
    let mut ctx = HairGenCtx::new(&Ball, &palette, Quality::Live, &gid, &dynamics, &mut rng);
    (bun::compile(g, &mut ctx).clumps, rng.draws)
}

/// How far `p` stands outside the ball.
fn outside(p: Vec3) -> f32 {
    (p - CENTRE).length() - R
}

const PRESETS: [&str; 2] = ["ponytail", "twintails"];

fn tail_preset(name: &str) -> TailGen {
    if name == "ponytail" { tail::ponytail() } else { tail::twintails() }
}

// ------------------------------------------------------------------------------------------------ the tail

#[test]
fn the_tail_makes_its_count_of_clumps_and_draws_nothing() {
    for count in [1_u8, 2, 9, 30] {
        let g = TailGen { count, ..TailGen::default() };
        let (clumps, draws) = compile_tail(&g);
        assert_eq!(clumps.len(), usize::from(count));
        assert_eq!(tail::clump_count(&g), usize::from(count));
        assert_eq!(draws, 0, "the tail is deterministic without the stream");
        assert!(clumps.iter().all(|c| c.rest.len() == tail::REST_POINTS));
    }
}

#[test]
fn every_rest_point_stands_clear_of_the_head_even_at_the_extremes() {
    let extreme = |root: [f64; 3], length: f64, droop: f64| TailGen { root, length, droop, ..TailGen::default() };
    let tails = [
        tail::ponytail(),
        tail::twintails(),
        extreme([0.0, 0.0, -0.05], 1.5, 1.0),
        extreme([180.0, 0.0, -0.05], 1.5, 1.0),
        extreme([90.0, 90.0, -0.05], 0.05, 0.0),
        extreme([150.0, 180.0, 0.0], 0.8, 1.0),
    ];
    for g in &tails {
        for clump in compile_tail(g).0 {
            for p in &clump.rest {
                assert!(outside(*p) >= 0.006, "{p:?} is {} off the head", outside(*p));
            }
        }
    }
}

#[test]
fn the_clumps_gather_at_the_tie_and_fan_out_after_it() {
    let g = TailGen { count: 12, ..tail::ponytail() };
    let (clumps, _) = compile_tail(&g);
    let clumps = &clumps[..usize::from(g.count)];
    let tie_at = (tail::TIE_LENGTH / g.length) as f32;
    let width_at = |t: f32| {
        let k = (t * (tail::REST_POINTS - 1) as f32).round() as usize;
        let points: Vec<Vec3> = clumps.iter().map(|c| c.rest[k]).collect();
        let mean = points.iter().copied().sum::<Vec3>() / points.len() as f32;
        points.iter().map(|p| (*p - mean).length()).fold(0.0, f32::max)
    };
    let tie = width_at(tie_at);
    assert!(tie <= 2.0 * g.tie_radius as f32 + 0.006, "the bunch at the tie is {tie} across");
    assert!(width_at(1.0) > 2.0 * tie, "the tips fan out: {} against the tie's {tie}", width_at(1.0));
}

#[test]
fn droop_pulls_the_tip_down() {
    let tip_y = |droop: f64| {
        let g = TailGen { droop, ..tail::ponytail() };
        compile_tail(&g).0[0].rest.last().unwrap().y
    };
    assert!(tip_y(1.0) < tip_y(0.0) - 0.1, "{} against {}", tip_y(1.0), tip_y(0.0));
}

#[test]
fn the_wave_sways_the_tail_sideways_and_only_when_asked() {
    let flat = TailGen { wave: Wave { amplitude: 0.0, ..Wave::default() }, count: 1, ..tail::ponytail() };
    let wavy = TailGen { wave: Wave { amplitude: 0.05, ..Wave::default() }, ..flat.clone() };
    let (a, b) = (compile_tail(&flat).0, compile_tail(&wavy).0);
    let apart = a[0].rest.iter().zip(&b[0].rest).map(|(p, q)| (*p - *q).length()).fold(0.0, f32::max);
    assert!(apart > 0.02, "the wave moved the tail by {apart}");
    assert_eq!(a[0].rest[0], b[0].rest[0], "the root stays put");
}

#[test]
fn non_finite_input_changes_neither_the_count_nor_the_finiteness() {
    let bad = TailGen {
        root: [f64::NAN, f64::INFINITY, f64::NAN],
        length: f64::NAN,
        droop: f64::INFINITY,
        spread_deg: f64::NAN,
        tie_radius: f64::NAN,
        wave: Wave { amplitude: f64::NAN, frequency: f64::INFINITY, phase: f64::NAN, curl: f64::NAN, growth: [f64::NAN; 2] },
        ..TailGen::default()
    };
    let (clumps, _) = compile_tail(&bad);
    assert_eq!(clumps.len(), tail::clump_count(&bad));
    assert!(clumps.iter().flat_map(|c| &c.rest).all(|p| p.is_finite() && outside(*p) >= 0.006));
    let ugly_bun = BunGen { centre: [f64::NAN; 3], radius: f64::NAN, wraps: f64::INFINITY, ..BunGen::default() };
    let (strands, _) = compile_bun(&ugly_bun);
    assert_eq!(strands.len(), bun::clump_count(&ugly_bun));
    assert!(strands.iter().flat_map(|c| &c.rest).all(|p| p.is_finite() && outside(*p) >= 0.006));
}

#[test]
fn compiling_twice_gives_the_same_bytes() {
    for name in PRESETS {
        let g = tail_preset(name);
        assert_eq!(compile_tail(&g).0, compile_tail(&g).0, "{name}");
    }
    assert_eq!(compile_bun(&BunGen::default()).0, compile_bun(&BunGen::default()).0);
}

/// `params` with one group `name`.
fn params_of(name: &str, generator: Generator, mirror: bool) -> HairParams {
    let group = HairGroup { enabled: true, mirror, dynamics: Dynamics::default(), generator };
    HairParams { groups: BTreeMap::from([(id(name), group)]), ..HairParams::default() }
}

#[test]
fn twintails_mirror_exactly() {
    let params = params_of("twin", Generator::Tail(tail::twintails()), true);
    let mut forked = Rng::new(1);
    let mut s = stream(HairRng::Forked, &mut forked).unwrap();
    let entries = compile_groups(&params, &Dispatch, &Ball, &Palette::default(), Quality::Live, s.as_mut()).unwrap();
    let clumps = &entries[0].group.clumps;
    let n = tail::clump_count(&tail::twintails());
    assert_eq!(clumps.len(), 2 * n);
    for (left, right) in clumps[..n].iter().zip(&clumps[n..]) {
        for (l, r) in left.rest.iter().zip(&right.rest) {
            assert_eq!(*r, Vec3::new(-l.x, l.y, l.z), "the right tail is the left one reflected in x = 0");
        }
    }
    assert!(clumps[..n].iter().flat_map(|c| &c.rest).all(|p| p.x > 0.0), "the authored tail is on the +x side");
}

// ------------------------------------------------------------------------------------------------ anchors and chains

#[test]
fn the_tail_root_anchor_sits_on_the_head_and_faces_along_the_tail() {
    let g = tail::ponytail();
    let anchors = tail::anchors(&g, &Ball, false);
    let [(name, anchor)] = anchors.as_slice() else { panic!("one anchor, got {}", anchors.len()) };
    assert_eq!(name, "tail_root");
    assert!((outside(anchor.position) - g.root[2] as f32).abs() < 1e-5, "{}", outside(anchor.position));
    let along = anchor.rotation * Vec3::Y;
    let outward = (anchor.position - CENTRE).normalize();
    assert!(along.dot(outward) > 0.999, "the anchor's +Y leaves the head: {along:?} against {outward:?}");
    // The first clump point is the tail's own start, right at the anchor's side of the head.
    assert!((compile_tail(&g).0[0].rest[0] - anchor.position).length() < tail::ROOT_RADIUS as f32 + 0.01);
}

#[test]
fn a_mirrored_tail_publishes_a_left_and_a_right_anchor() {
    let anchors = tail::anchors(&tail::twintails(), &Ball, true);
    let names: Vec<&str> = anchors.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["tail_root_l", "tail_root_r"]);
    let (l, r): (&Anchor, &Anchor) = (&anchors[0].1, &anchors[1].1);
    assert_eq!(r.position, Vec3::new(-l.position.x, l.position.y, l.position.z));
    let (up_l, up_r) = (l.rotation * Vec3::Y, r.rotation * Vec3::Y);
    assert!((up_r - Vec3::new(-up_l.x, up_l.y, up_l.z)).length() < 1e-5);
    assert!(l.position.x > 0.0);
}

#[test]
fn the_bun_publishes_its_centre_and_its_tie() {
    let g = bun::high_bun();
    let anchors = bun::anchors(&g, &Ball);
    let find = |name: &str| anchors.iter().find(|(n, _)| *n == name).map(|(_, a)| a.clone()).unwrap();
    let (centre, tie) = (find("bun_centre"), find("bun_tie"));
    assert!(((centre.position - tie.position).length() - g.radius as f32).abs() < 1e-5, "the tie is a radius from the centre");
    assert!((outside(tie.position) - g.centre[2] as f32).abs() < 1e-5);
    assert!(tie.rotation.dot(Quat::from_rotation_arc(Vec3::Y, (tie.position - CENTRE).normalize())).abs() > 0.9999);
}

#[test]
fn the_rig_chain_has_the_asked_bones_in_a_line_from_the_head() {
    use omuzaki_char_api::RigContribution;
    for bones in 4..=6_u8 {
        let g = TailGen { chain_bones: bones, ..tail::ponytail() };
        let mut rig = RigContribution::default();
        tail::rig(&g, &Ball, "hair_x_tail", false, &["head".to_owned()], &mut rig).unwrap();
        assert_eq!(rig.bones.len(), usize::from(bones));
        assert_eq!(rig.bones[0].parent, JointRef::HEAD);
        for pair in rig.bones.windows(2) {
            assert_eq!(pair[1].parent, JointRef::Named(pair[0].name.clone()), "each bone hangs from the one before");
            assert!(pair[1].rest.translation.y < pair[0].rest.translation.y + 0.2, "and the chain runs on");
        }
        let [chain] = rig.chains.as_slice() else { panic!("one chain") };
        assert_eq!(chain.joints.len(), usize::from(bones));
        assert_eq!(chain.collider_groups, ["head"]);
        assert!(chain.center.is_some());
        // The last bone is the tip of the tail (down at the length of the tail, within the droop).
        let tip = rig.bones.last().unwrap().rest.translation;
        assert!((tip - rig.bones[0].rest.translation).length() > 0.8 * g.length as f32);
    }
}

#[test]
fn a_mirrored_rig_has_a_chain_per_side_with_distinct_names() {
    use omuzaki_char_api::RigContribution;
    let mut rig = RigContribution::default();
    tail::rig(&tail::twintails(), &Ball, "hair_h_twin", true, &[], &mut rig).unwrap();
    assert_eq!(rig.chains.len(), 2);
    assert_eq!(rig.chains[0].name, "hair_h_twin_l");
    assert_eq!(rig.chains[1].name, "hair_h_twin_r");
    let (l, r) = (&rig.bones[0], &rig.bones[5]);
    assert_eq!(r.rest.translation.x, -l.rest.translation.x);
    assert_eq!(r.name.as_str(), "hair_h_twin_r_0");
    // The second chain's bones are numbered after the first's, so the provisional ids do not collide.
    assert_ne!(rig.chains[0].joints[0].bone, rig.chains[1].joints[0].bone);
}

#[test]
fn a_bad_prefix_is_an_error_not_a_panic() {
    use omuzaki_char_api::RigContribution;
    let mut rig = RigContribution::default();
    assert!(tail::rig(&tail::ponytail(), &Ball, "Not A Name", false, &[], &mut rig).is_err());
}

// ------------------------------------------------------------------------------------------------ the bun

#[test]
fn the_bun_winds_round_a_ball_that_rests_on_the_head() {
    for g in [bun::low_bun(), bun::high_bun(), BunGen::default()] {
        let (strands, draws) = compile_bun(&g);
        assert_eq!(strands.len(), bun::clump_count(&g));
        assert_eq!(draws, 0);
        let strands = &strands[..usize::from(g.strands)];
        let anchors = bun::anchors(&g, &Ball);
        let centre = anchors.iter().find(|(n, _)| *n == "bun_centre").unwrap().1.position;
        let reach = bun::LAYER_STEP as f32 * f32::from(bun::LAYERS);
        for p in strands.iter().flat_map(|c| &c.rest) {
            let d = (*p - centre).length();
            // On the ball, unless it was lifted off the head.
            assert!(d <= g.radius as f32 + reach + 1e-4, "{p:?} is {d} from the ball's centre");
            assert!(outside(*p) >= 0.006);
        }
        // The strands go round: they do not all lie in one half of the ball.
        let first = &strands[0].rest;
        let spread = first.iter().map(|p| (*p - centre).x).fold((f32::MAX, f32::MIN), |(lo, hi), x| (lo.min(x), hi.max(x)));
        assert!(spread.1 - spread.0 > 1.2 * g.radius as f32, "one strand spans {spread:?}");
    }
}

#[test]
fn more_wraps_wind_a_strand_further() {
    let length = |wraps: f64| {
        let g = BunGen { wraps, ..BunGen::default() };
        let rest = &compile_bun(&g).0[0].rest;
        rest.windows(2).map(|w| (w[1] - w[0]).length()).sum::<f32>()
    };
    assert!(length(3.0) > 1.5 * length(0.5));
}

// ------------------------------------------------------------------------------------------------ the checks

#[test]
fn the_checks_name_each_tail_and_bun_field() {
    let fails = |generator: Generator| {
        let mut c = omuzaki_face_api::Check::new();
        c.nested("g", &HairGroup { enabled: true, mirror: false, dynamics: Dynamics::default(), generator });
        c.finish().err().map(|e| e.to_string()).unwrap_or_default()
    };
    let tail_with = |f: &dyn Fn(&mut TailGen)| {
        let mut g = TailGen::default();
        f(&mut g);
        fails(Generator::Tail(g))
    };
    assert!(tail_with(&|g| g.chain_bones = 3).contains("chain_bones"));
    assert!(tail_with(&|g| g.chain_bones = 7).contains("chain_bones"));
    assert!(tail_with(&|g| g.tie_radius = f64::NAN).contains("tie_radius"));
    assert!(tail_with(&|g| g.spread_deg = 121.0).contains("spread_deg"));
    assert!(tail_with(&|g| g.root[1] = 400.0).contains("root/phi_deg"));
    assert!(tail_with(&|g| g.root[0] = f64::INFINITY).contains("root"));
    assert!(tail_with(&|g| g.width.start = 0.0).contains("width"));
    assert!(tail_with(&|_| ()).is_empty());
    let bun_with = |f: &dyn Fn(&mut BunGen)| {
        let mut g = BunGen::default();
        f(&mut g);
        fails(Generator::Bun(g))
    };
    assert!(bun_with(&|g| g.wraps = 7.0).contains("wraps"));
    assert!(bun_with(&|g| g.centre[1] = f64::NAN).contains("centre"));
    assert!(bun_with(&|g| g.thickness.base = 0.0).contains("thickness"));
    assert!(bun_with(&|_| ()).is_empty());
}

#[test]
fn the_presets_pass_the_checks() {
    for params in [
        params_of("tail", Generator::Tail(tail::ponytail()), false),
        params_of("tail", Generator::Tail(tail::twintails()), true),
        params_of("bun", Generator::Bun(bun::low_bun()), false),
        params_of("bun", Generator::Bun(bun::high_bun()), false),
    ] {
        assert!(params.validate().is_ok(), "{:?}", params.validate());
    }
}

// ------------------------------------------------------------------------------------------------ on the real templates

const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

/// No parameters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Nothing {}

impl omuzaki_face_api::Validate for Nothing {
    fn check(&self, _: &mut omuzaki_face_api::Check) {}
}

/// Stands in for the head collider, which no part publishes yet: a sphere on the head bone as big as the head.
struct HeadCollider;

impl PartKind for HeadCollider {
    type Params = Nothing;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-tail-head-collider",
            summary: "a stand-in collider",
            category: PartCategory::Accessory,
            requires: &["head"],
        }
    }

    fn build(_: &Nothing, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let joint = ctx.joint(&JointRef::HEAD).ok_or_else(|| ctx.fail("no head joint"))?;
        let sphere = ColliderShape::Sphere { offset: head.centre() - joint, radius: head.radius(Vec3::X) };
        let group = ColliderGroup {
            name: "head".to_owned(),
            colliders: vec![Collider { bone: omuzaki_char_api::humanoid_id(HumanoidBone::Head), shape: sphere }],
        };
        Ok(PartOutput::new().with_publishes(Publishes::default().with::<CollidersFact>(group)))
    }
}

omuzaki_char_api::register_part_kind!(HeadCollider);

/// Checks, on the real head and body, what a synthetic head cannot: the presets keep clear of the head and of the
/// shoulders at rest. It fails the build with a message naming the first rest point that does not.
struct ClearanceProbe;

/// How far the tails and buns must stay from a shoulder joint, metres: the radius of a shoulder collider.
const SHOULDER_CLEARANCE: f32 = 0.045;

impl PartKind for ClearanceProbe {
    type Params = HairParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-tail-probe",
            summary: "checks a hairstyle's rest points",
            category: PartCategory::Accessory,
            requires: &["head"],
        }
    }

    fn build(params: &HairParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let mut forked = Rng::new(1);
        let mut s = stream(HairRng::Forked, &mut forked).map_err(|e| ctx.fail(format!("seed {e}")))?;
        let entries = compile_groups(params, &Dispatch, head.as_ref(), ctx.palette, Quality::Live, s.as_mut())
            .map_err(|e| ctx.fail(e.to_string()))?;
        let shoulders: Vec<Vec3> = [HumanoidBone::LeftShoulder, HumanoidBone::RightShoulder]
            .into_iter()
            .filter_map(|b| ctx.joint(&JointRef::Humanoid(b)))
            .collect();
        for p in entries.iter().flat_map(|e| &e.group.clumps).flat_map(|c| &c.rest) {
            let off_head = (*p - head.centre()).length() - head.radius((*p - head.centre()).normalize());
            if off_head < 0.006 {
                return Err(ctx.fail(format!("rest point {p:?} is {off_head} off the head")));
            }
            if let Some(d) = shoulders.iter().map(|s| (*p - *s).length()).find(|d| *d < SHOULDER_CLEARANCE) {
                return Err(ctx.fail(format!("rest point {p:?} is {d} from a shoulder joint")));
            }
        }
        check_gather(params, &entries, head.as_ref()).map_err(|m| ctx.fail(m))?;
        Ok(PartOutput::new())
    }
}

omuzaki_char_api::register_part_kind!(ClearanceProbe);

/// Every gather lock stays within a few millimetres of the bare head (or the cap's shell over it) until it rises, and ends
/// inside its tie or bun.
fn check_gather(
    params: &HairParams,
    entries: &[omuzaki_char::hair::compile::CompiledEntry],
    head: &dyn HeadSurface,
) -> Result<(), String> {
    use omuzaki_char::hair::compile::GroupGeometry;
    let off = |p: Vec3| (p - head.centre()).length() - head.radius((p - head.centre()).normalize());
    for entry in entries {
        let Some(group) = params.groups.get(&entry.id) else { continue };
        let (own, gather) = match &group.generator {
            Generator::Tail(g) => (usize::from(g.count), usize::from(g.gather)),
            Generator::Bun(g) => (usize::from(g.strands), usize::from(g.gather)),
            _ => continue,
        };
        assert!(gather > 0, "the presets have gather locks");
        let side = own + gather;
        for (k, clump) in entry.group.clumps.iter().enumerate() {
            if k % side < own {
                continue;
            }
            let rise = (clump.rest.len() as f32 * 0.7) as usize;
            for p in &clump.rest[..rise] {
                if !(0.006..=0.028).contains(&off(*p)) {
                    return Err(format!("gather lock {k} of '{}' is {} off the head", entry.id, off(*p)));
                }
            }
            let end = *clump.rest.last().ok_or("an empty gather lock")?;
            let inside = match (&group.generator, &entry.group.geometry) {
                (Generator::Tail(_), GroupGeometry::Tied { ties }) => {
                    let tie = ties.get(k / side).ok_or("a tie per side")?;
                    (end - tie.centre).length() < tie.radius
                }
                (Generator::Bun(g), _) => {
                    let centre = bun::anchors(g, head).iter().find(|(n, _)| *n == "bun_centre").unwrap().1.position;
                    (end - centre).length() < g.radius as f32
                }
                _ => false,
            };
            if !inside {
                return Err(format!("gather lock {k} of '{}' ends at {end:?}, outside its tie or bun", entry.id));
            }
        }
    }
    Ok(())
}

fn pid(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

fn part(kind: &str, layer: i32, attach: Option<Attach>, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer, attach, params }
}

/// The head parameters that seat the head on `template`'s neck (see `accessory_rigid.rs`).
fn head_params(template: &str) -> Value {
    let body = omuzaki_char_api::BodyParams::default();
    let layout =
        omuzaki_char::template::registry::find(template).unwrap().layout(&body.proportions, &body.shape, &body.hand);
    let soft_sd =
        omuzaki_char::template::registry::find("soft-sd").unwrap().layout(&body.proportions, &body.shape, &body.hand);
    let rise = layout.head_base().unwrap().y - soft_sd.head_base().unwrap().y;
    let default = omuzaki_char_api::HeadParams::default().cranium.centre;
    json!({ "cranium": { "centre": [default[0], default[1] + rise, default[2]] } })
}

/// The four presets as hair parameters: `(name, params)`.
fn presets() -> Vec<(&'static str, HairParams)> {
    vec![
        ("ponytail", params_of("pony", Generator::Tail(tail::ponytail()), false)),
        ("twintails", params_of("twin", Generator::Tail(tail::twintails()), true)),
        ("low-bun", params_of("bun", Generator::Bun(bun::low_bun()), false)),
        ("high-bun", params_of("bun", Generator::Bun(bun::high_bun()), false)),
    ]
}

fn character(template: &str, hair: &HairParams, extra: Option<(&str, Attach)>) -> CharacterDef {
    let mut def = mock_character().unwrap();
    def.template = template.parse().unwrap();
    def.parts.clear();
    def.parts.insert(pid("body"), part("body", 0, None, json!({})));
    def.parts.insert(pid("head"), part("head", 0, None, head_params(template)));
    def.parts.insert(pid("collider"), part("test-tail-head-collider", 10, None, json!({})));
    def.parts.insert(pid("hair"), part("hair", 20, None, serde_json::to_value(hair).unwrap()));
    def.parts.insert(pid("probe"), part("test-tail-probe", 30, None, serde_json::to_value(hair).unwrap()));
    if let Some((kind, attach)) = extra {
        def.parts.insert(pid("charm"), part(kind, 50, Some(attach), json!({})));
    }
    def
}

fn build(def: &CharacterDef) -> Result<BuiltCharacter, BuildError> {
    RegistryBuilder.build(&CharId::new("t").unwrap(), def, &BuildOptions::default(), &BuildServices::default())
}

fn triangles(built: &BuiltCharacter, part: &str) -> usize {
    built.report.parts.iter().filter(|p| p.part.as_str() == part).map(|p| p.triangles).sum()
}

/// The most triangles one tail or bun hairstyle may add: a third of a Good-rank avatar's 32,000.
const TRIANGLE_BUDGET: usize = 10_000;

#[test]
fn every_preset_builds_on_every_template_clear_of_head_and_shoulders() {
    for template in TEMPLATES {
        for (name, hair) in presets() {
            let built = build(&character(template, &hair, None)).unwrap_or_else(|e| panic!("{name} on {template}: {e}"));
            let count = triangles(&built, "hair");
            assert!(count > 0 && count <= TRIANGLE_BUDGET, "{name} on {template}: {count} triangles");
            let warnings: Vec<_> = built.report.parts.iter().flat_map(|p| p.warnings.iter()).collect();
            assert!(warnings.is_empty(), "{name} on {template}: {warnings:?}");
            println!("{template} {name}: {count} triangles");
        }
    }
}

#[test]
fn the_hair_part_publishes_the_anchors_and_the_spring_chains() {
    for template in TEMPLATES {
        let built = build(&character(template, &presets()[1].1, None)).unwrap();
        built.rig.springs.validate(&built.rig.skeleton).unwrap_or_else(|e| panic!("{template}: {e}"));
        let names: Vec<&str> = built.rig.springs.chains.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["hair_hair_twin_l", "hair_hair_twin_r"]);
        assert!(built.rig.springs.chains.iter().all(|c| c.collider_groups == ["head"] && c.joints.len() == 5));
        let anchor = |name: &str| built.anchors.get(&(pid("hair"), name.parse().unwrap())).cloned();
        let (l, r) = (anchor("twin_tail_root_l").unwrap(), anchor("twin_tail_root_r").unwrap());
        assert!((l.position.x + r.position.x).abs() < 1e-6 && l.position.y == r.position.y);
        assert!(l.position.x > 0.0 && r.position.x < 0.0);
        let buns = build(&character(template, &presets()[3].1, None)).unwrap();
        assert!(buns.anchors.contains_key(&(pid("hair"), "bun_bun_centre".parse().unwrap())));
        assert!(buns.anchors.contains_key(&(pid("hair"), "bun_bun_tie".parse().unwrap())));
        assert!(buns.rig.springs.chains.is_empty(), "a bun is rigid: no chain");
    }
}

#[test]
fn another_part_attaches_to_a_published_tail_anchor() {
    let hair = &presets()[0].1;
    let attach = Attach::Anchor {
        part: pid("hair"),
        anchor: "pony_tail_root".parse().unwrap(),
        offset: AttachOffset::default(),
    };
    let built = build(&character("standard", hair, Some(("charm", attach)))).unwrap();
    let root = built.anchors[&(pid("hair"), "pony_tail_root".parse().unwrap())].position;
    let charm: Vec<Vec3> = built.meshes.iter().filter(|m| m.part.as_str() == "charm").flat_map(|m| m.mesh.geometry.positions.clone()).collect();
    assert!(!charm.is_empty());
    let middle = charm.iter().copied().sum::<Vec3>() / charm.len() as f32;
    assert!((middle - root).length() < 0.15, "the charm hangs at {middle:?}, the anchor is at {root:?}");
}

#[test]
fn a_disabled_group_publishes_nothing() {
    let mut hair = presets()[1].1.clone();
    for g in hair.groups.values_mut() {
        g.enabled = false;
    }
    let built = build(&character("standard", &hair, None)).unwrap();
    assert!(built.rig.springs.chains.is_empty());
    assert!(!built.anchors.keys().any(|(p, _)| p.as_str() == "hair"));
}

/// Renders every preset on `soft-sd` and `standard` (front, side, back) through the mock toon renderer when
/// `OMUZAKI_HAIR_TAIL_PPM=<directory>` is set, as binary PPM files `<template>-<preset>-<view>.ppm`. Taste is judged on
/// the pictures, not on numbers.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_presets_when_asked() {
    use omuzaki_core::colour::Colour;
    use omuzaki_core::glam::{Mat4, UVec2};
    use omuzaki_render_api::{
        AlphaEncoding, Background, Camera, Instance, Light, Post, Quality, Renderer, RendererExt, Scene, Tonemap,
    };
    use omuzaki_render_mock::MockRenderer;

    let Some(directory) = std::env::var_os("OMUZAKI_HAIR_TAIL_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    for template in ["soft-sd", "standard"] {
        for (name, hair) in presets() {
            let built = build(&character(template, &hair, None)).unwrap();
            let top = built.anchors[&(pid("head"), "top".parse().unwrap())].position;
            let centre = top - Vec3::Y * 0.3;
            for (view, azimuth_deg) in [("front", 0.0_f32), ("side", 90.0), ("back", 180.0)] {
                let mut renderer = MockRenderer::new();
                let eye = centre + Mat4::from_rotation_y(azimuth_deg.to_radians()).transform_vector3(Vec3::new(0.0, 0.05, 2.1));
                let mut scene = Scene::new(Camera::look_at(eye, centre, Vec3::Y, Camera::default().projection));
                scene.background = Background::Solid { colour: Colour::hex(0x001e_1c24) };
                scene.lights = vec![
                    Light::Hemisphere { sky: Colour::hex(0x00f1_eef8), ground: Colour::hex(0x004a_3e36), intensity: 0.75 },
                    Light::Directional {
                        to_light: Vec3::new(1.6, 3.0, 2.4),
                        colour: Colour::hex(0x00ff_f6ec),
                        intensity: 1.7,
                        casts_shadow: false,
                    },
                ];
                scene.post = Post { exposure: 1.0, tonemap: Tonemap::None, ..Post::default() };
                for built_mesh in &built.meshes {
                    let mesh = renderer.create_mesh(&built_mesh.mesh.geometry).unwrap();
                    let mut spec = built.materials[&built_mesh.mesh.material].clone();
                    spec.overlays.clear();
                    let material = renderer.create_material(&spec.to_material(&|_| None, &|_| None).unwrap()).unwrap();
                    scene.instances.push(Instance::new(mesh, material, Mat4::IDENTITY));
                }
                let quality = Quality { samples: 4, ..Quality::offline() };
                let image = renderer.render_image(UVec2::splat(512), AlphaEncoding::Straight, &scene, &quality).unwrap();
                let mut ppm = format!("P6\n{} {}\n255\n", image.size.x, image.size.y).into_bytes();
                for y in 0..image.size.y {
                    for x in 0..image.size.x {
                        let [r, g, b, _] = image.pixel(x, y).unwrap();
                        ppm.extend([r, g, b]);
                    }
                }
                std::fs::write(directory.join(format!("{template}-{name}-{view}.ppm")), ppm).unwrap();
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ the look of a tail

#[test]
fn the_tail_is_one_mass_of_broad_overlapping_locks_at_the_tie() {
    let g = tail::ponytail();
    let (clumps, _) = compile_tail(&g);
    let clumps = &clumps[..usize::from(g.count)];
    assert!((7..=9).contains(&clumps.len()), "{} locks", clumps.len());
    assert!(clumps.iter().all(|c| (0.025..=0.035).contains(&c.width.start) && c.width.mid >= c.width.start));
    // Near the tie every lock's centre is well within half a lock width of the others', so no background shows.
    let k = 2;
    let mean = clumps.iter().map(|c| c.rest[k]).sum::<Vec3>() / clumps.len() as f32;
    for c in clumps {
        assert!((c.rest[k] - mean).length() < 0.5 * c.width.start as f32, "a lock stands {} off the bunch", (c.rest[k] - mean).length());
    }
    // And the tips are pointed.
    assert!(clumps[0].width.at(1.0) < 0.3 * clumps[0].width.at(0.5));
}

#[test]
fn the_tie_band_sits_fifteen_to_twenty_millimetres_off_the_scalp_and_hides_the_gather() {
    use omuzaki_char::hair::compile::GroupGeometry;
    for name in PRESETS {
        let g = tail_preset(name);
        let (gid, dynamics, palette) = (id("tail"), Dynamics::default(), Palette::default());
        let mut rng = Counting::default();
        let mut ctx = HairGenCtx::new(&Ball, &palette, Quality::Live, &gid, &dynamics, &mut rng);
        let group = tail::compile(&g, &mut ctx);
        let GroupGeometry::Tied { ties } = &group.geometry else { panic!("a tail is tied") };
        let [tie] = ties.as_slice() else { panic!("one tie per tail") };
        assert!((0.015..=0.021).contains(&outside(tie.centre)), "{name}: the tie is {} off the scalp", outside(tie.centre));
        // The ring clears the bunch plus half a lock, so the locks start inside it.
        assert!(tie.radius > g.tie_radius as f32 + 0.3 * g.width.start as f32);
        assert!(tie.tube > 0.004);
    }
}

#[test]
fn the_tail_bends_to_hang_within_a_few_centimetres_of_the_tie() {
    for name in PRESETS {
        let g = tail_preset(name);
        let rest = &compile_tail(&g).0[0].rest;
        // Arc length along the clump at each point.
        let mut arc = 0.0;
        let mut direction_after = None;
        for w in rest.windows(2) {
            let step = (w[1] - w[0]).length();
            if arc >= 0.060 && direction_after.is_none() {
                direction_after = Some((w[1] - w[0]) / step);
            }
            arc += step;
        }
        let d = direction_after.unwrap();
        assert!(d.y < -0.75, "{name}: 6 cm along, the tail still runs {d:?}");
    }
}

#[test]
fn a_mirrored_group_has_a_tie_per_tail() {
    use omuzaki_char::hair::compile::GroupGeometry;
    let params = params_of("twin", Generator::Tail(tail::twintails()), true);
    let mut forked = Rng::new(1);
    let mut s = stream(HairRng::Forked, &mut forked).unwrap();
    let entries = compile_groups(&params, &Dispatch, &Ball, &Palette::default(), Quality::Live, s.as_mut()).unwrap();
    let GroupGeometry::Tied { ties } = &entries[0].group.geometry else { panic!("tied") };
    assert_eq!(ties.len(), 2);
    assert_eq!(ties[1].centre.x, -ties[0].centre.x);
    assert_eq!(ties[1].axis.x, -ties[0].axis.x);
}

#[test]
fn the_tie_band_is_a_closed_outward_torus() {
    use omuzaki_char::hair::tie::{self, Tie};
    let t = Tie { centre: Vec3::new(0.1, 0.3, 0.0), axis: Vec3::X, radius: 0.02, tube: 0.006 };
    let m = tie::mesh(&[t]).unwrap();
    m.validate().unwrap();
    assert!(tie::mesh(&[]).is_none());
    let mut outward = 0;
    for tri in m.indices.chunks(3) {
        let p = |i: u32| m.positions[i as usize];
        let n = (p(tri[1]) - p(tri[0])).cross(p(tri[2]) - p(tri[0]));
        let vertex_normal = m.normals[tri[0] as usize];
        if n.dot(vertex_normal) > 0.0 {
            outward += 1;
        }
    }
    assert!(outward * 10 > m.indices.len() / 3 * 9, "{outward} of {} triangles face out", m.indices.len() / 3);
    // Every vertex is one tube radius from the ring's centre line, about the axis X.
    for p in &m.positions {
        let d = *p - t.centre;
        let across = Vec3::new(0.0, d.y, d.z).length();
        let ring = ((across - t.radius).powi(2) + d.x.powi(2)).sqrt();
        assert!((ring - t.tube).abs() < 1e-5);
    }
}

// ------------------------------------------------------------------------------------------------ skinning to the chain

/// The mesh of the hair part's `name`, and the bone ids its joints name.
fn hair_mesh<'a>(built: &'a BuiltCharacter, name: &str) -> (&'a omuzaki_char_api::PartMesh, Vec<usize>) {
    let m = &built.meshes.iter().find(|m| m.mesh.name == name).unwrap().mesh;
    let bones = m.joint_bones.iter().map(|j| built.rig.bone(j).unwrap().index()).collect();
    (m, bones)
}

/// Where the mesh's vertices are when the skeleton is posed with `pose`.
fn skinned(built: &BuiltCharacter, mesh: &omuzaki_char_api::PartMesh, bones: &[usize], pose: &omuzaki_anim_api::Pose) -> Vec<Vec3> {
    let mut world = omuzaki_anim_api::Pose::IDENTITY;
    built.rig.skeleton.world_pose(pose, &mut world);
    let inverse = built.rig.inverse_bind();
    let matrix = |slot: u16| {
        let bone = bones[usize::from(slot)];
        world.local[bone].to_mat4() * inverse[bone]
    };
    let g = &mesh.geometry;
    (0..g.positions.len())
        .map(|v| {
            let mut p = Vec3::ZERO;
            for (j, w) in g.joints[v].iter().zip(g.weights[v]) {
                if w > 0.0 {
                    p += w * matrix(*j).transform_point3(g.positions[v]);
                }
            }
            p
        })
        .collect()
}

#[test]
fn the_locks_follow_their_chain_bones_and_the_rest_pose_is_unchanged() {
    for template in ["soft-sd", "standard"] {
        let built = build(&character(template, &presets()[1].1, None)).unwrap();
        let (mesh, bones) = hair_mesh(&built, "hair-static");
        mesh.validate().unwrap();
        // The head, then each side's five chain bones.
        assert_eq!(mesh.joint_bones.len(), 1 + 2 * 5, "{template}");
        // Rest: skinning reproduces the mesh.
        let rest = built.rig.skeleton.rest_pose();
        let at_rest = skinned(&built, mesh, &bones, &rest);
        let worst = at_rest.iter().zip(&mesh.geometry.positions).map(|(a, b)| (*a - *b).length()).fold(0.0, f32::max);
        assert!(worst < 1e-4, "{template}: the rest pose moves a vertex by {worst}");
        // Pose the third bone of the left chain: the locks it carries move, the root of the tail does not.
        let bone = built.rig.bone(&mesh.joint_bones[1 + 2]).unwrap().index();
        let mut posed = rest.clone();
        posed.local[bone].rotation = Quat::from_rotation_z(0.6) * posed.local[bone].rotation;
        let moved = skinned(&built, mesh, &bones, &posed);
        let dist = |v: usize| (moved[v] - mesh.geometry.positions[v]).length();
        let tip_moved = (0..moved.len()).filter(|v| dist(*v) > 0.02).count();
        assert!(tip_moved > 0, "{template}: posing a chain bone moves no vertex");
        let g = &mesh.geometry;
        let head_only = (0..moved.len()).filter(|v| g.weights[*v][0] > 0.999).count();
        assert!(head_only > 0, "{template}: the roots of the locks are the head's");
        assert!((0..moved.len()).filter(|v| g.weights[*v][0] > 0.999).all(|v| dist(v) < 1e-5));
        // The right chain's locks do not move: no vertex they carry has weight on the left chain's bones.
        let right = (0..moved.len()).filter(|v| g.positions[*v].x < -0.01).all(|v| dist(v) < 1e-5);
        assert!(right, "{template}: the right tail moved with the left chain");
    }
}

/// The hair part's own output for `def`.
fn hair_output(def: &CharacterDef) -> omuzaki_char_api::PartOutput {
    use omuzaki_char_api::{Assembler, build_part, plan};
    let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
    let mut assembler = Assembler::new(&id, def, &options);
    let mut result = None;
    for part in plan(def).unwrap() {
        let out = build_part(def, &part, &options, &services, assembler.published()).unwrap();
        if part.as_str() == "hair" {
            result = Some(build_part(def, &part, &options, &services, assembler.published()).unwrap());
        }
        assembler.add(&part, out).unwrap();
    }
    result.unwrap()
}

#[test]
fn the_tie_mesh_has_its_own_material_and_recolours_alone() {
    use omuzaki_char_api::{Recoloured, recolour_part};
    use omuzaki_face_api::ColourRef;
    let mut hair = presets()[0].1.clone();
    let before = character("standard", &hair, None);
    hair.shading.tie = ColourRef::rgb(1, 2, 3);
    let after = character("standard", &hair, None);
    let mut out = hair_output(&before);
    let ties = out.meshes.iter().find(|m| m.name == "hair-ties").unwrap();
    assert_eq!(ties.material.as_str(), "hair-tie");
    assert_ne!(ties.material, out.meshes.iter().find(|m| m.name == "hair-static").unwrap().material);
    let rebuilt = hair_output(&after);
    let key = |o: &omuzaki_char_api::PartOutput| format!("{:?}\n{:?}", o.meshes, o.materials);
    assert_ne!(key(&out), key(&rebuilt), "the tie colour really changes the output");
    let done = recolour_part(&after, &PartId::new("hair").unwrap(), &BuildServices::default(), &mut out);
    assert_eq!(done, Ok(Recoloured::Done));
    assert_eq!(key(&out), key(&rebuilt));
}

#[test]
fn gather_locks_are_counted_finite_and_can_be_turned_off() {
    let g = tail::ponytail();
    assert_eq!(compile_tail(&g).0.len(), usize::from(g.count + g.gather));
    let off = TailGen { gather: 0, ..g.clone() };
    assert_eq!(compile_tail(&off).0.len(), usize::from(off.count));
    assert_eq!(tail::clump_count(&off), usize::from(off.count));
    let bad = TailGen { root: [f64::NAN; 3], gather: 4, ..g };
    let (clumps, _) = compile_tail(&bad);
    assert_eq!(clumps.len(), tail::clump_count(&bad));
    assert!(clumps.iter().flat_map(|c| &c.rest).all(|p| p.is_finite()));
    let bun = bun::high_bun();
    assert_eq!(compile_bun(&bun).0.len(), usize::from(bun.strands + bun.gather));
    let no_gather = BunGen { gather: 0, ..bun };
    assert_eq!(compile_bun(&no_gather).0.len(), usize::from(no_gather.strands));
}

#[test]
fn the_gather_locks_are_broad_and_start_on_the_back_hairline() {
    let g = tail::ponytail();
    let locks = &compile_tail(&g).0[usize::from(g.count)..];
    assert!(locks.iter().all(|c| c.width.mid >= 0.045 && c.width.start < c.width.mid), "broad over the head, pointed at the nape");
    // They start low and spread across the back, and all end at one point, the tie.
    let starts: Vec<Vec3> = locks.iter().map(|c| c.rest[0]).collect();
    let (lo, hi) = starts.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.x), hi.max(p.x)));
    assert!(hi - lo > 0.1, "the gather starts {} wide", hi - lo);
    assert!(starts.iter().all(|p| p.y < CENTRE.y), "the gather starts below the middle of the head");
    let ends: Vec<Vec3> = locks.iter().map(|c| *c.rest.last().unwrap()).collect();
    assert!(ends.iter().all(|e| *e == ends[0]));
}
