//! The hair's shared volume and flow (#2478): one parting, one crown whorl and one fullness that the scalp shell, the
//! part sweeps and the fringe all follow. On a synthetic head, each consumer is checked against the field; on each
//! template's own head, the shell and a pair of part sweeps share one parting and whorl; on the real kit, a full volume
//! builds on every template. Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)] // fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::sync::Arc;

use omuzaki_char::hair::compile::{CompiledEntry, Dispatch, GroupGeometry, UnitStream, compile_groups};
use omuzaki_char::hair::r#gen::{cap, part_sweep};
use omuzaki_char::hair::volume::{self, Field};
use omuzaki_char_api::{
    Assembler, BodyParams, BodyShape, BodyTemplateId, BuildOptions, BuildServices, CapGen, CharId, CharacterDef,
    Dynamics, Generator, GroupId, HairGroup, HairParams, HairParting, HairVolume, HairWhorl, HeadParams, HeadSurface,
    LockShape, PartDef, PartId, PartKindId, PartSweepGen, Quality, build_part, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::Palette;
use serde_json::json;

/// An ellipsoid head: centre `(0, 0.2, 0)`, semi-axes `(0.08, 0.11, 0.095)`.
#[derive(Debug)]
struct Egg;

impl HeadSurface for Egg {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 0.2, 0.0)
    }
    fn radius(&self, dir: Vec3) -> f32 {
        1.0 / (dir / Vec3::new(0.08, 0.11, 0.095)).length()
    }
}

/// A stream that always draws one half.
struct Half;

impl UnitStream for Half {
    fn next_unit(&mut self) -> f64 {
        0.5
    }
}

fn parted(side_deg: f64) -> HairVolume {
    HairVolume { part: Some(HairParting { side_deg, ..HairParting::default() }), ..HairVolume::default() }
}

/// The unit direction from the head's centre to `p`.
fn dir(p: Vec3) -> DVec3 {
    (p - Egg.centre()).normalize().as_dvec3()
}

/// The default style with `volume`, compiled on the egg.
fn compiled(volume: Option<HairVolume>) -> Vec<CompiledEntry> {
    let params = HairParams { volume, ..HairParams::default() };
    compile_groups(&params, &Dispatch, &Egg, &Palette::default(), Quality::Live, &mut Half).unwrap()
}

// ------------------------------------------------------------------------------------------------ part sweeps

#[test]
fn part_sweeps_start_on_the_shared_parting_each_on_its_own_side() {
    let g = PartSweepGen::default();
    for side_deg in [-30.0, 0.0, 25.0] {
        let field = Field::new(&parted(side_deg));
        let mut locks = Vec::new();
        part_sweep::rebuild_on(&g, &Egg, Some(&field), &mut locks);
        assert_eq!(locks.len(), part_sweep::clump_count(&g));
        let across = field.part_normal().unwrap();
        for (n, lock) in locks.iter().enumerate() {
            let root = dir(lock[0]);
            let comb = field.comb(root);
            // `part_spread` (0.02) to its side of the line, and no further.
            assert!(comb.from_part && comb.distance < 0.03, "part {side_deg}, lock {n}: {comb:?}");
            let side = if n < usize::from(g.count) { 1.0 } else { -1.0 };
            assert!(root.dot(across) * side > 0.0, "part {side_deg}, lock {n} roots on the wrong side");
        }
    }
    // Without a parting the sweeps keep their own part line, as before.
    let (mut own, mut none) = (Vec::new(), Vec::new());
    part_sweep::rebuild(&g, &Egg, &mut own);
    part_sweep::rebuild_on(&g, &Egg, Some(&Field::new(&HairVolume::default())), &mut none);
    assert_eq!(own, none);
}

// ------------------------------------------------------------------------------------------------ the scalp shell

/// The shell's `hair-uv.y` (how far down its strands each vertex lies) and each vertex's direction.
fn shell_runs(field: Option<&Field>) -> Vec<(DVec3, f32)> {
    let mut shell = MeshData::default();
    cap::fill_shell_combed(&CapGen::default(), &Egg, field, &mut shell);
    let uv = shell.extra.iter().find(|a| a.name.as_str() == "hair-uv").unwrap();
    shell.positions.iter().zip(uv.data.chunks(2)).map(|(p, uv)| (dir(*p), uv[1])).collect()
}

#[test]
fn the_scalp_shell_is_combed_away_from_the_parting_and_the_whorl() {
    let reach = CapGen::default().hairline_back_deg.to_radians();
    for volume in [parted(20.0), parted(-35.0), HairVolume::default()] {
        let field = Field::new(&volume);
        let runs = shell_runs(Some(&field));
        for (d, along) in &runs {
            let comb = field.comb(*d);
            // `hair-uv.y` is the distance from the nearer source over the shell's reach (the lift that keeps the
            // shell clear of the head moves a vertex a little off its grid direction).
            let expected = (comb.distance / reach).min(1.0) as f32;
            assert!((along - expected).abs() < 0.03, "{volume:?} at {d}: {along} against {expected}");
        }
        // The strands start at the source: the shell's least `hair-uv.y` sits on it.
        let (start, _) = runs.iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
        assert!(field.comb(*start).distance < 0.1, "{volume:?}: the strands start at {start}");
    }
    // Without a field the shell runs down from the top, as the prototype's does.
    let plain = shell_runs(None);
    let (top, _) = plain.iter().min_by(|a, b| a.1.total_cmp(&b.1)).unwrap();
    assert!(top.y > 0.99, "{top}");
}

#[test]
fn a_parted_shell_runs_its_strands_along_the_parting() {
    // Two vertices just off the parting, one near its front and one near its back, lie on different strands.
    let field = Field::new(&parted(15.0));
    let mut shell = MeshData::default();
    cap::fill_shell_combed(&CapGen::default(), &Egg, Some(&field), &mut shell);
    let near = |s: f64| {
        let target = field.part(s).unwrap();
        let (k, _) = shell
            .positions
            .iter()
            .enumerate()
            .max_by(|a, b| dir(*a.1).dot(target).total_cmp(&dir(*b.1).dot(target)))
            .unwrap();
        shell.uvs[k].x
    };
    assert!(near(0.8) - near(0.2) > 1.0, "{} then {}", near(0.2), near(0.8));
}

// ------------------------------------------------------------------------------------------------ the fringe

/// The mean x of the fringe's tips against its roots: negative sweeps toward the character's right.
fn fringe_sweep(volume: Option<HairVolume>) -> f32 {
    let entries = compiled(volume);
    let bangs = entries.iter().find(|e| e.id.as_str() == "b-bangs").unwrap();
    let clumps = &bangs.group.clumps;
    clumps.iter().map(|c| c.rest[c.rest.len() - 1].x - c.rest[0].x).sum::<f32>() / clumps.len() as f32
}

#[test]
fn the_fringe_sweeps_away_from_the_parting() {
    let (left, centre, right) =
        (fringe_sweep(Some(parted(25.0))), fringe_sweep(Some(parted(0.0))), fringe_sweep(Some(parted(-25.0))));
    // A parting on the character's left (+x) sweeps the fringe toward its right (-x), and the other way about.
    assert!(left < centre - 0.002 && right > centre + 0.002, "{left}, {centre}, {right}");
    // A centre parting leaves the fringe's own sweep alone.
    assert!((centre - fringe_sweep(None)).abs() < 1e-6);
}

// ------------------------------------------------------------------------------------------------ fullness

#[test]
fn fullness_scales_how_far_the_hair_stands_off_the_head_and_leaves_the_fall_alone() {
    let volume = HairVolume { fullness: 1.6, crown: 1.25, back: 0.8, ..HairVolume::default() };
    let field = Field::new(&volume);
    let before = compiled(None);
    let mut after = before.clone();
    volume::swell(&mut after, &field, &Egg, &volume::Accents::default());
    let height = |p: Vec3| (p - Egg.centre()).length() - Egg.radius((p - Egg.centre()).normalize());
    let (mut over, mut under) = (0, 0);
    for (b, a) in before.iter().zip(&after) {
        for (cb, ca) in b.group.clumps.iter().zip(&a.group.clumps) {
            for (pb, pa) in cb.rest.iter().zip(&ca.rest) {
                let (d, h) = (dir(*pb), height(*pb));
                assert!((dir(*pa) - d).length() < 1e-4, "{}: a point moves only along its own direction", b.id);
                if h <= 1e-4 {
                    continue;
                }
                let polar = d.y.clamp(-1.0, 1.0).acos();
                if polar < 1.5 {
                    let expected = h * field.fullness(d) as f32;
                    assert!(
                        (height(*pa) - expected).abs() < 2e-4 * (1.0 + expected * 100.0),
                        "{}: {h} to {}, not {expected}",
                        b.id,
                        height(*pa)
                    );
                    over += 1;
                } else if polar > 2.2 {
                    assert!((*pa - *pb).length() < 1e-5, "{}: hanging hair keeps its fall", b.id);
                    under += 1;
                }
            }
        }
    }
    assert!(over > 50 && under > 10, "{over} points over the head and {under} below the fade");
}

// ------------------------------------------------------------------------------------------------ on every template

const TEMPLATES: [&str; 4] = ["soft-sd", "mini", "standard", "tall"];

/// The default head seated on `template`'s neck, as the head part builds it.
fn template_head(template: &str) -> Arc<dyn HeadSurface> {
    let body = BodyParams::default();
    let layout = omuzaki_char::template::registry::find(template).unwrap().layout(
        &body.proportions,
        &BodyShape::default(),
        &body.hand,
    );
    let seated = omuzaki_char::head::part::seated(&HeadParams::default(), layout.head_base().unwrap().y);
    omuzaki_char::head::cranium::surface(&seated, None)
}

/// The default style plus a pair of part sweeps, with `volume`, compiled on `head`.
fn compiled_on(head: &dyn HeadSurface, volume: &HairVolume) -> Vec<CompiledEntry> {
    let mut params = HairParams { volume: Some(volume.clone()), ..HairParams::default() };
    let sweep = HairGroup {
        enabled: true,
        mirror: false,
        dynamics: Dynamics::default(),
        generator: Generator::PartSweep(PartSweepGen::default()),
        lock: LockShape::default(),
    };
    params.groups.insert(GroupId::new("e-sweep").unwrap(), sweep);
    compile_groups(&params, &Dispatch, head, &Palette::default(), Quality::Live, &mut Half).unwrap()
}

#[test]
fn the_shell_and_the_sweeps_follow_one_parting_and_one_whorl_on_every_template() {
    for template in TEMPLATES {
        let head = template_head(template);
        let centre = head.centre();
        let towards = |p: Vec3| (p - centre).normalize().as_dvec3();
        for volume in [parted(25.0), parted(-15.0), HairVolume { part: None, ..parted(0.0) }] {
            let field = Field::new(&volume);
            let entries = compiled_on(head.as_ref(), &volume);
            let find = |id: &str| entries.iter().find(|e| e.id.as_str() == id).unwrap();
            // The scalp shell's strands start at the parting (or the whorl) and run away from it.
            let GroupGeometry::Shell(shell) = &find("a-cap").group.geometry else {
                panic!("{template}: the cap is a shell")
            };
            let uv = shell.extra.iter().find(|a| a.name.as_str() == "hair-uv").unwrap();
            let (start, _) =
                shell.positions.iter().zip(uv.data.chunks(2)).min_by(|a, b| a.1[1].total_cmp(&b.1[1])).unwrap();
            let comb = field.comb(towards(*start));
            assert!(comb.distance < 0.1, "{template} {volume:?}: the shell's strands start {comb:?} off the source");
            if volume.part.is_some() {
                assert!(comb.from_part, "{template}: a parted shell starts at the parting");
            }
            // The sweeps root on the same parting, each on its own side.
            let Some(across) = field.part_normal() else { continue };
            let sweeps = &find("e-sweep").group.clumps;
            let per_side = sweeps.len() / 2;
            for (n, clump) in sweeps.iter().enumerate() {
                let root = towards(clump.rest[0]);
                let comb = field.comb(root);
                assert!(comb.from_part && comb.distance < 0.03, "{template}, sweep {n}: {comb:?}");
                let side = if n < per_side { 1.0 } else { -1.0 };
                assert!(root.dot(across) * side > 0.0, "{template}, sweep {n} roots on the wrong side");
            }
        }
    }
}

// ------------------------------------------------------------------------------------------------ on the kit

fn part(kind: &str, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

#[test]
fn a_full_volume_builds_on_every_template() {
    omuzaki_char::link();
    let volume = HairVolume {
        part: Some(HairParting { side_deg: 30.0, front_deg: 70.0, back_deg: 40.0 }),
        whorl: HairWhorl { polar_deg: 40.0, azimuth_deg: 150.0 },
        fullness: 1.8,
        crown: 1.4,
        back: 1.3,
    };
    for template in ["soft-sd", "mini", "standard", "tall"] {
        let mut def: CharacterDef = mock_character().unwrap();
        def.template = BodyTemplateId::new(template).unwrap();
        def.parts.clear();
        def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
        def.parts.insert(PartId::new("head").unwrap(), part("head", json!({})));
        let params = HairParams { volume: Some(volume.clone()), ..HairParams::default() };
        def.parts.insert(PartId::new("hair").unwrap(), part("hair", serde_json::to_value(params).unwrap()));
        let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
        let mut assembler = Assembler::new(&id, &def, &options);
        for p in plan(&def).unwrap() {
            let out = build_part(&def, &p, &options, &services, assembler.published())
                .unwrap_or_else(|e| panic!("{template}: {e}"));
            assembler.add(&p, out).unwrap();
        }
    }
}
