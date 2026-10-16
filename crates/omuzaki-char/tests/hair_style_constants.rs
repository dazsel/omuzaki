//! The cap's and the part sweep's style constants as `CapGen` and `PartSweepGen` fields (#142), and what
//! `CapGen::thickness` means (#1595): the offset at the hairline, with the crown standing `1 + crown_rise` times as high.
//! The defaults are the numbers the generators used before the fields existed, so nothing changes for a saved file.

#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::float_cmp, clippy::cast_precision_loss)] // exact values that were copied, not computed

use omuzaki_char::hair::r#gen::cap::{self, RINGS, SEGMENTS};
use omuzaki_char::hair::r#gen::part_sweep::{self, Lock};
use omuzaki_char::hair::compile::{Forked, HairGenCtx};
use omuzaki_char_api::{CapGen, Dynamics, GroupId, HAIR_INFO, HeadSurface, PartSweepGen, Quality};
use omuzaki_core::rng::Rng;
use omuzaki_face_api::Palette;
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use serde_json::json;

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

fn shell(g: &CapGen) -> MeshData {
    let mut m = MeshData::default();
    cap::fill_shell(g, &Ball, &mut m);
    m
}

fn locks(g: &PartSweepGen) -> Vec<Lock> {
    let mut l = Vec::new();
    part_sweep::rebuild(g, &Ball, &mut l);
    l
}

/// The vertex of ring `ring` and column `column` of the shell.
fn vertex(m: &MeshData, ring: usize, column: usize) -> Vec3 {
    m.positions[ring * (SEGMENTS + 1) + column]
}

#[test]
fn the_defaults_are_the_numbers_the_generators_used() {
    let c = CapGen::default();
    assert_eq!((c.front_blend, c.notch_width, c.notch_edges), ([-0.2, 0.75], 0.10, [-0.6, 0.2]));
    assert_eq!((c.crown_rise, c.strand_seed, c.shade), (0.625, 0.37, 0.93));
    let s = PartSweepGen::default();
    assert_eq!((s.part_start, s.part_spread, s.end_spread, s.sweep_ease), (0.02, 0.02, 0.9, 0.9));
    assert_eq!((s.height_blend_deg, s.lift, s.belly, s.alternate_step), ([70.0, 180.0], 0.030, 0.016, 0.005));
    assert_eq!(s.shades, [0.97, 1.0]);
    assert_eq!(part_sweep::HEIGHT_BLEND, (s.height_blend_deg[0], s.height_blend_deg[1]), "the braid shares the default");
}

#[test]
fn a_saved_file_without_the_new_fields_reads_back_as_the_defaults() {
    let old_cap = json!({ "hairline_front_deg": 58.0, "hairline_back_deg": 118.0, "ear_notch_deg": 16.0, "thickness": 0.016 });
    assert_eq!(serde_json::from_value::<CapGen>(old_cap).unwrap(), CapGen::default());
    let old_sweep = json!({ "count": 26, "part_front_deg": -32.0 });
    assert_eq!(serde_json::from_value::<PartSweepGen>(old_sweep).unwrap(), PartSweepGen::default());
}

#[test]
fn thickness_is_the_offset_at_the_hairline_and_the_crown_stands_higher_by_the_rise() {
    for g in [CapGen::default(), CapGen { thickness: 0.03, crown_rise: 1.0, ..CapGen::default() }] {
        let m = shell(&g);
        let off = |p: Vec3| f64::from((p - CENTRE).length() - R);
        // Ring 0 is the crown (theta 0): all of its vertices are one point.
        let crown = off(vertex(&m, 0, 0));
        assert!((crown - g.thickness * (1.0 + g.crown_rise)).abs() < 1e-5, "crown {crown} on {g:?}");
        // The last ring at the back (phi = pi) reaches the back hairline, which is the reach of the grid.
        let hairline = off(vertex(&m, RINGS, SEGMENTS / 2));
        assert!((hairline - g.thickness).abs() < 1e-5, "hairline {hairline} on {g:?}");
    }
}

#[test]
fn the_crown_rise_field_sets_how_high_the_crown_stands() {
    let flat = shell(&CapGen { crown_rise: 0.0, ..CapGen::default() });
    let crown = (vertex(&flat, 0, 0) - CENTRE).length() - R;
    assert!((crown - 0.016).abs() < 1e-5, "no rise: the crown stands at the thickness, not {crown}");
}

#[test]
fn the_ear_notch_and_the_forehead_blend_move_the_hairline() {
    let base = shell(&CapGen::default());
    // Forward of the ear (phi = 60 degrees, cos = 0.5), where the notch's width still matters.
    let side = SEGMENTS / 6;
    let end = |m: &MeshData| vertex(m, RINGS, side);
    let wide = shell(&CapGen { notch_width: 0.5, ..CapGen::default() });
    assert_ne!(end(&base), end(&wide), "a wider notch lifts the hairline over the ear further");
    let no_notch = shell(&CapGen { ear_notch_deg: 0.0, ..CapGen::default() });
    assert_ne!(end(&base), end(&no_notch));
    // The forehead (phi = 0) takes the front hairline only once cos(phi) has passed the blend's upper end.
    let late = shell(&CapGen { front_blend: [0.9, 1.0], ..CapGen::default() });
    let front = |m: &MeshData| vertex(m, RINGS, 0);
    assert_eq!(front(&base), front(&late), "at phi = 0 both blends are complete");
    assert_ne!(vertex(&base, RINGS, SEGMENTS / 8), vertex(&late, RINGS, SEGMENTS / 8));
    let edges = shell(&CapGen { notch_edges: [0.3, 0.9], ..CapGen::default() });
    assert_ne!(end(&base), end(&edges));
}

#[test]
fn the_strand_seed_and_shade_are_the_cap_s_hair_info() {
    let m = shell(&CapGen { strand_seed: 0.5, shade: 0.8, ..CapGen::default() });
    let info = &m.extra_attribute(HAIR_INFO).unwrap().data;
    assert!(info.chunks(3).all(|v| v[0] == 0.5 && v[1] == 0.8));
    let d = shell(&CapGen::default());
    let info = &d.extra_attribute(HAIR_INFO).unwrap().data;
    assert!(info.chunks(3).all(|v| v[0] == 0.37 && v[1] == 0.93));
}

#[test]
fn each_part_sweep_field_changes_the_locks() {
    let base = locks(&PartSweepGen::default());
    let changed: [(&str, PartSweepGen); 8] = [
        ("part_start", PartSweepGen { part_start: 0.2, ..PartSweepGen::default() }),
        ("part_spread", PartSweepGen { part_spread: 0.06, ..PartSweepGen::default() }),
        ("end_spread", PartSweepGen { end_spread: 1.5, ..PartSweepGen::default() }),
        ("sweep_ease", PartSweepGen { sweep_ease: 1.5, ..PartSweepGen::default() }),
        ("height_blend_deg", PartSweepGen { height_blend_deg: [20.0, 120.0], ..PartSweepGen::default() }),
        ("lift", PartSweepGen { lift: 0.06, ..PartSweepGen::default() }),
        ("belly", PartSweepGen { belly: 0.04, ..PartSweepGen::default() }),
        ("alternate_step", PartSweepGen { alternate_step: 0.02, ..PartSweepGen::default() }),
    ];
    for (name, g) in changed {
        assert_ne!(locks(&g), base, "{name} moves the locks");
    }
}

#[test]
fn the_shades_alternate_between_the_two_fields() {
    let (id, dynamics, palette) = (GroupId::new("sweep").unwrap(), Dynamics::default(), Palette::default());
    let mut rng = Rng::new(1);
    let mut forked = Forked(&mut rng);
    let mut ctx = HairGenCtx::new(&Ball, &palette, Quality::Live, &id, &dynamics, &mut forked);
    let g = PartSweepGen { count: 4, shades: [0.6, 0.7], ..PartSweepGen::default() };
    let shades: Vec<f32> = part_sweep::compile(&g, &mut ctx).clumps.iter().map(|c| c.shade).collect();
    assert_eq!(shades, [0.6, 0.7, 0.6, 0.7, 0.6, 0.7, 0.6, 0.7]);
}
