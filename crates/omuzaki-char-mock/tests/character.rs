//! The mock character through the whole contract: fixtures, build order, layering, variants, simulation and the frame loop.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{Channel, ChannelBuf, HumanoidBone, Pose};
use omuzaki_char_api::{
    BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, HAIR_PROGRAM,
    MaterialName, MeshData, PartKindId, PartRuntime, RegistryBuilder, RuntimeCost, StepCtx, VariantId, find_kind,
    resolve_variant, select_variant,
};
use omuzaki_char_mock::{MockLive, RestPose, mock_character, mock_variants};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_face_api::{FaceLayerId, FacePainter};
use omuzaki_face_mock::MockPainter;
use omuzaki_render_api::Shading;
use serde_json::json;

fn build(def: &CharacterDef) -> Result<BuiltCharacter, BuildError> {
    RegistryBuilder.build(&CharId::new("mock").unwrap(), def, &BuildOptions::default(), &BuildServices::default())
}

fn base() -> BuiltCharacter {
    build(&mock_character().unwrap()).unwrap()
}

fn close_up() -> BuiltCharacter {
    let (def, variants) = (mock_character().unwrap(), mock_variants().unwrap());
    let id = VariantId::new("close-up").unwrap();
    RegistryBuilder
        .build(
            &CharId::new("mock").unwrap(),
            &resolve_variant(&def, &variants, &id).unwrap(),
            &BuildOptions { variant: Some(id), ..BuildOptions::default() },
            &BuildServices::default(),
        )
        .unwrap()
}

fn mesh<'a>(c: &'a BuiltCharacter, name: &str) -> &'a MeshData {
    &c.meshes.iter().find(|m| m.mesh.name == name).unwrap_or_else(|| panic!("no mesh {name}")).mesh.geometry
}

fn max_abs_x(m: &MeshData) -> f32 {
    m.positions.iter().map(|p| p.x.abs()).fold(0.0, f32::max)
}

#[test]
fn the_fixtures_match_the_types_and_every_part_validates_through_its_kind() {
    let def = mock_character().unwrap();
    assert_eq!(def.parts.len(), 5);
    for (id, part) in &def.parts {
        let reg = find_kind(part.kind.as_str()).unwrap_or_else(|| panic!("{id}: kind {} isn't registered", part.kind));
        (reg.check)(&part.params).unwrap_or_else(|e| panic!("{id}: {e}"));
    }
    assert_eq!(mock_variants().unwrap().len(), 2);
    assert!(PartKindId::new("mock-hair").is_ok());
}

#[test]
fn it_builds_in_layer_order_with_pinned_geometry_counts() {
    let c = base();
    let order: Vec<_> = c.report.parts.iter().map(|p| p.part.to_string()).collect();
    assert_eq!(
        order,
        ["body", "head", "hair", "shirt", "vest"],
        "layer 0 first (hair after the head it needs), then the garments"
    );
    let counts: Vec<_> = c.report.parts.iter().map(|p| (p.vertices, p.triangles)).collect();
    // torso tube: 6 rings x 10; head: 11 x 17; curtain ribbons: 44 clumps x 6 points x 2 vertices.
    assert_eq!(counts, [(60, 100), (187, 320), (528, 440), (60, 100), (60, 100)]);
    assert!(c.report.parts[2].warnings.iter().any(|w| w.contains("a-cap")), "the mock skips what it doesn't know");
    assert_eq!((c.report.vertices(), c.report.triangles()), (895, 1060));
}

#[test]
fn a_rebuild_is_identical() {
    let (a, b) = (base(), base());
    assert_eq!(a.meshes.len(), b.meshes.len());
    for (x, y) in a.meshes.iter().zip(&b.meshes) {
        assert_eq!(x.mesh, y.mesh, "{}", x.mesh.name);
    }
}

#[test]
fn garments_wrap_what_is_inside_them_layer_by_layer() {
    let c = base();
    let (body, shirt, vest) = (max_abs_x(mesh(&c, "torso")), max_abs_x(mesh(&c, "shirt")), max_abs_x(mesh(&c, "vest")));
    // Ten segments round the torso: the widest vertex sits at sin(72 degrees) = 0.951 of the radius, so the ease shows
    // at that fraction in the vertex positions.
    let at_vertex = 72.0_f32.to_radians().sin();
    assert!((shirt - body - 0.004 * at_vertex).abs() < 1e-5, "the shirt stands 4 mm off the body: {body} {shirt}");
    assert!((vest - shirt - 0.006 * at_vertex).abs() < 1e-5, "and the vest 6 mm off the shirt: {shirt} {vest}");
}

#[test]
fn triangles_face_outward() {
    let c = base();
    let centre = Vec3::new(0.0, 0.78, -0.02);
    for (name, around) in [("head", Some(centre)), ("torso", None), ("shirt", None)] {
        let g = mesh(&c, name);
        let mut checked = 0;
        for t in g.indices.chunks(3) {
            let p: Vec<Vec3> = t.iter().map(|i| g.positions[*i as usize]).collect();
            let n = (p[1] - p[0]).cross(p[2] - p[0]);
            if n.length() < 1e-6 {
                continue; // a sliver at a pole
            }
            let mid = (p[0] + p[1] + p[2]) / 3.0;
            let outward = around.map_or(Vec3::new(mid.x, 0.0, mid.z), |c| mid - c);
            assert!(
                n.dot(outward) > 0.0,
                "{name}: a triangle faces inward: indices {t:?} positions {p:?} n {n:?} outward {outward:?}"
            );
            checked += 1;
        }
        assert!(checked > 50, "{name}: {checked}");
    }
}

#[test]
fn the_head_carries_the_face_and_the_hair_carries_the_brows() {
    let c = base();
    let overlay = |material: &str| c.materials[&MaterialName::new(material).unwrap()].overlays.clone();
    let (head, hair) = (overlay("mock-head-skin"), overlay("mock-hair"));
    assert_eq!((head.len(), head[0].layer, head[0].strength), (1, FaceLayerId::Base, 1.0));
    assert_eq!((hair.len(), hair[0].layer, hair[0].strength), (1, FaceLayerId::Brows, 0.85));
    assert_eq!(head[0].window.min, hair[0].window.min, "brows line up with the face");
    // The window sits on the head: centre y 0.78, so the face rectangle's top edge is at 0.78 + 0.105.
    assert!((head[0].window.max.y - 0.885).abs() < 1e-5);
    let Shading::Program { name, params } = &c.materials[&MaterialName::new("mock-hair").unwrap()].shading else {
        panic!("hair uses a program")
    };
    assert_eq!((name.as_str(), params.len()), (HAIR_PROGRAM, 17));
    let strands = &mesh(&c, "hair-dynamic").extra;
    let attributes: Vec<_> = strands.iter().map(|a| (a.name.as_str(), a.components)).collect();
    assert_eq!(attributes, [("hair-uv", 2), ("hair-info", 3)]);
    assert!(
        !c.materials[&MaterialName::new("mock-head-skin").unwrap()].receives_shadows,
        "faces stay clean under hair shadows"
    );
}

#[test]
fn the_close_up_variant_changes_the_face_the_hair_and_the_layout() {
    let (game, close) = (base(), close_up());
    assert_eq!(close.variant, Some(VariantId::new("close-up").unwrap()));
    let (g, c) = (game.face.as_ref().unwrap(), close.face.as_ref().unwrap());
    assert!(c.projection.rect.y0 < g.projection.rect.y0 - 0.05, "a longer chin gives a taller face window");
    assert_eq!(c.profile.layout.mouth_y, -0.2);
    assert_eq!(c.profile.mapping.rest.eye_open, 1.3);
    assert_eq!(g.profile.mapping.rest.eye_open, 1.0);
    let names = |b: &BuiltCharacter| b.meshes.iter().map(|m| m.mesh.name.clone()).collect::<Vec<_>>();
    assert!(
        !names(&game).contains(&"hair-static".to_owned()) && names(&close).contains(&"hair-static".to_owned()),
        "the fringe exists only in the close-up"
    );
    assert_eq!(mesh(&close, "hair-static").positions.len(), 9 * 6 * 2);
}

#[test]
fn the_game_variant_equals_the_base_and_shots_select_variants() {
    let (def, variants) = (mock_character().unwrap(), mock_variants().unwrap());
    assert_eq!(
        resolve_variant(&def, &variants, &VariantId::new("game").unwrap()).unwrap(),
        def,
        "no overrides: identical"
    );
    let pick = |coverage: f64| select_variant(&variants, coverage, None).map(VariantId::to_string);
    assert_eq!((pick(0.1).as_deref(), pick(0.5).as_deref()), (Some("game"), Some("close-up")));
}

#[test]
fn bad_edits_fail_at_the_right_place_with_the_right_path() {
    let (def, variants) = (mock_character().unwrap(), mock_variants().unwrap());
    // A part parameter out of range: resolves (the typed check is the kind's job), fails at build naming the field.
    let mut bad = def.clone();
    bad.parts.get_mut(&"hair".parse().unwrap()).unwrap().params = json!({"physics": {"iterations": 99}});
    match build(&bad).unwrap_err() {
        BuildError::Params { part, errors } => {
            assert_eq!((part.as_str(), errors.0[0].path.as_str()), ("hair", "physics/iterations"));
        }
        other => panic!("{other}"),
    }
    // A typo in a typed field of the character: caught when the variant is resolved, naming the override.
    let mut typo = variants.clone();
    typo.get_mut(&VariantId::new("close-up").unwrap())
        .unwrap()
        .overrides
        .insert(omuzaki_doc_api::DocPath::parse("face/layoutt/mouth_y").unwrap(), json!(0.0));
    let e = resolve_variant(&def, &typo, &VariantId::new("close-up").unwrap()).unwrap_err().to_string();
    assert!(e.contains("face/layoutt/mouth_y") && e.contains("layoutt"), "{e}");
    // An out-of-range face value: caught by the build.
    let mut face = def.clone();
    face.face.layout.eye_spacing = 5.0;
    assert!(matches!(build(&face).unwrap_err(), BuildError::Face(_)));
}

#[test]
fn the_hair_simulation_moves_deterministically_and_resets() {
    let mut a = base();
    let mut b = base();
    let slot = a.runtimes.iter().position(|s| s.part.as_str() == "hair").expect("the curtain is simulated");
    assert_eq!(a.runtimes.len(), 1);
    let cost = a.runtimes[slot].runtime.cost();
    assert_eq!((cost.vertices, cost.particles), (528, 264));
    let rest: Vec<Vec3> = a.runtimes[slot].runtime.dynamic_mesh(0).unwrap().positions.to_vec();
    let step = |built: &mut BuiltCharacter, secs: f64| {
        let ctx = omuzaki_char_api::StepCtx::new(1.0 / 60.0, omuzaki_core::time::Seconds(secs), &RestPose, Vec3::ZERO);
        built.runtimes[0].runtime.step(&ctx);
    };
    for built in [&mut a, &mut b] {
        step(built, 0.4);
        step(built, 0.5);
    }
    let (ma, mb) = (a.runtimes[0].runtime.dynamic_mesh(0).unwrap(), b.runtimes[0].runtime.dynamic_mesh(0).unwrap());
    assert_eq!(
        (ma.revision, ma.mesh),
        (2, 0),
        "two steps; the dynamic mesh is the part's only one (no fringe in the base)"
    );
    assert_eq!(ma.positions, mb.positions, "same inputs, same hair");
    let moved: f32 = ma.positions.iter().zip(&rest).map(|(p, r)| p.distance(*r)).fold(0.0, f32::max);
    assert!(moved > 0.005 && moved < 0.05, "tips sway by a centimetre or so: {moved}");
    let root_move = ma.positions.iter().zip(&rest).take(2).map(|(p, r)| p.distance(*r)).fold(0.0, f32::max);
    assert!(root_move < 1e-6, "the root stays put (stiffness 1 at the root)");
    a.runtimes[0].runtime.reset(&RestPose);
    let after = a.runtimes[0].runtime.dynamic_mesh(0).unwrap();
    assert_eq!(
        (after.positions, after.revision),
        (rest.as_slice(), 3),
        "reset restores the rest shape and still bumps the revision"
    );
}

/// One dynamic mesh as a stage sees it, as bit patterns (equal bits, not merely close numbers).
#[derive(Debug, PartialEq)]
struct MeshBits {
    positions: Vec<[u32; 3]>,
    normals: Vec<[u32; 3]>,
    revision: u64,
}

/// A simulation as a stage sees it, bit for bit: its dynamic meshes and its cost.
#[derive(Debug, PartialEq)]
struct SimState {
    meshes: Vec<MeshBits>,
    cost: RuntimeCost,
}

fn sim_state(runtime: &dyn PartRuntime) -> SimState {
    let bits = |v: &[Vec3]| v.iter().map(|p| p.to_array().map(f32::to_bits)).collect::<Vec<_>>();
    let meshes = (0..runtime.dynamic_count())
        .filter_map(|i| runtime.dynamic_mesh(i))
        .map(|m| MeshBits { positions: bits(m.positions), normals: bits(m.normals), revision: m.revision })
        .collect();
    SimState { meshes, cost: runtime.cost() }
}

fn windy_frames(runtime: &mut dyn PartRuntime, frames: u32) {
    for frame in 0..frames {
        let ctx = StepCtx::new(
            1.0 / 60.0,
            omuzaki_core::time::Seconds(f64::from(frame) / 60.0),
            &RestPose,
            Vec3::new(1.0, 0.0, 0.5),
        );
        runtime.step(&ctx);
    }
}

#[test]
fn a_forked_hair_simulation_is_the_freshly_built_one() {
    let (mut original, fresh) = (base(), base());
    let mut forked = original.runtimes[0].runtime.fork().expect("the mock hair can be copied");
    let pristine = sim_state(fresh.runtimes[0].runtime.as_ref());
    assert_eq!(pristine.meshes[0].positions.len(), 528, "a real simulation, not an empty one");
    assert_eq!(sim_state(forked.as_ref()), pristine, "an unstepped fork is bit-identical to a fresh build");

    // The original and the fork live separately, and the fork behaves as a fresh build does under the same frames.
    windy_frames(original.runtimes[0].runtime.as_mut(), 40);
    assert_ne!(sim_state(original.runtimes[0].runtime.as_ref()), pristine, "the original moved");
    assert_eq!(sim_state(forked.as_ref()), pristine, "and the fork did not");
    // A fork is the initial state whenever it is taken: after 40 frames, and after a reset (which bumps the revision).
    let late = original.runtimes[0].runtime.fork().expect("the mock hair can be copied");
    assert_eq!(sim_state(late.as_ref()), pristine, "forked from a stepped runtime: initial, not current");
    original.runtimes[0].runtime.reset(&RestPose);
    let after_reset = original.runtimes[0].runtime.fork().expect("the mock hair can be copied");
    assert_eq!(sim_state(after_reset.as_ref()), pristine, "forked after a reset: revision 0 again");
    // The runtime counts what it holds: rest, positions and normals (3 x 528 x 12 bytes), strand stiffness and phase.
    assert_eq!(original.runtimes[0].runtime.heap_bytes(), 3 * 528 * 12 + 2 * 528 * 4);
    let mut reference = base();
    windy_frames(reference.runtimes[0].runtime.as_mut(), 25);
    windy_frames(forked.as_mut(), 25);
    assert_eq!(
        sim_state(forked.as_ref()),
        sim_state(reference.runtimes[0].runtime.as_ref()),
        "stepped alike, they stay alike to the bit"
    );
}

#[test]
fn the_frame_loop_repaints_only_the_layers_that_changed() {
    let built = base();
    let painter = Box::new(MockPainter::new(built.face.as_ref().unwrap().projection));
    assert_eq!(painter.layers().len(), 2);
    let mut live = MockLive::new(built, painter);
    let mut frame = ChannelBuf::NEUTRAL;
    let first = live.tick(1.0 / 60.0, &RestPose, &frame).unwrap();
    assert_eq!(first.face_changed, [true; 3], "the first tick paints everything");
    let still = live.tick(1.0 / 60.0, &RestPose, &frame).unwrap();
    assert_eq!(still.face_changed, [false; 3], "an unchanged face costs nothing");
    assert!(still.revisions[0] > first.revisions[0], "the hair keeps moving");
    frame[Channel::JAW_OPEN] = 0.7;
    let talking = live.tick(1.0 / 60.0, &RestPose, &frame).unwrap();
    assert_eq!(talking.face_changed, [true, false, false], "a mouth moves the base layer, not the brows");
    frame[Channel::BROW_INNER_UP] = 0.8;
    let raised = live.tick(1.0 / 60.0, &RestPose, &frame).unwrap();
    assert_eq!(raised.face_changed, [true, true, false], "brows are on both layers");
    assert!(!live.drawing()[FaceLayerId::Base].is_empty());
}

#[test]
fn the_mock_builds_a_valid_rig_that_is_the_same_every_time() {
    let (a, b) = (base(), base());
    let rig = &a.rig;
    assert_eq!(rig.skeleton.len(), 20, "15 required humanoid bones + chest + neck + 3 hair bones");
    assert_eq!(a.rig, b.rig, "the skeleton and springs are identical across two builds");
    assert_eq!(close_up().rig, a.rig, "variants change faces and hair, not the rig");
    for bone in HumanoidBone::ALL.into_iter().filter(|b| b.is_required()) {
        assert!(rig.skeleton.humanoid_bone(bone).is_some(), "{bone}");
    }
    let hair = rig.skeleton.find("hair_hair_2").expect("the hair part added its bones");
    let head = rig.skeleton.humanoid_bone(HumanoidBone::Head).unwrap();
    assert!(rig.skeleton.is_ancestor_or_self(head, hair), "the chain hangs from the head");

    // One chain, colliding with the body's head-and-neck group; the group's bones are the rebased head and neck.
    assert_eq!(rig.springs.chains.len(), 1);
    assert_eq!(rig.springs.chains[0].collider_groups, ["head-neck"]);
    assert_eq!(rig.springs.chains[0].center, Some(head));
    let group = &rig.springs.groups[0];
    let bones: Vec<_> = group.colliders.iter().map(|c| rig.skeleton.get(c.bone).unwrap().name.as_str()).collect();
    assert_eq!(bones, ["head", "neck"]);
    rig.springs.validate(&rig.skeleton).unwrap();
}

#[test]
fn at_rest_every_skinning_matrix_is_the_identity() {
    let rig = base().rig;
    let skeleton = &rig.skeleton;
    let (inverse_bind, mut world) = (skeleton.inverse_bind(), Pose::IDENTITY);
    let mut skin = vec![Mat4::ZERO; skeleton.len()];
    skeleton.skin_matrices(&skeleton.rest_pose(), &inverse_bind, &mut world, &mut skin);
    assert!(skin.iter().all(|m| m.abs_diff_eq(Mat4::IDENTITY, 1e-6)));
}

#[test]
fn every_bone_a_mock_mesh_is_skinned_to_is_in_the_rig() {
    let c = base();
    for m in &c.meshes {
        for joint in &m.mesh.joint_bones {
            assert!(c.rig.bone(joint).is_some(), "{} uses {joint:?}", m.mesh.name);
        }
    }
}

#[test]
fn hair_info_carries_the_polar_angle_from_the_top_of_the_head() {
    // `HAIR_INFO`'s third float is the angle from +Y to the direction from the head centre (the prototype's
    // `acos(dir.y)`): the curtain's roots are guided to 100 degrees from the top (a little below the equator) and the
    // hair hangs down, so the angle only grows along a clump. The ring of the hair program sits at 0.92 rad from the top.
    let c = base();
    let hair = mesh(&c, "hair-dynamic");
    let (uv, info) = (&hair.extra[0].data, &hair.extra[1].data);
    let mut roots = 0;
    for (vertex, (uv, info)) in uv.chunks(2).zip(info.chunks(3)).enumerate() {
        let (along, polar) = (uv[1], info[2]);
        if along == 0.0 {
            roots += 1;
            assert!(
                (polar - 100.0_f32.to_radians()).abs() < 0.02,
                "vertex {vertex}: a root is at {polar} rad, not 100 degrees"
            );
        } else {
            assert!(
                polar >= 100.0_f32.to_radians() - 0.02,
                "vertex {vertex}: the angle shrank to {polar} rad down the clump"
            );
        }
    }
    assert!(roots > 0);
}
