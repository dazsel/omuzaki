//! The build pipeline as public pieces ([`plan`], [`build_part`], [`part_rng`], [`Assembler`]): another builder (the caching
//! one in `omuzaki-char`) reuses them, so they must compose into exactly what `RegistryBuilder` does, report the same
//! errors, and give every part the same random stream however it is built.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use omuzaki_char_api::{
    Assembler, BuildCtx, BuildError, BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder,
    CharacterDef, FitFact, MaterialName, MaterialSpec, MeshData, PartCategory, PartDef, PartId, PartKind, PartKindId,
    PartMesh, PartMeta, PartOutput, Published, RegistryBuilder, build_part, part_rng, plan,
};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_face_api::{FaceProfile, Palette};
use serde_json::json;

// ---- a kind that reports the random numbers it was given

/// Reports the first number of its stream as a warning, so a test sees exactly what the pipeline handed the part.
struct Probe;

impl PartKind for Probe {
    type Params = common::NoParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "pipeline-probe",
            summary: "Report the first random number drawn.",
            category: PartCategory::Accessory,
            requires: &[],
        }
    }

    fn build(_: &common::NoParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        Ok(PartOutput { warnings: vec![draw(&mut ctx.rng)], ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(Probe);

fn draw(rng: &mut omuzaki_core::rng::Rng) -> String {
    format!("{:016x}", rng.next_u64())
}

// ---- helpers

fn id(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

fn probe(layer: i32) -> PartDef {
    PartDef {
        kind: PartKindId::new("pipeline-probe").unwrap(),
        parent: None,
        enabled: true,
        layer,
        attach: None,
        params: json!({}),
    }
}

/// A body (for the skeleton) and the given probes.
fn character(seed: u32, probes: &[(&str, i32)]) -> CharacterDef {
    let mut parts = BTreeMap::from([(id("body"), common::body_part())]);
    parts.extend(probes.iter().map(|(name, layer)| (id(name), probe(*layer))));
    CharacterDef {
        name: "Pipeline".to_owned(),
        template: omuzaki_char_api::BodyTemplateId::new("soft-sd").unwrap(),
        seed,
        palette: Palette::default(),
        skin_tone: None,
        parts,
        face: FaceProfile::default(),
        weight_brushes: BTreeMap::new(),
    }
}

fn char_id() -> CharId {
    CharId::new("pipeline").unwrap()
}

fn registry(def: &CharacterDef, options: &BuildOptions) -> Result<BuiltCharacter, BuildError> {
    RegistryBuilder.build(&char_id(), def, options, &BuildServices::default())
}

/// The pipeline driven by hand, the way a caching builder drives it.
fn by_hand(def: &CharacterDef, options: &BuildOptions) -> Result<BuiltCharacter, BuildError> {
    let id = char_id();
    let services = BuildServices::default();
    let mut assembler = Assembler::new(&id, def, options);
    for part in plan(def)? {
        let output = build_part(def, &part, options, &services, assembler.published())?;
        assembler.add(&part, output)?;
    }
    assembler.finish()
}

fn triangle(name: &str, material: &str) -> PartMesh {
    PartMesh {
        name: name.to_owned(),
        geometry: MeshData {
            positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
            normals: vec![Vec3::Z; 3],
            uvs: vec![Vec2::ZERO; 3],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        },
        joint_bones: Vec::new(),
        material: MaterialName::new(material).unwrap(),
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    }
}

fn material(name: &str) -> (MaterialName, MaterialSpec) {
    (MaterialName::new(name).unwrap(), MaterialSpec::toon("#808080".parse().unwrap()))
}

/// The body's output, which every assembly needs for a skeleton.
fn body_output(def: &CharacterDef) -> PartOutput {
    let options = BuildOptions::default();
    build_part(def, &id("body"), &options, &BuildServices::default(), &Published::default()).unwrap()
}

// ---- the random stream

#[test]
fn a_parts_stream_is_pinned() {
    // Computed by an independent SplitMix64 + FNV-1a implementation following the derivation in `part_rng`'s docs, so
    // this fails if the code or the documented contract changes. A change reshuffles every procedural part.
    for (seed, part, expected) in [
        (0, "a", [0x451c_f8f9_1a93_d034, 0xd747_65b3_5d5e_656c]),
        (7, "hair", [0x0532_b0fe_ec32_b985, 0x9221_b918_ad6a_902e]),
        (42, "head", [0x4590_def8_881c_e38d, 0x52d4_2461_6da0_2895]),
        (u32::MAX, "shirt", [0x553c_6212_dbe8_ec3d, 0x7d56_255f_e7cb_76d9]),
    ] {
        let mut rng = part_rng(seed, &id(part));
        assert_eq!([rng.next_u64(), rng.next_u64()], expected, "seed {seed}, part '{part}'");
    }
}

#[test]
fn a_stream_depends_on_the_seed_and_the_id_only() {
    let first = |seed, part: &str| part_rng(seed, &id(part)).next_u64();
    assert_eq!(first(5, "hair"), first(5, "hair"), "each call starts the stream again");
    assert_ne!(first(5, "hair"), first(6, "hair"));
    assert_ne!(first(5, "hair"), first(5, "hair2"));
}

#[test]
fn a_part_is_given_its_stream_by_every_way_of_building_it() {
    let def = character(31, &[("a", 10), ("b", 20)]);
    let expect = |part: &str| draw(&mut part_rng(def.seed, &id(part)));
    let options = BuildOptions::default();

    for built in [registry(&def, &options).unwrap(), by_hand(&def, &options).unwrap()] {
        for report in built.report.parts.iter().filter(|p| p.kind.as_str() == "pipeline-probe") {
            assert_eq!(report.warnings, [expect(report.part.as_str())], "{}", report.part);
        }
    }
    // Alone, with no other part built and nothing published: the same numbers, so a cache can rebuild one part.
    let alone = build_part(&def, &id("b"), &options, &BuildServices::default(), &Published::default()).unwrap();
    assert_eq!(alone.warnings, [expect("b")]);
    // Adding a part or dropping one leaves the rest alone.
    let more = character(31, &[("a", 10), ("b", 20), ("c", 5)]);
    let fewer = character(31, &[("b", 20)]);
    for other in [more, fewer] {
        let built = registry(&other, &options).unwrap();
        let b = built.report.parts.iter().find(|p| p.part == id("b")).unwrap();
        assert_eq!(b.warnings, [expect("b")]);
    }
}

// ---- the pieces compose into RegistryBuilder

#[test]
fn driving_the_pieces_by_hand_equals_the_registry_builder() {
    let def = character(9, &[("a", 10), ("b", 20), ("c", 20)]);
    for options in [
        BuildOptions::default(),
        BuildOptions { quality: omuzaki_char_api::Quality::Offline, rig_export: true, ..BuildOptions::default() },
    ] {
        let (a, b) = (registry(&def, &options).unwrap(), by_hand(&def, &options).unwrap());
        assert_eq!(a.id, b.id);
        assert_eq!(a.def, b.def);
        assert_eq!(a.palette, b.palette);
        assert_eq!(a.meshes, b.meshes);
        assert_eq!(a.materials, b.materials);
        assert_eq!(a.textures, b.textures);
        assert_eq!(a.anchors, b.anchors);
        assert_eq!(a.rig, b.rig);
        assert_eq!(a.report, b.report);
        assert_eq!(a.report.parts.len(), 4, "body and three probes");
    }
}

#[test]
fn the_plan_is_the_build_order() {
    let def = character(1, &[("late", 30), ("early", 5), ("mid", 20)]);
    let order: Vec<_> = plan(&def).unwrap().iter().map(|p| p.as_str().to_owned()).collect();
    assert_eq!(order, ["body", "early", "mid", "late"], "inner layers first, ties by id");
    let built = registry(&def, &BuildOptions::default()).unwrap();
    let built_order: Vec<_> = built.report.parts.iter().map(|p| p.part.as_str().to_owned()).collect();
    assert_eq!(order, built_order);

    let mut disabled = def;
    disabled.parts.get_mut(&id("mid")).unwrap().enabled = false;
    assert_eq!(plan(&disabled).unwrap().len(), 3, "disabled parts are left out");
}

#[test]
fn the_plan_and_the_part_build_report_what_the_registry_builder_reports() {
    let options = BuildOptions::default();
    let same_error = |def: &CharacterDef| {
        let (a, b) = (registry(def, &options).unwrap_err(), by_hand(def, &options).unwrap_err());
        assert_eq!(a, b);
        a
    };

    let mut bad_face = character(1, &[]);
    bad_face.face.layout.eye_spacing = 5.0;
    assert!(matches!(same_error(&bad_face), BuildError::Face(_)));

    let mut ghost_brush = character(1, &[]);
    ghost_brush.weight_brushes.insert(id("ghost"), Vec::new());
    assert!(matches!(same_error(&ghost_brush), BuildError::Brush { .. }));

    let mut loop_ = character(1, &[("a", 0), ("b", 0)]);
    loop_.parts.get_mut(&id("a")).unwrap().parent = Some(id("b"));
    loop_.parts.get_mut(&id("b")).unwrap().parent = Some(id("a"));
    assert!(matches!(same_error(&loop_), BuildError::Tree(_)));

    // `plan` leaves kinds to `build_part`, so an unknown kind surfaces when its turn comes in both builders.
    let mut unknown = character(1, &[("a", 10)]);
    unknown.parts.get_mut(&id("a")).unwrap().kind = PartKindId::new("no-such-kind").unwrap();
    assert!(plan(&unknown).is_ok());
    assert!(matches!(same_error(&unknown), BuildError::UnknownKind { ref part, .. } if part.as_str() == "a"));

    let mut bad_params = character(1, &[("a", 10)]);
    bad_params.parts.get_mut(&id("a")).unwrap().params = json!({"nope": 1});
    assert!(matches!(same_error(&bad_params), BuildError::Params { ref part, .. } if part.as_str() == "a"));
}

// ---- the assembler on its own

#[test]
fn the_assembler_rejects_a_part_the_character_lacks_or_one_added_twice() {
    let def = character(1, &[("a", 10)]);
    let (cid, options) = (char_id(), BuildOptions::default());

    let mut assembler = Assembler::new(&cid, &def, &options);
    let err = assembler.add(&id("ghost"), PartOutput::default()).unwrap_err();
    assert!(matches!(err, BuildError::Part { ref part, .. } if part.as_str() == "ghost"), "{err}");
    let err = build_part(&def, &id("ghost"), &options, &BuildServices::default(), assembler.published()).unwrap_err();
    assert!(matches!(err, BuildError::Part { ref part, .. } if part.as_str() == "ghost"), "{err}");

    let mut assembler = Assembler::new(&cid, &def, &options);
    assembler.add(&id("a"), PartOutput::default()).unwrap();
    let err = assembler.add(&id("a"), PartOutput::default()).unwrap_err();
    assert!(matches!(err, BuildError::Part { ref message, .. } if message.contains("twice")), "{err}");
}

#[test]
fn the_assembler_validates_what_it_is_given_in_the_registry_builders_order() {
    let def = character(1, &[("a", 10), ("b", 20)]);
    let (cid, options) = (char_id(), BuildOptions::default());
    let new = || Assembler::new(&cid, &def, &options);

    // A broken mesh is named before the (also broken) material that follows it.
    let mut broken = triangle("oops", "m");
    broken.geometry.normals.pop();
    let output =
        PartOutput { meshes: vec![broken], materials: vec![material("m"), material("m")], ..PartOutput::default() };
    assert!(matches!(new().add(&id("a"), output).unwrap_err(), BuildError::Mesh { ref mesh, .. } if mesh == "oops"));

    // A material another part defined.
    let mut assembler = new();
    let define = || PartOutput { materials: vec![material("shared")], ..PartOutput::default() };
    assembler.add(&id("a"), define()).unwrap();
    let err = assembler.add(&id("b"), define()).unwrap_err();
    assert!(matches!(err, BuildError::Duplicate { what: "material", ref id } if id == "shared"), "{err}");

    // A mesh naming a material nobody defines is found at the end, once all parts are in.
    let mut assembler = new();
    assembler.add(&id("body"), body_output(&def)).unwrap();
    let orphan = PartOutput { meshes: vec![triangle("orphan", "ghost")], ..PartOutput::default() };
    assembler.add(&id("a"), orphan).unwrap();
    let err = assembler.finish().unwrap_err();
    assert!(matches!(err, BuildError::UnknownMaterial { ref material, .. } if material.as_str() == "ghost"), "{err}");

    // With nothing published there is no skeleton.
    assert!(matches!(new().finish().unwrap_err(), BuildError::Skeleton(_)));
}

#[test]
fn the_assembler_records_what_parts_publish_for_the_next_part() {
    let def = character(1, &[("a", 10)]);
    let (cid, options) = (char_id(), BuildOptions::default());
    let mut assembler = Assembler::new(&cid, &def, &options);
    assert!(assembler.published().get::<FitFact>().fits.is_empty());
    assembler.add(&id("body"), body_output(&def)).unwrap();
    assert_eq!(
        assembler.published().get::<FitFact>().fits.len(),
        1,
        "the body's fit is what the next part builds against"
    );
}
