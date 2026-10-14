//! End-to-end check of the part-kind contract: register kinds in their own "files", find them, validate parameters
//! through their schema, and build a character through the reference builder.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_char_api::{
    BuildCtx, BuildError, BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, FaceOverlay,
    HeadSurface, MaterialSpec, MeshData, PartCategory, PartDef, PartId, PartKind, PartKindId, PartMesh, PartMeta,
    PartOutput, Publishes, RegistryBuilder, find_kind, kinds,
};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_face_api::{Check, FaceLayerId, FaceProfile, FaceProjection, FaceRect, Palette, Validate, narrow};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ---- three tiny kinds, each in the shape a real generator file would have

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct BallParams {
    /// Radius in metres.
    radius: f64,
}

impl Default for BallParams {
    fn default() -> Self {
        Self { radius: 0.1 }
    }
}

impl Validate for BallParams {
    fn check(&self, c: &mut Check) {
        c.range("radius", self.radius, 0.01, 1.0);
    }
}

fn material(base: &str, overlays: Vec<FaceOverlay>) -> MaterialSpec {
    let mut spec = MaterialSpec::toon(base.parse().unwrap());
    spec.overlays = overlays;
    spec
}

fn mesh(name: &str, material: &str, offset: Vec3) -> PartMesh {
    PartMesh {
        name: name.to_owned(),
        geometry: MeshData {
            positions: vec![offset, offset + Vec3::X, offset + Vec3::Y],
            normals: vec![Vec3::Z; 3],
            uvs: vec![Vec2::ZERO; 3],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        },
        joint_bones: Vec::new(),
        material: omuzaki_char_api::MaterialName::new(material).unwrap(),
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    }
}

#[derive(Debug)]
struct Ball(f32);

impl HeadSurface for Ball {
    fn centre(&self) -> Vec3 {
        Vec3::new(0.0, 1.0, 0.0)
    }

    fn radius(&self, _: Vec3) -> f32 {
        self.0
    }
}

struct TestHead;

impl PartKind for TestHead {
    type Params = BallParams;

    fn meta() -> PartMeta {
        PartMeta { id: "test-head", summary: "Build a ball head.", category: PartCategory::Head, requires: &[] }
    }

    fn build(params: &BallParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let projection =
            FaceProjection { rect: FaceRect { x0: -0.1, x1: 0.1, y0: -0.1, y1: 0.1 }, front_fade: [0.0, 0.05] };
        // The head publishes the projection, so it builds its own Base overlay.
        let overlay = FaceOverlay::of(&projection, Ball(narrow(params.radius)).centre(), FaceLayerId::Base);
        Ok(PartOutput {
            meshes: vec![mesh("head", "test-head-skin", Vec3::ZERO)],
            materials: vec![(
                omuzaki_char_api::MaterialName::new("test-head-skin").unwrap(),
                material("#f9d3c5", vec![overlay]),
            )],
            publishes: Publishes {
                head: Some(Arc::new(Ball(narrow(params.radius)))),
                face: Some(projection),
                ..Publishes::default()
            },
            warnings: vec![format!("built by {}", ctx.part)],
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(TestHead);

struct TestHair;

impl PartKind for TestHair {
    type Params = BallParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-hair",
            summary: "Build hair on the head.",
            category: PartCategory::Hair,
            requires: &["test-head"],
        }
    }

    fn build(_: &BallParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let jitter = ctx.rng.next_f32();
        let root = head.point(Vec3::Y, 0.01) + Vec3::new(jitter, 0.0, 0.0);
        let brows = ctx.face_overlay(FaceLayerId::Brows)?;
        Ok(PartOutput {
            meshes: vec![mesh("strand", "test-hair", root)],
            materials: vec![(
                omuzaki_char_api::MaterialName::new("test-hair").unwrap(),
                material("#e6d2be", vec![brows]),
            )],
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(TestHair);

/// Uses the random stream without needing a head: proves a part's numbers don't depend on other parts.
struct TestNoise;

impl PartKind for TestNoise {
    type Params = BallParams;

    fn meta() -> PartMeta {
        PartMeta { id: "test-noise", summary: "Draw random numbers.", category: PartCategory::Accessory, requires: &[] }
    }

    fn build(_: &BallParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let _ = (ctx.rng.next_u64(), ctx.rng.next_u64());
        Ok(PartOutput::default())
    }
}
omuzaki_char_api::register_part_kind!(TestNoise);

/// A mesh whose material nobody defines.
struct TestOrphan;

impl PartKind for TestOrphan {
    type Params = BallParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-orphan",
            summary: "Use an undefined material.",
            category: PartCategory::Accessory,
            requires: &[],
        }
    }

    fn build(_: &BallParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        Ok(PartOutput { meshes: vec![mesh("orphan", "ghost", Vec3::ZERO)], ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(TestOrphan);

struct TestBroken;

impl PartKind for TestBroken {
    type Params = BallParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "test-broken",
            summary: "Return a broken mesh.",
            category: PartCategory::Accessory,
            requires: &[],
        }
    }

    fn build(_: &BallParams, _: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let mut m = mesh("oops", "test-hair", Vec3::ZERO);
        m.geometry.normals.pop();
        Ok(PartOutput { meshes: vec![m], ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(TestBroken);

// ---- helpers

fn part(kind: &str, layer: i32, params: serde_json::Value) -> PartDef {
    PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer, attach: None, params }
}

/// The given parts plus a `body`, which supplies the skeleton every built character needs.
fn character(parts: &[(&str, PartDef)]) -> CharacterDef {
    let body = ("body", common::body_part());
    let parts: Vec<_> = std::iter::once(&body).chain(parts).collect();
    CharacterDef {
        name: "Test".to_owned(),
        template: omuzaki_char_api::BodyTemplateId::new("soft-sd").unwrap(),
        seed: 42,
        palette: Palette::default(),
        skin_tone: None,
        parts: parts.iter().map(|(n, p)| (PartId::new(*n).unwrap(), p.clone())).collect(),
        face: FaceProfile::default(),
        weight_brushes: BTreeMap::new(),
    }
}

fn build(def: &CharacterDef) -> Result<omuzaki_char_api::BuiltCharacter, BuildError> {
    RegistryBuilder.build(&CharId::new("mira").unwrap(), def, &BuildOptions::default(), &BuildServices::default())
}

// ---- tests

#[test]
fn kinds_register_themselves_and_are_found_by_id() {
    let mut ids: Vec<_> = kinds().map(|r| (r.meta)().id).filter(|id| id.starts_with("test-")).collect();
    ids.sort_unstable();
    assert_eq!(ids, ["test-body", "test-broken", "test-hair", "test-head", "test-noise", "test-orphan"]);
    assert!(find_kind("test-head").is_some() && find_kind("no-such-kind").is_none());
    assert_eq!((find_kind("test-hair").unwrap().meta)().requires, ["test-head"]);
    for r in kinds() {
        PartKindId::new((r.meta)().id).expect("kind ids are valid slugs");
    }
}

#[test]
fn a_kind_exposes_its_schema_defaults_and_validation() {
    let reg = find_kind("test-head").unwrap();
    let schema = serde_json::to_value((reg.schema)()).unwrap();
    assert!(schema["properties"]["radius"].is_object(), "{schema}");
    assert_eq!(schema["additionalProperties"], false, "typos are rejected, and the schema says so");
    assert_eq!((reg.defaults)(), json!({"radius": 0.1}));
    assert_eq!((reg.check)(&json!({})).unwrap(), json!({"radius": 0.1}), "defaults are filled in");
    assert_eq!((reg.check)(&json!({"radius": 0.25})).unwrap(), json!({"radius": 0.25}));
    let e = (reg.check)(&json!({"radius": 5.0})).unwrap_err();
    assert_eq!((e.0[0].path.as_str(), e.0.len()), ("radius", 1));
    let e = (reg.check)(&json!({"radis": 1.0})).unwrap_err();
    assert!(e.to_string().contains("radis"), "{e}");
}

#[test]
fn building_runs_parts_in_dependency_order_and_threads_what_they_publish() {
    let c =
        character(&[("hair", part("test-hair", 0, json!({}))), ("head", part("test-head", 0, json!({"radius": 0.2})))]);
    let built = build(&c).unwrap();
    let order: Vec<_> = built.report.parts.iter().map(|p| p.part.to_string()).collect();
    assert_eq!(order, ["body", "head", "hair"], "hair requires the head, so it builds after it");
    assert_eq!(built.meshes.len(), 2);
    assert_eq!(built.report.triangles(), 2);
    assert_eq!(built.report.parts[1].warnings, ["built by head"]);
    assert!(built.materials.contains_key(&omuzaki_char_api::MaterialName::new("test-hair").unwrap()));
    let face = built.face.as_ref().expect("the head published a face projection");
    assert_eq!(face.projection.rect.x1, 0.1);
    // The hair root sits on the head surface: centre y 1.0 + radius 0.2 + offset 0.01.
    let hair = built.meshes.iter().find(|m| m.part.as_str() == "hair").unwrap();
    assert!((hair.mesh.geometry.positions[0].y - 1.21).abs() < 1e-5);
    // Both materials composite a face layer through the same window, placed on the head (centre y = 1).
    let overlay = |m: &str| built.materials[&omuzaki_char_api::MaterialName::new(m).unwrap()].overlays[0];
    let (skin, brows) = (overlay("test-head-skin"), overlay("test-hair"));
    assert_eq!((skin.layer, brows.layer), (FaceLayerId::Base, FaceLayerId::Brows));
    assert_eq!(skin.window.min, brows.window.min, "brows line up with the face exactly");
    assert!((skin.window.min.y - 0.9).abs() < 1e-6, "the window moved from head-local to bind space");
    assert_eq!(brows.window.fade_from, brows.window.fade_to, "brows cut hard");
    // The stage creates one texture per composited layer: the face on the skin, and the thinner brow layer for the hair.
    let textures = built.face_textures(3600.0);
    let layers: Vec<_> = textures.iter().map(|(spec, desc)| (spec.id, desc.size.x)).collect();
    assert_eq!(layers, [(FaceLayerId::Base, 720), (FaceLayerId::Brows, 360)]);
}

#[test]
fn a_parts_random_numbers_depend_only_on_the_seed_and_its_own_id() {
    let hair = || ("hair", part("test-hair", 0, json!({})));
    let head = || ("head", part("test-head", 0, json!({})));
    let jitter = |c: &CharacterDef| {
        build(c).unwrap().meshes.iter().find(|m| m.part.as_str() == "hair").unwrap().mesh.geometry.positions[0].x
    };
    let a = jitter(&character(&[head(), hair()]));
    assert_eq!(a, jitter(&character(&[head(), hair()])), "rebuilding is identical");
    assert_eq!(
        a,
        jitter(&character(&[head(), hair(), ("noise", part("test-noise", 0, json!({})))])),
        "adding a part doesn't shift another's numbers"
    );
    let mut other_seed = character(&[head(), hair()]);
    other_seed.seed = 43;
    assert_ne!(a, jitter(&other_seed), "a different seed gives different hair");
}

#[test]
fn disabled_parts_are_not_built() {
    let mut c = character(&[("head", part("test-head", 0, json!({}))), ("hair", part("test-hair", 0, json!({})))]);
    c.parts.get_mut(&PartId::new("hair").unwrap()).unwrap().enabled = false;
    assert_eq!(build(&c).unwrap().meshes.len(), 1);
}

#[test]
fn errors_name_the_part_and_what_to_fix() {
    let unknown = build(&character(&[("x", part("no-such-kind", 0, json!({})))])).unwrap_err();
    assert!(matches!(unknown, BuildError::UnknownKind { ref part, .. } if part.as_str() == "x"));

    let bad_params = build(&character(&[("head", part("test-head", 0, json!({"radius": 9})))])).unwrap_err();
    assert!(
        matches!(bad_params, BuildError::Params { ref part, ref errors } if part.as_str() == "head" && errors.0[0].path == "radius"),
        "{bad_params}"
    );

    let broken = build(&character(&[("b", part("test-broken", 0, json!({})))])).unwrap_err();
    assert!(matches!(broken, BuildError::Mesh { ref mesh, .. } if mesh == "oops"), "{broken}");

    // A kind that requires a head, with no head part in the character: the head surface is missing.
    let mut c = character(&[("hair", part("test-hair", 0, json!({})))]);
    c.parts.insert(PartId::new("head").unwrap(), part("test-head", 0, json!({})));
    c.parts.get_mut(&PartId::new("head").unwrap()).unwrap().enabled = false;
    let missing = build(&c).unwrap_err();
    assert!(matches!(missing, BuildError::Missing { needs: "a head surface", .. }), "{missing}");
    assert!(missing.to_string().contains("hair"));

    let mut bad_face = character(&[]);
    bad_face.face.layout.eye_spacing = 5.0;
    assert!(matches!(build(&bad_face).unwrap_err(), BuildError::Face(_)));
}

#[test]
fn materials_must_exist_and_be_unique_across_parts() {
    let orphan = build(&character(&[("o", part("test-orphan", 0, json!({})))])).unwrap_err();
    assert!(
        matches!(orphan, BuildError::UnknownMaterial { ref material, .. } if material.as_str() == "ghost"),
        "{orphan}"
    );

    // Two heads both define `test-head-skin`.
    let dup =
        build(&character(&[("head", part("test-head", 0, json!({}))), ("head2", part("test-head", 0, json!({})))]))
            .unwrap_err();
    assert_eq!(dup, BuildError::Duplicate { what: "material", id: "test-head-skin".to_owned() });
}

#[test]
fn a_character_of_only_a_body_builds_to_nothing_drawable() {
    let built = build(&character(&[])).unwrap();
    assert!(built.meshes.is_empty() && built.face.is_none() && built.runtimes.is_empty());
}
