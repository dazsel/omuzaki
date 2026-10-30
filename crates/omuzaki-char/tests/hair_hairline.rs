//! The front hairline's shape (#2517): a curve across the forehead (an arch, a widow's peak, receding temples) that
//! the scalp shell ends on, a band above it where the shell's strands thin out, and baby hairs that follow it. Every
//! named shape builds on every template. Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)] // fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::f64::consts::PI;
use std::sync::Arc;

use omuzaki_char::hair::r#gen::cap;
use omuzaki_char::hair::strands;
use omuzaki_char::template::registry::templates;
use omuzaki_char_api::{
    Assembler, BodyTemplateId, BuildOptions, BuildServices, CapGen, CharId, CharacterDef, Generator, HAIR_INFO,
    HairParams, Hairline, HeadFact, HeadSurface, PartDef, PartId, PartKindId, PartOutput, build_part, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
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

fn shaped(name: &str) -> CapGen {
    CapGen { hairline: Hairline::preset(name).unwrap(), ..CapGen::default() }
}

/// The hairline's polar angle, degrees, at `x` across the forehead (0 the middle, 1 the temple).
fn at(g: &CapGen, x: f64) -> f64 {
    cap::hairline_at(g, (x * Hairline::FOREHEAD).asin()).to_degrees()
}

#[test]
fn each_shape_bends_the_line_across_the_forehead_its_own_way() {
    let level = CapGen::default();
    let middle = level.hairline_front_deg;
    assert!((at(&level, 0.0) - middle).abs() < 0.01, "the middle is hairline_front_deg");
    assert!((at(&level, 0.4) - middle).abs() < 1.0, "the default is level across the forehead");

    let rounded = shaped("rounded");
    assert!(at(&rounded, 0.9) > at(&rounded, 0.0) + 4.0, "an arch sits lower at the temples");

    let peak = shaped("widows_peak");
    let (mid, off) = (at(&peak, 0.0), at(&peak, 0.35));
    assert!(mid > off + 5.0, "a widow's peak dips in the middle: {mid} against {off}");
    let near = at(&peak, 0.05);
    assert!(mid > near, "and comes to a point: {mid} then {near}");

    let m = shaped("m_shape");
    let (temple, mid) = (at(&m, 0.85), at(&m, 0.4));
    assert!(temple < mid - 5.0, "an M-shape recedes at the temples: {temple} against {mid}");

    // The back is untouched by any shape.
    for name in Hairline::PRESETS {
        let g = shaped(name);
        let back = cap::hairline_at(&g, PI).to_degrees();
        assert!((back - g.hairline_back_deg).abs() < 1e-6, "{name}: the nape stays at hairline_back_deg");
    }
}

#[test]
fn the_shell_ends_on_the_curve_and_thins_toward_it() {
    let head = Egg;
    for name in Hairline::PRESETS {
        let g = shaped(name);
        let mut m = MeshData::default();
        cap::fill_shell(&g, &head, &mut m);
        let info = &m.extra_attribute(HAIR_INFO).unwrap().data;
        // The shell's lowest point at each azimuth is the hairline there (hair-info.z is the polar angle).
        for (i, c) in info.as_chunks::<3>().0.iter().enumerate() {
            let phi = (i % (cap::SEGMENTS + 1)) as f64 / cap::SEGMENTS as f64 * 2.0 * PI;
            let edge = cap::hairline_at(&g, phi);
            assert!(f64::from(c[2]) <= edge + 1e-5, "{name}: no vertex below the hairline");
        }
        // Thinning: the vertices in the band above the front hairline take the strand texture's tips.
        let band = g.hairline.density_fade_deg.to_radians();
        let tip_start = (1.0 - strands::TIP) * strands::V_END;
        let mut thinned = 0;
        for (i, (uv, c)) in m.uvs.iter().zip(info.as_chunks::<3>().0).enumerate() {
            let phi = (i % (cap::SEGMENTS + 1)) as f64 / cap::SEGMENTS as f64 * 2.0 * PI;
            let edge = cap::hairline_at(&g, phi);
            if phi.cos() > 0.9 && f64::from(c[2]) > edge - band * 0.5 {
                assert!(uv.y >= tip_start - 1e-5, "{name}: v {} near the hairline is in the tip band", uv.y);
                thinned += 1;
            }
        }
        assert!(thinned > 0, "{name}: the band holds vertices");
    }
    // Without a band the shell's v stays in the body of the texture, as before.
    let mut hard = MeshData::default();
    cap::fill_shell(&CapGen::default(), &head, &mut hard);
    let tip_start = (1.0 - strands::TIP) * strands::V_END;
    assert!(hard.uvs.iter().all(|uv| uv.y <= tip_start + 1e-5), "a hard edge keeps every strand whole");
}

fn part(kind: &str, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The hair part `hair` builds on `template` with the default body and head, and the head it is built on.
fn hair_on(template: &str, hair: &HairParams) -> (PartOutput, Arc<dyn HeadSurface>) {
    omuzaki_char::link();
    let mut def: CharacterDef = mock_character().unwrap();
    def.template = BodyTemplateId::new(template).unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", json!({})));
    def.parts.insert(PartId::new("hair").unwrap(), part("hair", serde_json::to_value(hair).unwrap()));
    let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
    let mut assembler = Assembler::new(&id, &def, &options);
    for p in plan(&def).unwrap() {
        let out = build_part(&def, &p, &options, &services, assembler.published())
            .unwrap_or_else(|e| panic!("{template}: {e}"));
        if p.as_str() == "hair" {
            let head = assembler.published().get::<HeadFact>().clone().unwrap();
            return (out, head);
        }
        assembler.add(&p, out).unwrap();
    }
    panic!("no hair part was planned")
}

fn with_hairline(name: &str, baby: u32) -> HairParams {
    let mut hair = HairParams::default();
    hair.strays.share = 0.0;
    hair.strays.baby = baby;
    for group in hair.groups.values_mut() {
        if let Generator::Cap(cap) = &mut group.generator {
            cap.hairline = Hairline::preset(name).unwrap();
        }
    }
    hair
}

#[test]
fn every_shape_builds_on_every_template() {
    for template in templates() {
        for name in Hairline::PRESETS {
            let (out, _) = hair_on(template.id(), &with_hairline(name, 0));
            assert!(!out.meshes.is_empty(), "{name} on {}: the hair builds", template.id());
        }
    }
}

#[test]
fn baby_hairs_follow_a_widows_peak_down_the_middle() {
    // The middle baby hairs of 41 root at the front; their roots sit lower on a widow's peak than on a level line.
    let lowest = |name: &str| {
        let stat = |baby: u32| {
            let (out, head) = hair_on("standard", &with_hairline(name, baby));
            let mesh = out.meshes.into_iter().find(|m| m.geometry.extra_attribute(HAIR_INFO).is_some()).unwrap();
            (mesh.geometry.positions, head.centre())
        };
        let ((bare, _), (with, centre)) = (stat(0), stat(41));
        // The baby hairs come last in the static mesh; the lowest of them near the middle of the face is a root.
        with[bare.len()..]
            .iter()
            .filter(|p| (p.x - centre.x).abs() < 0.004 && p.z > centre.z)
            .map(|p| p.y)
            .fold(f32::INFINITY, f32::min)
    };
    let (level, peak) = (lowest("level"), lowest("widows_peak"));
    println!("lowest baby hair in the middle: level {level}, widow's peak {peak}");
    assert!(peak < level - 0.004, "the widow's peak carries the middle baby hairs down: {peak} against {level}");
}
