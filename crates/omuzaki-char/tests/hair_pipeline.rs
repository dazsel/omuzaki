//! The `hair` part kind's pipeline with a stub generator: ordering, mirroring, `enabled`, mesh attributes,
//! determinism, the legacy random stream, the material, recolour against build, and what the kind declares.
//!
//! The real generators live elsewhere, so these tests substitute a stub that makes known clumps from
//! `guides` groups, draws one random number per clump, and stands in for a cap shell and a curtain strip. Everything
//! is synthetic.

#![allow(clippy::expect_used, clippy::unwrap_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::float_cmp, clippy::unreadable_literal, clippy::default_trait_access, clippy::assert_is_empty)]
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::chunks_exact_to_as_chunks)]
#![allow(clippy::field_reassign_with_default)] // exact values that were copied, not computed

use omuzaki_char::hair::compile::{
    CompiledGroup, GroupGenerator, GroupGeometry, HairGenCtx, UnitStream, compile_groups, stream,
};
use omuzaki_char::hair::part::{build_with, recolour_with};
use omuzaki_char::hair::tube::RING;
use omuzaki_char_api::{
    Assembler, BuildCtx, BuildError, BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, Generator,
    GroupId, GuideClump, GuidePoint, GuidesGen, HAIR_INFO, HAIR_PROGRAM, HAIR_UV, HairGroup, HairParams, HairRng,
    HeadSurface, KindProvenance, PartCategory, PartDef, PartId, PartKind, PartKindId, PartMeta, PartOutput, Quality,
    RecolourCtx, Recoloured, RegistryBuilder, build_part, find_kind, plan, recolour_part,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{Palette, PaletteSlot};
use omuzaki_render_api::Shading;
use serde_json::{Value, json};

// ------------------------------------------------------------------------------------------------ the stub generator

/// The shade a stub clump gets from draw `r`: so a test can read the random stream back out of the clumps.
fn shade_of(draw: f64) -> f64 {
    0.5 + 0.5 * draw
}

/// Makes a clump per `guides` entry (one draw each), a shell for the cap and a strip for the curtain.
struct Stub;

impl GroupGenerator for Stub {
    fn compile(&self, generator: &Generator, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
        match generator {
            Generator::Guides(g) => CompiledGroup::tubes(
                g.clumps
                    .iter()
                    .map(|c| {
                        let rest = c.points.iter().map(|p| ctx.point(*p)).collect();
                        let draw = ctx.rnd();
                        ctx.clump(rest, c.width, c.thickness, shade_of(draw))
                    })
                    .collect(),
            ),
            Generator::Cap(_) => CompiledGroup::empty(GroupGeometry::Shell(shell(ctx))),
            Generator::Curtain(c) => {
                let clumps = (0..c.count)
                    .map(|k| {
                        let root = ctx.point(GuidePoint::Surface([100.0, 150.0 + 10.0 * f64::from(k), 0.02]));
                        ctx.clump(vec![root, root - Vec3::Y * 0.1], c.width, c.thickness, 1.0)
                    })
                    .collect();
                CompiledGroup { clumps, geometry: GroupGeometry::Strip { columns: c.columns, u: Vec::new() } }
            }
            _ => CompiledGroup::tubes(Vec::new()),
        }
    }

    fn clump_count(&self, generator: &Generator) -> usize {
        match generator {
            Generator::Guides(g) => g.clumps.len(),
            Generator::Curtain(c) => usize::from(c.count),
            _ => 0,
        }
    }
}

/// A one-triangle "shell" over the crown, with the hair attributes, wound counter-clockwise from above.
fn shell(ctx: &HairGenCtx<'_>) -> MeshData {
    let top = ctx.point(GuidePoint::Surface([5.0, 0.0, 0.01]));
    let mut m = MeshData {
        positions: vec![top, top + Vec3::new(0.0, 0.0, 0.05), top + Vec3::new(0.05, 0.0, 0.0)],
        normals: vec![Vec3::Y; 3],
        indices: vec![0, 1, 2],
        ..MeshData::default()
    };
    let attribute = |name: &str, components, data: Vec<f32>| omuzaki_core::mesh::ExtraAttribute {
        name: omuzaki_core::id::Slug::new(name).unwrap(),
        components,
        data,
    };
    m.extra = vec![attribute(HAIR_UV, 2, vec![0.0; 6]), attribute(HAIR_INFO, 3, vec![0.5; 9])];
    m
}

// ------------------------------------------------------------------------------------------------ fixtures

fn gid(s: &str) -> GroupId {
    GroupId::new(s).unwrap()
}

fn pid(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

/// A lock from `phi` degrees round the head, down the side, from the crown to below the ear.
fn lock(phi: f64) -> GuideClump {
    GuideClump {
        points: [20.0, 55.0, 90.0, 125.0].map(|theta| GuidePoint::Surface([theta, phi, 0.01 + theta * 1e-4])).to_vec(),
        ..GuideClump::default()
    }
}

fn group(clumps: Vec<GuideClump>, simulate: bool, mirror: bool) -> HairGroup {
    let mut g = HairGroup {
        enabled: true,
        mirror,
        dynamics: Default::default(),
        generator: Generator::Guides(GuidesGen { clumps }),
    };
    g.dynamics.simulate = simulate;
    g
}

fn params(groups: Vec<(&str, HairGroup)>) -> HairParams {
    HairParams { groups: groups.into_iter().map(|(id, g)| (gid(id), g)).collect(), ..HairParams::default() }
}

/// A sphere standing in for a head: centre `(0, 0.2, 0)`, radius 0.1.
#[derive(Debug)]
struct Ball;

impl HeadSurface for Ball {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 0.2, 0.0)
    }
    fn radius(&self, _: Vec3) -> f32 {
        0.1
    }
}

fn compile(p: &HairParams, rng: &mut dyn UnitStream) -> Vec<omuzaki_char::hair::compile::CompiledEntry> {
    compile_groups(p, &Stub, &Ball, &Palette::default(), Quality::Live, rng).unwrap()
}

// A part kind that builds the real pipeline with the stub generator, so a character can be built and recoloured through
// the registry exactly as the real kind is.
struct StubHair;

impl PartKind for StubHair {
    type Params = HairParams;
    fn meta() -> PartMeta {
        PartMeta {
            id: "test-stub-hair",
            summary: "hair with stub generators",
            category: PartCategory::Hair,
            requires: &["head"],
        }
    }
    fn build(p: &HairParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        build_with(p, ctx, &Stub)
    }
    fn recolour(p: &HairParams, out: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour_with(p, out, ctx, &Stub)
    }
}

omuzaki_char_api::register_part_kind!(StubHair);

fn part(kind: &str, params: Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer: 0, attach: None, params }
}

/// The soft-sd template body, the real head and the stub hair.
fn character(hair: &HairParams) -> CharacterDef {
    let mut def = mock_character().unwrap();
    def.parts.clear();
    def.parts.insert(pid("body"), part("body", json!({})));
    def.parts.insert(pid("head"), part("head", json!({})));
    def.parts.insert(pid("hair"), part("test-stub-hair", serde_json::to_value(hair).unwrap()));
    def
}

fn hair_output(def: &CharacterDef) -> Result<PartOutput, BuildError> {
    let (id, options, services) = (CharId::new("t").unwrap(), BuildOptions::default(), BuildServices::default());
    let mut assembler = Assembler::new(&id, def, &options);
    let mut result = None;
    for part in plan(def).unwrap() {
        let out = build_part(def, &part, &options, &services, assembler.published())?;
        if part.as_str() == "hair" {
            result = Some(build_part(def, &part, &options, &services, assembler.published())?);
        }
        assembler.add(&part, out).unwrap();
    }
    Ok(result.unwrap())
}

fn exact(out: &PartOutput) -> String {
    format!("{:?}\n{:?}\n{:?}", out.meshes, out.materials, out.warnings)
}

fn static_mesh(out: &PartOutput) -> &omuzaki_char_api::PartMesh {
    out.meshes.iter().find(|m| !m.dynamic).expect("a static mesh")
}

// ------------------------------------------------------------------------------------------------ the kind

#[test]
fn the_kind_is_registered_project_owned_and_needs_the_head() {
    omuzaki_char::link();
    let reg = find_kind("hair").expect("registered");
    let meta = (reg.meta)();
    assert_eq!((meta.category, meta.requires), (PartCategory::Hair, &["head"][..]));
    assert_eq!((reg.provenance)(), KindProvenance::ProjectOwned);
    assert_eq!((reg.defaults)(), serde_json::to_value(HairParams::default()).unwrap());
}

#[test]
fn the_real_kind_builds_the_default_style_on_the_template_body_with_its_cap_shell() {
    // The cap is a real generator: the default style's static mesh holds its shell, and it is no longer reported as a
    // stub. (The default style has no part sweep.)
    let mut def = character(&HairParams::default());
    def.parts.insert(pid("hair"), part("hair", json!({})));
    let out = hair_output(&def).unwrap();
    assert!(out.meshes.iter().any(|m| !m.dynamic && !m.geometry.positions.is_empty()));
    assert!(!out.warnings.iter().any(|w| w.contains("a-cap")), "{:?}", out.warnings);
    assert_eq!(out.materials.len(), 1);
}

// ------------------------------------------------------------------------------------------------ compile

#[test]
fn groups_compile_in_id_order_and_each_clump_carries_its_groups_motion() {
    let p = params(vec![
        ("b-second", group(vec![lock(40.0)], true, false)),
        ("a-first", group(vec![lock(-40.0), lock(-60.0)], false, false)),
    ]);
    let entries = compile(&p, &mut stream(HairRng::Forked, &mut Rng::new(3)).unwrap());
    let ids: Vec<_> = entries.iter().map(|e| e.id.as_str()).collect();
    assert_eq!(ids, ["a-first", "b-second"]);
    assert_eq!(entries[0].group.clumps.len(), 2);
    let clump = &entries[1].group.clumps[0];
    assert!(clump.simulate && clump.group == gid("b-second"));
    assert!(!entries[0].group.clumps[0].simulate);
}

#[test]
fn a_disabled_group_is_skipped_and_draws_nothing() {
    let mut off = group(vec![lock(20.0)], false, false);
    off.enabled = false;
    let with = params(vec![
        ("a", group(vec![lock(-20.0)], false, false)),
        ("b", off),
        ("c", group(vec![lock(60.0)], false, false)),
    ]);
    let without =
        params(vec![("a", group(vec![lock(-20.0)], false, false)), ("c", group(vec![lock(60.0)], false, false))]);
    let (a, b) = (
        compile(&with, &mut stream(HairRng::Forked, &mut Rng::new(9)).unwrap()),
        compile(&without, &mut stream(HairRng::Forked, &mut Rng::new(9)).unwrap()),
    );
    assert_eq!(a.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a", "c"]);
    assert_eq!(
        a.iter().map(|e| &e.group.clumps).collect::<Vec<_>>(),
        b.iter().map(|e| &e.group.clumps).collect::<Vec<_>>()
    );
}

#[test]
fn mirrored_clumps_are_the_originals_reflected_across_the_midline_and_use_no_draws() {
    let locks = vec![lock(30.0), lock(70.0)];
    let mirrored =
        params(vec![("a", group(locks.clone(), false, true)), ("b", group(vec![lock(-10.0)], false, false))]);
    let plain = params(vec![("a", group(locks, false, false)), ("b", group(vec![lock(-10.0)], false, false))]);
    let (m, p) = (
        compile(&mirrored, &mut stream(HairRng::Forked, &mut Rng::new(5)).unwrap()),
        compile(&plain, &mut stream(HairRng::Forked, &mut Rng::new(5)).unwrap()),
    );
    let (a, original) = (&m[0].group.clumps, &p[0].group.clumps);
    assert_eq!(a.len(), 4);
    assert_eq!(&a[..2], &original[..], "originals first");
    for (mirror, orig) in a[2..].iter().zip(original) {
        for (q, o) in mirror.rest.iter().zip(&orig.rest) {
            assert_eq!(*q, Vec3::new(-o.x, o.y, o.z), "x reflected through the head centre's x = 0");
        }
        assert_eq!((mirror.shade, mirror.width, mirror.thickness), (orig.shade, orig.width, orig.thickness));
    }
    assert_eq!(m[1].group.clumps, p[1].group.clumps, "the next group's draws are unshifted by mirroring");
}

#[test]
fn a_generator_that_misreports_its_clump_count_is_an_error() {
    struct Liar;
    impl GroupGenerator for Liar {
        fn compile(&self, g: &Generator, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
            Stub.compile(g, ctx)
        }
        fn clump_count(&self, g: &Generator) -> usize {
            Stub.clump_count(g) + 1
        }
    }
    let p = params(vec![("a", group(vec![lock(0.0)], false, false))]);
    let mut rng = Rng::new(1);
    let err = compile_groups(
        &p,
        &Liar,
        &Ball,
        &Palette::default(),
        Quality::Live,
        stream(HairRng::Forked, &mut rng).unwrap().as_mut(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("made 1 clumps but reports 2"), "{err}");
}

// ------------------------------------------------------------------------------------------------ random streams

#[test]
fn legacy_mode_hands_every_group_the_prototypes_stream_in_id_order() {
    let p = params(vec![
        ("b", group(vec![lock(10.0), lock(20.0)], false, false)),
        ("a", group(vec![lock(30.0)], true, false)),
    ]);
    let mut rng = Rng::new(1);
    let entries = compile(&p, &mut stream(HairRng::LegacyLcg { seed: 7 }, &mut rng).unwrap());
    let shades: Vec<f32> = entries.iter().flat_map(|e| e.group.clumps.iter().map(|c| c.shade)).collect();
    // Node: seed 7, then 7 * 16807 mod 2147483647 ... (see the parity test for the first 10 000 draws).
    let prototype = [5.4784119180202595e-05, 0.9207645169652668, 0.2892372550342579];
    for (shade, draw) in shades.iter().zip(prototype) {
        assert_eq!(*shade, shade_of(draw) as f32);
    }
}

#[test]
fn a_forked_stream_is_the_parts_own_and_reproducible() {
    let p = params(vec![("a", group(vec![lock(10.0), lock(20.0)], false, false))]);
    let draw = |seed| {
        let mut rng = Rng::new(seed);
        compile(&p, &mut stream(HairRng::Forked, &mut rng).unwrap())[0]
            .group
            .clumps
            .iter()
            .map(|c| c.shade)
            .collect::<Vec<_>>()
    };
    assert_eq!(draw(4), draw(4));
    assert_ne!(draw(4), draw(5));
}

// ------------------------------------------------------------------------------------------------ the part

fn two_group_style() -> HairParams {
    params(vec![
        ("a-static", group(vec![lock(-30.0), lock(-70.0)], false, true)),
        ("b-moving", group(vec![lock(160.0)], true, false)),
    ])
}

#[test]
fn static_groups_make_one_head_skinned_mesh_and_simulated_groups_a_dynamic_one() {
    let out = hair_output(&character(&two_group_style())).unwrap();
    let names: Vec<_> = out.meshes.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["hair-static", "hair-dynamic"]);
    let (stat, dynamic) = (&out.meshes[0], &out.meshes[1]);
    assert!(!stat.dynamic && stat.joint_bones == [omuzaki_char_api::JointRef::HEAD]);
    assert!(
        stat.geometry.joints.iter().all(|j| *j == [0; 4])
            && stat.geometry.weights.iter().all(|w| *w == [1.0, 0.0, 0.0, 0.0])
    );
    assert!(dynamic.dynamic && dynamic.joint_bones.is_empty() && !dynamic.geometry.is_skinned());
    // 4 static clumps (2 locks, mirrored), 1 dynamic, 4 rings of 8 each.
    assert_eq!(stat.geometry.positions.len(), 4 * 4 * RING);
    assert_eq!(dynamic.geometry.positions.len(), 4 * RING);
    assert!(stat.casts_shadow && stat.render_order == 1);
    assert!(out.runtime.is_none(), "the simulation is not built yet");
}

#[test]
fn mesh_attributes_validate_and_have_the_right_widths() {
    let out = hair_output(&character(&two_group_style())).unwrap();
    for mesh in &out.meshes {
        mesh.validate().unwrap();
        let g = &mesh.geometry;
        let n = g.positions.len();
        assert_eq!(g.extra.len(), 2);
        assert_eq!((g.extra[0].name.as_str(), g.extra[0].components, g.extra[0].data.len()), (HAIR_UV, 2, 2 * n));
        assert_eq!((g.extra[1].name.as_str(), g.extra[1].components, g.extra[1].data.len()), (HAIR_INFO, 3, 3 * n));
        assert_eq!((g.normals.len(), g.colours.len()), (n, n));
        assert!(g.normals.iter().all(|v| (v.length() - 1.0).abs() < 1e-3));
        assert!(g.extra[1].data.chunks_exact(3).all(|c| (0.0..=std::f32::consts::PI).contains(&c[2])), "polar angle");
    }
}

#[test]
fn groups_are_meshed_in_id_order_and_a_mirrored_group_follows_its_originals() {
    let out = hair_output(&character(&two_group_style())).unwrap();
    let positions = &static_mesh(&out).geometry.positions;
    let root = |clump: usize| positions[clump * 4 * RING..clump * 4 * RING + RING].iter().sum::<Vec3>() / RING as f32;
    // Clumps 0 and 1 are the locks at phi -30 and -70 (x < 0 on the character's right), 2 and 3 their mirrors.
    assert!(root(0).x < 0.0 && root(1).x < 0.0 && root(2).x > 0.0 && root(3).x > 0.0);
    assert!((root(0).x + root(2).x).abs() < 1e-5 && (root(1).x + root(3).x).abs() < 1e-5);
    assert!((root(0).y - root(2).y).abs() < 1e-5);
}

#[test]
fn the_same_seed_builds_identical_bytes_and_a_different_seed_does_not() {
    let def = character(&two_group_style());
    assert_eq!(exact(&hair_output(&def).unwrap()), exact(&hair_output(&def).unwrap()));
    let mut other = def.clone();
    other.seed = def.seed.wrapping_add(1);
    assert_ne!(exact(&hair_output(&def).unwrap()), exact(&hair_output(&other).unwrap()), "shade follows the draws");
}

#[test]
fn a_disabled_group_is_not_in_the_mesh() {
    let mut style = two_group_style();
    style.groups.get_mut(&gid("b-moving")).unwrap().enabled = false;
    let out = hair_output(&character(&style)).unwrap();
    assert_eq!(out.meshes.len(), 1);
    assert!(!out.meshes[0].dynamic);
}

#[test]
fn a_shell_follows_the_tubes_and_a_strip_is_reported_not_drawn() {
    let mut style = two_group_style();
    style.groups.insert(
        gid("0-cap"),
        HairGroup { generator: Generator::Cap(Default::default()), ..group(vec![], false, false) },
    );
    let mut curtain = omuzaki_char_api::CurtainGen::default();
    curtain.count = 3;
    style
        .groups
        .insert(gid("z-curtain"), HairGroup { generator: Generator::Curtain(curtain), ..group(vec![], true, false) });
    let out = hair_output(&character(&style)).unwrap();
    let stat = static_mesh(&out);
    stat.validate().unwrap();
    let tube_vertices = 4 * 4 * RING;
    assert_eq!(
        stat.geometry.positions.len(),
        tube_vertices + 3,
        "tubes first, the cap's triangle after, though the cap is group 0"
    );
    assert_eq!(
        stat.geometry.indices.len(),
        4 * (4 - 1) * RING * 6 + 3,
        "the shell's triangle is renumbered after the tubes"
    );
    assert!(out.warnings.iter().any(|w| w.contains("z-curtain") && w.contains("strip")), "{:?}", out.warnings);
    let dynamic = out.meshes.iter().find(|m| m.dynamic).unwrap();
    assert_eq!(dynamic.geometry.positions.len(), 4 * RING, "the strip's clumps add nothing to the tube mesh");
}

#[test]
fn a_guide_clump_with_one_point_is_refused_before_any_generator_runs() {
    let bad = GuideClump { points: vec![GuidePoint::Surface([20.0, 0.0, 0.01])], ..GuideClump::default() };
    let err = hair_output(&character(&params(vec![("a", group(vec![bad], false, false))]))).unwrap_err();
    assert!(err.to_string().contains("at least two points"), "{err}");
}

// ------------------------------------------------------------------------------------------------ the material

#[test]
fn the_material_is_the_hair_program_double_sided_with_the_hairs_outline_and_the_brows_overlay() {
    let mut style = two_group_style();
    let out = hair_output(&character(&style)).unwrap();
    let [(name, material)] = out.materials.as_slice() else { panic!("one material") };
    assert_eq!(name.as_str(), "hair-hair");
    assert!(out.meshes.iter().all(|m| &m.material == name));
    let Shading::Program { name: program, .. } = &material.shading else { panic!("the hair program") };
    assert_eq!(program.as_str(), HAIR_PROGRAM);
    assert!(material.double_sided && material.vertex_colours && material.receives_shadows);
    let outline = material.outline.expect("an outline");
    assert_eq!((outline.colour, outline.width), (Colour::rgb(0xa4, 0x78, 0x68), 0.0014), "the prototype's default");

    // Overrides take effect as given, including explicitly choosing the kit's old line colour and width.
    for (colour, width) in [("#8c685c", 0.0012), ("@skin_line", 0.003)] {
        style.shading.outline = serde_json::from_value(json!(colour)).unwrap();
        style.shading.outline_width = width;
        let mut def = character(&style);
        colourway(&mut def, 30);
        let o = hair_output(&def).unwrap().materials[0].1.outline.expect("an outline");
        let expected = if colour.starts_with('@') {
            def.palette.0[&PaletteSlot::new("skin_line").unwrap()]
        } else {
            Colour::rgb(0x8c, 0x68, 0x5c)
        };
        assert_eq!((o.colour, o.width), (expected, width as f32), "{colour}");
    }
    style = two_group_style();
    assert_eq!(material.overlays.len(), 1, "brows through the hair");

    style.shading.brows_through = false;
    let out = hair_output(&character(&style)).unwrap();
    assert!(out.materials[0].1.overlays.is_empty());
}

// ------------------------------------------------------------------------------------------------ recolour

fn colourway(def: &mut CharacterDef, n: u8) {
    for (i, name) in ["skin", "skin_shade", "skin_line", "outfit", "streak"].into_iter().enumerate() {
        let k = n.wrapping_add(u8::try_from(i).unwrap() * 40);
        def.palette.0.insert(PaletteSlot::new(name).unwrap(), Colour::rgb(k, k.wrapping_mul(5), 255 - k));
    }
}

fn streaked() -> HairParams {
    let mut style = params(vec![
        ("a-cap", HairGroup { generator: Generator::Cap(Default::default()), ..group(vec![], false, false) }),
        ("b-locks", group(vec![lock(-30.0), lock(-70.0), lock(100.0)], false, true)),
        ("c-moving", group(vec![lock(160.0), lock(-160.0)], true, false)),
        ("d-curtain", HairGroup { generator: Generator::Curtain(Default::default()), ..group(vec![], true, false) }),
    ]);
    let streaks = json!([
        { "fraction": 0.5, "colour": "@streak", "strength": 0.8, "seed": 3, "blend": "replace" },
        { "fraction": 0.6, "colour": "@outfit", "strength": 0.6, "seed": 9, "blend": "multiply", "groups": ["b-locks", "c-moving"] },
    ]);
    style.shading.mid = serde_json::from_value(json!("@skin")).unwrap();
    style.shading.line = serde_json::from_value(json!("@skin_line")).unwrap();
    style.shading.streaks = serde_json::from_value(streaks).unwrap();
    style
}

#[test]
fn recolouring_is_building_with_the_new_palette_byte_for_byte() {
    omuzaki_char::link();
    let mut before = character(&streaked());
    colourway(&mut before, 5);
    let mut after = before.clone();
    colourway(&mut after, 120);
    let services = BuildServices::default();
    let mut recoloured = hair_output(&before).unwrap();
    let rebuilt = hair_output(&after).unwrap();
    assert_ne!(exact(&recoloured), exact(&rebuilt), "the palette really changes the output");
    let done = recolour_part(&after, &pid("hair"), &services, &mut recoloured);
    assert_eq!(done, Ok(Recoloured::Done));
    assert_eq!(exact(&recoloured), exact(&rebuilt));
}

#[test]
fn streaks_land_on_the_tubes_only_and_leave_shell_vertices_white() {
    let mut def = character(&streaked());
    colourway(&mut def, 40);
    let out = hair_output(&def).unwrap();
    let stat = static_mesh(&out);
    let tube_vertices = 6 * 4 * RING;
    assert_eq!(stat.geometry.positions.len(), tube_vertices + 3);
    assert!(stat.geometry.colours[tube_vertices..].iter().all(|c| *c == [1.0; 4]), "the cap shell is untinted");
    assert!(stat.geometry.colours[..tube_vertices].iter().any(|c| *c != [1.0; 4]), "some tube is streaked");
}

#[test]
fn a_mesh_that_is_not_what_build_makes_asks_for_a_rebuild() {
    let mut def = character(&streaked());
    colourway(&mut def, 40);
    let mut out = hair_output(&def).unwrap();
    out.meshes[0].geometry.colours.pop();
    assert_eq!(recolour_part(&def, &pid("hair"), &BuildServices::default(), &mut out), Ok(Recoloured::NeedsRebuild));
}

#[test]
fn the_character_builder_builds_the_stub_hair_with_the_head_and_the_body() {
    let def = character(&two_group_style());
    let built = RegistryBuilder
        .build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap();
    assert!(built.meshes.iter().any(|m| m.mesh.name == "hair-static"));
}

#[test]
fn a_missing_tube_mesh_asks_for_a_rebuild_instead_of_claiming_done() {
    let mut def = character(&streaked());
    colourway(&mut def, 40);
    let services = BuildServices::default();
    let built = || hair_output(&def).unwrap();

    let mut no_dynamic = built();
    no_dynamic.meshes.retain(|m| !m.dynamic);
    assert_eq!(recolour_part(&def, &pid("hair"), &services, &mut no_dynamic), Ok(Recoloured::NeedsRebuild));

    let mut no_static = built();
    no_static.meshes.retain(|m| m.dynamic);
    assert_eq!(recolour_part(&def, &pid("hair"), &services, &mut no_static), Ok(Recoloured::NeedsRebuild));

    let mut none = built();
    none.meshes.clear();
    assert_eq!(recolour_part(&def, &pid("hair"), &services, &mut none), Ok(Recoloured::NeedsRebuild));
}
