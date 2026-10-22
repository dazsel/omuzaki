//! What a scene owes the stage, paid (#274, #2143, design scenes D3): setting, changing or clearing a scene's skin floor
//! updates every toon skin material in place, and nothing else; the floor stays through a look edit; clearing it gives
//! each material its own floor back. A ground change makes exactly one mesh (#2143).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeMap;

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, LookDef, MaterialName, RegistryBuilder,
};
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Capabilities, Colour, EnvironmentDesc, EnvironmentId, FrameReport, Image,
    Material, MaterialId, MeshData, MeshId, MeshOptions, Painter, Projection, Quality, RenderError, Renderer,
    ResourceRef, ResourceStats, Scene, Shading, TargetDesc, TargetId, TextureDesc, TextureId,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::StageError;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::FrameInput;
use omuzaki_stage::frame::{Stage, StageClock};
use omuzaki_stage::scene::resolve::{ResolvedEnvironment, ResolvedScene};
use omuzaki_stage_api::{EnvironmentSource, GroundKind};
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store};
use omuzaki_store_mock::MemStore;
use serde_json::json;

/// The mock renderer, keeping the last material sent for every id and counting updates.
struct Materials {
    inner: MockRenderer,
    held: BTreeMap<MaterialId, Material>,
    updates: usize,
    meshes_made: usize,
    environments_made: usize,
}

impl Renderer for Materials {
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    fn stats(&self) -> ResourceStats {
        self.inner.stats()
    }
    fn create_mesh(&mut self, mesh: &MeshData) -> Result<MeshId, RenderError> {
        self.meshes_made += 1;
        self.inner.create_mesh(mesh)
    }
    fn create_mesh_with(&mut self, mesh: &MeshData, options: &MeshOptions) -> Result<MeshId, RenderError> {
        self.meshes_made += 1;
        self.inner.create_mesh_with(mesh, options)
    }
    fn update_vertices(&mut self, mesh: MeshId, positions: &[Vec3], normals: &[Vec3]) -> Result<(), RenderError> {
        self.inner.update_vertices(mesh, positions, normals)
    }
    fn create_texture(&mut self, desc: &TextureDesc, rgba: Option<&[u8]>) -> Result<TextureId, RenderError> {
        self.inner.create_texture(desc, rgba)
    }
    fn paint_texture(&mut self, texture: TextureId, painter: &mut dyn Painter) -> Result<(), RenderError> {
        self.inner.paint_texture(texture, painter)
    }
    fn create_material(&mut self, material: &Material) -> Result<MaterialId, RenderError> {
        let id = self.inner.create_material(material)?;
        self.held.insert(id, material.clone());
        Ok(id)
    }
    fn update_material(&mut self, id: MaterialId, material: &Material) -> Result<(), RenderError> {
        self.inner.update_material(id, material)?;
        self.held.insert(id, material.clone());
        self.updates += 1;
        Ok(())
    }
    fn create_target(&mut self, desc: &TargetDesc) -> Result<TargetId, RenderError> {
        self.inner.create_target(desc)
    }
    fn resize_target(&mut self, target: TargetId, size: UVec2) -> Result<(), RenderError> {
        self.inner.resize_target(target, size)
    }
    fn create_environment(&mut self, desc: &EnvironmentDesc) -> Result<EnvironmentId, RenderError> {
        self.environments_made += 1;
        self.inner.create_environment(desc)
    }
    fn release(&mut self, resource: ResourceRef) -> Result<(), RenderError> {
        self.inner.release(resource)
    }
    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError> {
        self.inner.render(target, scene, quality)
    }
    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        self.inner.read_target(target)
    }
}

fn renderer() -> Materials {
    Materials { inner: MockRenderer::new(), held: BTreeMap::new(), updates: 0, meshes_made: 0, environments_made: 0 }
}

fn mock() -> BuiltCharacter {
    omuzaki_stage::link();
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

fn staged(renderer: &mut Materials) -> Stage {
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::splat(32), alpha: AlphaEncoding::Straight }).unwrap();
    let scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.8, 0.95),
        Vec3::new(0.0, 0.78, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    ));
    Stage::new(renderer, mock(), UploadOptions::default(), scene, target, Quality::offline(), StageClock::fixed(60.0))
        .expect("the stage starts")
}

/// The skin floor the renderer holds for every staged material: `Some` for toon skin, `None` for the rest.
fn floors(stage: &Stage, renderer: &Materials) -> BTreeMap<MaterialName, Option<f32>> {
    stage
        .built()
        .materials
        .keys()
        .map(|name| {
            let id = stage.character().material(name).expect("every material is uploaded");
            let floor = match &renderer.held[&id].shading {
                Shading::Toon(toon) => toon.skin.as_ref().map(|skin| skin.floor),
                _ => None,
            };
            (name.clone(), floor)
        })
        .collect()
}

/// A scene with no ground and the skin floor override `floor`.
fn scene(floor: Option<f32>) -> ResolvedScene {
    let mut resolved = ResolvedScene::new();
    resolved.ground.kind = GroundKind::None;
    resolved.skin_floor = floor;
    resolved
}

/// Sets a scene whose only difference is its skin floor override, pays what it owes and checks nothing stays owed.
fn floor_to(stage: &mut Stage, renderer: &mut Materials, floor: Option<f32>) {
    let resolved = scene(floor);
    let owed = stage.set_scene(&resolved, Mat4::IDENTITY);
    assert!(owed.skin_floor.is_some(), "a changed floor is owed");
    stage.pay_scene(renderer).unwrap();
    assert!(stage.owed().is_empty(), "the floor was paid");
    assert_eq!(stage.character().skin_floor(), floor);
}

#[test]
fn the_scene_skin_floor_reaches_every_skin_material_and_nothing_else() {
    let mut renderer = renderer();
    let mut stage = staged(&mut renderer);
    let own = floors(&stage, &renderer);
    let skin: Vec<&MaterialName> = own.iter().filter(|(_, f)| f.is_some()).map(|(n, _)| n).collect();
    assert!(!skin.is_empty(), "the mock character has skin");
    assert!(own.len() > skin.len(), "and materials that aren't skin");
    assert!(own.values().flatten().all(|f| (*f - 0.9).abs() > 1e-3), "0.9 is not a material's own floor");

    // Set: every skin material, and only those, is updated to the scene's floor.
    let (before, meshes) = (renderer.updates, renderer.stats().meshes);
    floor_to(&mut stage, &mut renderer, Some(0.9));
    assert_eq!(renderer.updates - before, skin.len(), "one update per skin material");
    assert_eq!(renderer.stats().meshes, meshes, "no mesh is made");
    for (name, floor) in floors(&stage, &renderer) {
        assert_eq!(floor, own[&name].map(|_| 0.9), "{name}");
    }

    // A look edit redraws materials; the floor stays on them.
    let look = LookDef::from_node(Some(&json!({ "overrides": { "skin": { "rim_scale": 0.0 } } })))
        .and_then(|def| def.resolve())
        .unwrap();
    assert!(stage.apply_look(&mut renderer, &look).unwrap() > 0);
    for name in &skin {
        assert_eq!(floors(&stage, &renderer)[*name], Some(0.9), "{name} keeps the floor through a look edit");
    }

    // Change, then clear: back to each material's own floor.
    floor_to(&mut stage, &mut renderer, Some(0.6));
    assert!(floors(&stage, &renderer).values().flatten().all(|f| (*f - 0.6).abs() < 1e-6));
    floor_to(&mut stage, &mut renderer, None);
    assert_eq!(floors(&stage, &renderer), own, "clearing the override gives each material its own floor");

    // The same scene again owes nothing.
    let owed = stage.set_scene(&scene(None), Mat4::IDENTITY);
    assert!(owed.is_empty());
    stage.release(&mut renderer).unwrap();
}

#[test]
fn a_ground_change_makes_exactly_one_mesh_and_a_value_change_none() {
    let mut renderer = renderer();
    let mut stage = staged(&mut renderer);
    let meshes = renderer.stats().meshes;
    let mut shadow = scene(None);
    shadow.ground.kind = GroundKind::Shadow;
    shadow.ground.radius_m = 1.2;
    shadow.ground.opacity = 0.5;

    let made = renderer.meshes_made;
    assert!(stage.set_scene(&shadow, Mat4::IDENTITY).ground.is_some());
    stage.pay_scene(&mut renderer).unwrap();
    assert_eq!(renderer.meshes_made - made, 1, "the disc");
    assert_eq!(renderer.stats().meshes, meshes + 1);

    // A wider disc replaces the old one: one mesh made, the old released.
    shadow.ground.radius_m = 2.0;
    let made = renderer.meshes_made;
    stage.set_scene(&shadow, Mat4::IDENTITY);
    stage.pay_scene(&mut renderer).unwrap();
    assert_eq!(renderer.meshes_made - made, 1, "one new disc");
    assert_eq!(renderer.stats().meshes, meshes + 1, "the old disc is released");

    // A value-only change (the outline light) owes nothing and makes nothing.
    shadow.outline_light = 0.5;
    let made = renderer.meshes_made;
    assert!(stage.set_scene(&shadow, Mat4::IDENTITY).is_empty());
    stage.pay_scene(&mut renderer).unwrap();
    assert_eq!(renderer.meshes_made, made);

    // No ground: the disc goes.
    stage.set_scene(&scene(None), Mat4::IDENTITY);
    stage.pay_scene(&mut renderer).unwrap();
    assert_eq!(renderer.stats().meshes, meshes);
    stage.release(&mut renderer).unwrap();
}

fn sky(zenith: Colour) -> EnvironmentSource {
    EnvironmentSource::Sky { zenith, horizon: Colour::hex(0x00e8_c9a0), ground: Colour::hex(0x003b_3026) }
}

/// A scene with no ground, the environment `source` used at `intensity`, shown behind the cast.
fn environment_scene(source: Option<EnvironmentSource>, intensity: f32) -> ResolvedScene {
    let mut resolved = scene(None);
    resolved.environment = source.map(|s| {
        let mut environment = ResolvedEnvironment::new(s);
        environment.intensity = intensity;
        environment
    });
    resolved.background = Background::Environment;
    resolved
}

#[test]
fn the_environment_is_made_when_its_source_changes_and_used_by_every_frame() {
    let mut renderer = renderer();
    let mut stage = staged(&mut renderer);
    let first = environment_scene(Some(sky(Colour::hex(0x006f_86b8))), 0.6);
    let owed = stage.set_scene(&first, Mat4::IDENTITY);
    assert_eq!(owed.environment, Some(Some(sky(Colour::hex(0x006f_86b8)))));
    assert_eq!(stage.scene().environment, None, "not used until it is made");
    stage.pay_scene(&mut renderer).unwrap();
    assert!(stage.owed().is_empty());
    assert_eq!((renderer.environments_made, renderer.stats().environments), (1, 1));
    let using = stage.scene().environment.expect("the frame uses it");
    assert_eq!((using.intensity, using.toon_ramp), (0.6, true));
    stage.tick(&mut renderer, &FrameInput::default()).expect("a frame renders with it");

    // A value change reaches the frame at once and makes nothing.
    let brighter = environment_scene(Some(sky(Colour::hex(0x006f_86b8))), 1.4);
    assert!(stage.set_scene(&brighter, Mat4::IDENTITY).is_empty());
    assert_eq!(stage.scene().environment.unwrap().intensity, 1.4);

    // Another sky replaces it; no environment releases it.
    stage.set_scene(&environment_scene(Some(sky(Colour::WHITE)), 1.0), Mat4::IDENTITY);
    stage.pay_scene(&mut renderer).unwrap();
    assert_eq!((renderer.environments_made, renderer.stats().environments), (2, 1), "the old one is released");
    stage.set_scene(&environment_scene(None, 1.0), Mat4::IDENTITY);
    stage.pay_scene(&mut renderer).unwrap();
    assert_eq!((renderer.stats().environments, stage.scene().environment), (0, None));

    stage.set_scene(&first, Mat4::IDENTITY);
    stage.pay_scene(&mut renderer).unwrap();
    stage.release(&mut renderer).unwrap();
    assert_eq!(renderer.stats().environments, 0, "releasing the stage releases its environment");
}

/// A 2 x 1 Radiance file (flat RGBE).
fn tiny_hdr() -> Vec<u8> {
    let mut bytes = b"#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y 1 +X 2\n".to_vec();
    bytes.extend_from_slice(&[128, 64, 32, 129, 128, 128, 128, 130]);
    bytes
}

#[test]
fn an_hdri_is_loaded_from_the_store_off_the_render_thread_then_made() {
    let store = MemStore::new();
    let provenance = Provenance {
        source: Source::Authored,
        author: None,
        licence: "CC0-1.0".into(),
        attribution: None,
        commercial: Commercial::Ok,
        consent: None,
        private_test: false,
    };
    let asset = store.put(&tiny_hdr(), provenance).unwrap();
    let mut renderer = renderer();
    let mut stage = staged(&mut renderer);
    let mut resolved = environment_scene(Some(EnvironmentSource::Hdri { asset }), 1.0);

    // Not loaded: the work stays owed and says why.
    stage.set_scene(&resolved, Mat4::IDENTITY);
    let error = stage.pay_scene(&mut renderer).unwrap_err();
    assert!(
        matches!(error, StageError::Scene { ref source } if source.path == "lighting.environment.asset"),
        "{error}"
    );
    assert!(stage.owed().environment.is_some());

    resolved.load_hdri(&store).unwrap();
    stage.set_scene(&resolved, Mat4::IDENTITY);
    stage.pay_scene(&mut renderer).unwrap();
    assert!(stage.owed().is_empty() && stage.scene().environment.is_some());

    let mut missing = environment_scene(Some(EnvironmentSource::Hdri { asset: ContentHash::of(b"gone") }), 1.0);
    let error = missing.load_hdri(&store).unwrap_err();
    assert!(
        matches!(error, StageError::Scene { ref source } if source.path == "lighting.environment.asset"),
        "{error}"
    );
    stage.release(&mut renderer).unwrap();
}
