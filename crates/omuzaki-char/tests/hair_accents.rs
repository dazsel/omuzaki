//! Accent pieces on the shared volume (#2484): an ahoge, a bun or a tail rides the hair's fullness as one piece, by how
//! far its root rises, instead of stretching with it, and its spring chain, ties and anchors move with it, so the drawn
//! hair and its rig agree. Each accent takes every lock shape as the main locks do. Everything is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used)] // fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_possible_truncation)]

use omuzaki_char::hair::compile::{CompiledEntry, Dispatch, GroupGeometry, UnitStream, compile_groups};
use omuzaki_char::hair::r#gen::{ahoge, bun, tail};
use omuzaki_char::hair::styles;
use omuzaki_char::hair::volume::{self, Accents, Field};
use omuzaki_char_api::{
    Assembler, BodyTemplateId, BuildOptions, BuildServices, CharId, CharacterDef, Dynamics, Generator, GroupId,
    HairGroup, HairParams, HairVolume, HeadSurface, LockProfile, LockShape, LockTip, PartDef, PartId, PartKindId,
    PartOutput, Quality, build_part, plan,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::Vec3;
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

/// A full volume: the hair stands well off the head, more over the crown.
fn full() -> HairVolume {
    HairVolume { fullness: 1.6, crown: 1.25, ..HairVolume::default() }
}

fn group(generator: Generator, mirror: bool) -> HairGroup {
    HairGroup { enabled: true, mirror, dynamics: Dynamics::default(), generator, lock: LockShape::default() }
}

/// A style of accent pieces only: an ahoge, a high bun, a ponytail and mirrored twin tails.
fn accents_only() -> HairParams {
    let mut params = HairParams { volume: Some(full()), ..HairParams::default() };
    params.groups.clear();
    for (id, generator, mirror) in [
        ("ahoge", Generator::Ahoge(ahoge::antenna()), false),
        ("bun", Generator::Bun(bun::high_bun()), false),
        ("pony", Generator::Tail(tail::ponytail()), false),
        ("twin", Generator::Tail(tail::twintails()), true),
    ] {
        params.groups.insert(GroupId::new(id).unwrap(), group(generator, mirror));
    }
    params
}

fn compiled(params: &HairParams) -> Vec<CompiledEntry> {
    compile_groups(params, &Dispatch, &Egg, &Palette::default(), Quality::Live, &mut Half).unwrap()
}

#[test]
fn an_accent_piece_moves_as_one_by_its_roots_rise_and_its_gather_locks_swell() {
    let params = accents_only();
    let field = Field::new(params.volume.as_ref().unwrap());
    let accents = Accents::new(&params, Some(&field), &Egg);
    let before = compiled(&params);
    let mut after = before.clone();
    volume::swell(&mut after, &field, &Egg, &accents);
    let (mut rigid, mut swelled) = (0, 0);
    for (b, a) in before.iter().zip(&after) {
        assert!(accents.contains(&b.id), "{} is an accent piece", b.id);
        for (cb, ca) in b.group.clumps.iter().zip(&a.group.clumps) {
            let root = cb.rest[0];
            let shift = accents.shift(&b.id, root);
            let moved: Vec<Vec3> = cb.rest.iter().zip(&ca.rest).map(|(p, q)| *q - *p).collect();
            if moved.iter().all(|m| m.distance(shift) < 1e-5) {
                rigid += 1;
            } else {
                // Only a lock rooted away from the piece (a gather lock from the hairline) swells as it lies.
                let lifted: Vec<Vec3> = cb.rest.iter().map(|p| field.lifted(&Egg, *p)).collect();
                assert_eq!(ca.rest, lifted, "{}: a lock rooted at {root} neither moved as one nor swelled", b.id);
                swelled += 1;
            }
        }
        if let (GroupGeometry::Tied { ties: tb }, GroupGeometry::Tied { ties: ta }) =
            (&b.group.geometry, &a.group.geometry)
        {
            for (tb, ta) in tb.iter().zip(ta) {
                assert!((ta.centre - tb.centre).distance(accents.shift(&b.id, tb.centre)) < 1e-6, "{}'s tie", b.id);
                assert!(ta.centre != tb.centre, "{}'s tie moves with the volume", b.id);
            }
        }
    }
    assert!(rigid >= 10, "the ahoge, bun and tail locks move as one: {rigid}");
    assert!(swelled > 0, "the bun's and tails' gather locks swell");
    // Every root rises, more on the crown than at the nape, and a mirrored group's sides rise alike.
    let rise = |id: &str, p: Vec3| accents.shift(&GroupId::new(id).unwrap(), p).length();
    let ahoge_root = ahoge::root(&ahoge::antenna(), &Egg).as_vec3();
    let twin_root = tail::root(&tail::twintails(), &Egg).as_vec3();
    assert!(rise("ahoge", ahoge_root) > 0.01, "{}", rise("ahoge", ahoge_root));
    assert!(rise("bun", bun::root(&bun::high_bun(), &Egg).as_vec3()) > 0.004);
    let twin_other = Vec3::new(-twin_root.x, twin_root.y, twin_root.z);
    assert!((rise("twin", twin_root) - rise("twin", twin_other)).abs() < 1e-6 && rise("twin", twin_root) > 0.0);
}

#[test]
fn without_a_volume_nothing_moves_and_other_groups_are_not_accents() {
    let params = HairParams { volume: None, ..accents_only() };
    let accents = Accents::new(&params, None, &Egg);
    for id in ["ahoge", "bun", "pony", "twin"] {
        assert!(!accents.contains(&GroupId::new(id).unwrap()));
    }
    let defaults = HairParams { volume: Some(full()), ..HairParams::default() };
    let accents = Accents::new(&defaults, Some(&Field::new(&full())), &Egg);
    for id in defaults.groups.keys() {
        let accent =
            matches!(defaults.groups[id].generator, Generator::Ahoge(_) | Generator::Bun(_) | Generator::Tail(_));
        assert_eq!(accents.contains(id), accent, "{id}");
    }
}

#[test]
fn a_chain_and_an_anchor_move_with_their_piece() {
    let params = accents_only();
    let field = Field::new(params.volume.as_ref().unwrap());
    let accents = Accents::new(&params, Some(&field), &Egg);
    let id = GroupId::new("twin").unwrap();
    let still = tail::chains(&tail::twintails(), &Egg, None, "hair_t_twin", true).unwrap();
    let mut moved = still.clone();
    accents.move_chains(&id, &mut moved);
    for (s, m) in still.iter().zip(&moved) {
        let shift = accents.shift(&id, s.rest[0]);
        assert!(shift.length() > 0.0);
        for (p, q) in s.rest.iter().zip(&m.rest) {
            assert!((*q - *p).distance(shift) < 1e-6, "{}: every bone moves by its root's rise", s.name);
        }
    }
    let bun_id = GroupId::new("bun").unwrap();
    for (name, anchor) in bun::anchors(&bun::high_bun(), &Egg) {
        let mut a = anchor.clone();
        accents.move_anchor(&bun_id, &mut a);
        let root = bun::root(&bun::high_bun(), &Egg).as_vec3();
        assert!((a.position - anchor.position).distance(accents.shift(&bun_id, root)) < 1e-6, "{name}");
    }
}

// ------------------------------------------------------------------------------------------------ on the kit

fn part(kind: &str, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The hair part `hair` builds on `template`, with the default body and head.
fn hair_on(template: &str, hair: &HairParams) -> PartOutput {
    omuzaki_char::link();
    let mut def: CharacterDef = mock_character().unwrap();
    def.template = BodyTemplateId::new(template).unwrap();
    def.parts.clear();
    def.parts.insert(PartId::new("body").unwrap(), part("body", json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", json!({})));
    def.parts.insert(PartId::new("hair").unwrap(), part("hair", serde_json::to_value(hair).unwrap()));
    let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
    let mut assembler = Assembler::new(&id, &def, &options);
    let mut result = None;
    for p in plan(&def).unwrap() {
        let out = build_part(&def, &p, &options, &services, assembler.published())
            .unwrap_or_else(|e| panic!("{template}: {e}"));
        if p.as_str() == "hair" {
            result = Some(build_part(&def, &p, &options, &services, assembler.published()).unwrap());
        }
        assembler.add(&p, out).unwrap();
    }
    result.unwrap()
}

/// The farthest any bone of the chains named for `group` lies from the nearest vertex of the drawn hair, metres.
fn farthest_bone(out: &PartOutput, group: &str) -> f32 {
    let vertices: Vec<Vec3> = out.meshes.iter().flat_map(|m| m.geometry.positions.iter().copied()).collect();
    let bones: Vec<Vec3> = out
        .rig
        .bones
        .iter()
        .filter(|b| b.name.as_str().contains(&format!("_{group}")))
        .map(|b| b.rest.translation)
        .collect();
    assert!(!bones.is_empty(), "the {group} group has a chain");
    bones.iter().map(|b| vertices.iter().map(|v| v.distance(*b)).fold(f32::INFINITY, f32::min)).fold(0.0, f32::max)
}

#[test]
fn on_a_full_volume_the_chains_stay_in_the_hair_they_move() {
    // The library's accent styles, their volume made fuller still: every chain bone lies in its drawn locks.
    for (style, group) in [("messy-short", "f-ahoge"), ("ponytail", "tail"), ("twin-tails", "tails")] {
        let style = styles::find(style).unwrap();
        let mut hair = style.applied_to(&HairParams::default());
        let volume = hair.volume.get_or_insert_with(HairVolume::default);
        volume.fullness = 1.5;
        volume.crown = 1.3;
        let group =
            hair.groups.keys().find(|id| id.as_str().contains(group)).map_or(group.to_owned(), ToString::to_string);
        for template in ["soft-sd", "standard"] {
            let out = hair_on(template, &hair);
            let far = farthest_bone(&out, &group);
            assert!(far < 0.02, "{} on {template}: a {group} bone lies {far} m from its hair", style.id);
        }
    }
}

#[test]
fn every_lock_shape_builds_on_every_accent() {
    let base = accents_only();
    let count = |out: &PartOutput| out.meshes.iter().map(|m| m.geometry.positions.len()).sum::<usize>();
    for template in ["soft-sd", "mini", "standard", "tall"] {
        let plain = count(&hair_on(template, &base));
        for profile in [LockProfile::Lens, LockProfile::Card, LockProfile::Round] {
            for tip in [LockTip::Point, LockTip::Fork, LockTip::Blunt, LockTip::CurlIn, LockTip::CurlOut] {
                let mut hair = base.clone();
                for g in hair.groups.values_mut() {
                    g.lock = LockShape { profile, tip, ..LockShape::default() };
                }
                let vertices = count(&hair_on(template, &hair));
                assert!(
                    if tip == LockTip::Fork { vertices > plain } else { vertices > 0 },
                    "{template} {profile:?} {tip:?}: {vertices} vertices against {plain}"
                );
            }
        }
    }
}
