//! A scene's props on the stage (#932, design D7, D10): resolved with their builds kept while their params are, their
//! lights placed in the world beside the rig's within the limit, uploaded once per build, drawn every frame without
//! allocating, and a prop in a hand following the hand's bone.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::{Mat4, Quat, UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Camera, Capabilities, EnvironmentDesc, EnvironmentId, FrameReport, Image, Light, Material,
    MaterialId, MeshData, MeshId, MeshOptions, Painter, Projection, Quality, RenderError, Renderer, ResourceRef,
    ResourceStats, Scene, TargetDesc, TargetId, TextureDesc, TextureId,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage::scene::resolve::{CheckedScene, Figure, ResolvedScene};
use omuzaki_stage_api::{
    Anchor, CastMember, GroundKind, MemberId, PropDef, PropId, PropKindId, SceneDef, SceneSettings,
};
use serde_json::json;

/// Counts the allocations made on a thread that has switched counting on.
struct Counting;

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    let _ = WATCHING.try_with(|w| {
        if w.get() {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        }
    });
}

// SAFETY: forwards to the system allocator; the counter is thread-local.
#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn counted<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|n| n.set(0));
    WATCHING.with(|w| w.set(true));
    let result = f();
    WATCHING.with(|w| w.set(false));
    (result, ALLOCATIONS.with(Cell::get))
}

/// The mock renderer, counting the meshes made. Once `canned` holds a report, `render` answers it without drawing and
/// vertex updates aren't copied, so a frame measures the stage's own work (as the frame loop's `Recorder` does).
struct Counted {
    inner: MockRenderer,
    meshes_made: usize,
    canned: Option<FrameReport>,
}

impl Renderer for Counted {
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
        if self.canned.is_some() { Ok(()) } else { self.inner.update_vertices(mesh, positions, normals) }
    }
    fn create_texture(&mut self, desc: &TextureDesc, rgba: Option<&[u8]>) -> Result<TextureId, RenderError> {
        self.inner.create_texture(desc, rgba)
    }
    fn paint_texture(&mut self, texture: TextureId, painter: &mut dyn Painter) -> Result<(), RenderError> {
        self.inner.paint_texture(texture, painter)
    }
    fn create_material(&mut self, material: &Material) -> Result<MaterialId, RenderError> {
        self.inner.create_material(material)
    }
    fn update_material(&mut self, id: MaterialId, material: &Material) -> Result<(), RenderError> {
        self.inner.update_material(id, material)
    }
    fn create_target(&mut self, desc: &TargetDesc) -> Result<TargetId, RenderError> {
        self.inner.create_target(desc)
    }
    fn resize_target(&mut self, target: TargetId, size: UVec2) -> Result<(), RenderError> {
        self.inner.resize_target(target, size)
    }
    fn create_environment(&mut self, desc: &EnvironmentDesc) -> Result<EnvironmentId, RenderError> {
        self.inner.create_environment(desc)
    }
    fn release(&mut self, resource: ResourceRef) -> Result<(), RenderError> {
        self.inner.release(resource)
    }
    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError> {
        match self.canned {
            Some(report) => Ok(report),
            None => self.inner.render(target, scene, quality),
        }
    }
    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        self.inner.read_target(target)
    }
}

fn built() -> BuiltCharacter {
    omuzaki_stage::link();
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

fn member(name: &str) -> MemberId {
    MemberId::new(name).unwrap()
}

fn id(name: &str) -> PropId {
    PropId::new(name).unwrap()
}

fn prop(kind: &str, params: serde_json::Value) -> PropDef {
    let mut prop = PropDef::new(PropKindId::new(kind).unwrap());
    prop.params = omuzaki_stage_api::prop_kind(kind).unwrap().check(&params).unwrap();
    prop
}

/// One member, `kiri`, standing at `position`.
fn figures(built: &BuiltCharacter, position: [f32; 2]) -> BTreeMap<MemberId, Figure> {
    let cast = CastMember { position, ..CastMember::new(CharId::new("mock").unwrap()) };
    BTreeMap::from([(member("kiri"), Figure::new(&cast, Landmarks::measure(built).unwrap()))])
}

/// A furnished corner: ten props, a lamp lit among them, and a cup in kiri's right hand.
fn room() -> SceneDef {
    let mut def = SceneDef::new(SceneSettings::named("Room"));
    // The room has its own floor: no ground disc, so the meshes made are the props'.
    def.scene.ground.kind = GroundKind::None;
    def.cast.insert(member("kiri"), CastMember::new(CharId::new("mock").unwrap()));
    let mut put = |name: &str, kind: &str, params: serde_json::Value, at: [f32; 3]| {
        let mut p = prop(kind, params);
        p.position = at;
        def.props.insert(id(name), p);
    };
    put("room", "room", json!({}), [0.0; 3]);
    put("rug", "rug", json!({}), [0.0, 0.0, 0.3]);
    put("desk", "desk", json!({}), [0.9, 0.0, -0.8]);
    put("chair", "chair", json!({}), [0.9, 0.0, -0.3]);
    put("shelf", "shelf", json!({"books": 0.5}), [-1.6, 0.0, -1.8]);
    put("lamp", "lamp", json!({}), [-1.4, 0.0, -0.9]);
    put("plant", "plant", json!({}), [1.9, 0.0, -1.6]);
    put("table", "cafe-table", json!({}), [-0.8, 0.0, 0.6]);
    put("mug", "cup", json!({}), [-0.8, 0.75, 0.6]);
    let mut cup = prop("cup", json!({"handle": false}));
    cup.anchor = Anchor::Member(member("kiri"));
    cup.attach = Some(HumanoidBone::RightHand);
    cup.position = [0.0, -0.03, 0.04];
    cup.rotation = [0.0, 0.0, 90.0];
    def.props.insert(id("cup"), cup);
    def
}

fn resolve(out: &mut ResolvedScene, def: &SceneDef, figures: &BTreeMap<MemberId, Figure>) {
    let proof = CheckedScene::new(def).expect("the scene passes its check");
    out.resolve_into(proof, figures, 16.0 / 9.0).unwrap();
}

#[test]
fn props_resolve_into_builds_and_placements_and_the_lamp_lights_the_room() {
    let built = built();
    let (def, figures) = (room(), figures(&built, [0.5, 0.0]));
    let mut out = ResolvedScene::new();
    let changes = {
        let proof = CheckedScene::new(&def).unwrap();
        out.resolve_into(proof, &figures, 1.0).unwrap()
    };
    assert!(changes.prop_builds && changes.props && !changes.value_only());
    let builds = &out.props.builds;
    assert_eq!(builds.props.len(), 10);
    assert!(builds.props.iter().all(|b| b.output.is_some()), "every kind is registered and builds");
    // The lamp's bulb is a point light where the lamp stands, after the rig's ambient and lights.
    let lamp = def.props[&id("lamp")].params["height"].as_f64().unwrap() as f32;
    let point = out.lights.as_slice().iter().find_map(|l| match *l {
        Light::Point { position, .. } => Some(position),
        _ => None,
    });
    let point = point.expect("the lit lamp brings its light");
    assert!((point.x + 1.4).abs() < 1e-5 && (point.z + 0.9).abs() < 1e-5, "{point}");
    assert!(point.y > lamp * 0.7 && point.y < lamp, "the bulb is under the shade: {point}");
    // The cup follows kiri's hand: its placement is in the bone's frame, not the world.
    let cup = builds.props.iter().position(|b| b.id == id("cup")).unwrap();
    assert!(out.props.placements[cup].attach.is_some());
}

#[test]
fn an_unchanged_room_resolves_without_allocating_and_a_move_is_a_value() {
    let built = built();
    let (mut def, figures) = (room(), figures(&built, [0.0, 0.0]));
    let mut out = ResolvedScene::new();
    resolve(&mut out, &def, &figures);
    let proof = CheckedScene::new(&def).unwrap();
    let (changes, allocations) = counted(|| out.resolve_into(proof, &figures, 16.0 / 9.0).unwrap());
    assert!(changes.is_empty(), "{changes:?}");
    assert_eq!(allocations, 0);

    def.props.get_mut(&id("chair")).unwrap().position = [1.0, 0.0, -0.2];
    let builds = Arc::clone(&out.props.builds);
    let proof = CheckedScene::new(&def).unwrap();
    let (changes, allocations) = counted(|| out.resolve_into(proof, &figures, 16.0 / 9.0).unwrap());
    assert!(changes.props && !changes.prop_builds && changes.value_only(), "{changes:?}");
    assert_eq!(allocations, 0, "a move rebuilds nothing");
    assert!(Arc::ptr_eq(&builds, &out.props.builds));
}

#[test]
fn a_param_edit_rebuilds_that_prop_alone() {
    let built = built();
    let (mut def, figures) = (room(), figures(&built, [0.0, 0.0]));
    let mut out = ResolvedScene::new();
    resolve(&mut out, &def, &figures);
    let before = Arc::clone(&out.props.builds);
    let desk = def.props.get_mut(&id("desk")).unwrap();
    desk.params = omuzaki_stage_api::prop_kind("desk").unwrap().check(&json!({"width": 1.8})).unwrap();
    resolve(&mut out, &def, &figures);
    let after = &out.props.builds;
    assert_ne!(before.generation, after.generation);
    for (old, new) in before.props.iter().zip(&after.props) {
        let same = Arc::ptr_eq(old.output.as_ref().unwrap(), new.output.as_ref().unwrap());
        assert_eq!(same, new.id != id("desk"), "{}: only the desk is rebuilt", new.id);
    }
}

#[test]
fn an_unknown_kind_is_kept_and_skipped_and_extra_lights_are_dropped() {
    let built = built();
    let figures = figures(&built, [0.0, 0.0]);
    let mut def = SceneDef::new(SceneSettings::named("Lamps"));
    def.cast.insert(member("kiri"), CastMember::new(CharId::new("mock").unwrap()));
    def.props.insert(id("ghost"), PropDef::new(PropKindId::new("teleporter").unwrap()));
    // The ambient and the default rig take some of the eight lights; twelve lamps can't all fit.
    for i in 0..12_u8 {
        let mut lamp = prop("lamp", json!({}));
        lamp.position = [f32::from(i) * 0.5 - 3.0, 0.0, -1.0];
        def.props.insert(id(&format!("lamp-{i:02}")), lamp);
    }
    let mut out = ResolvedScene::new();
    resolve(&mut out, &def, &figures);
    let ghost = out.props.builds.props.iter().find(|b| b.id == id("ghost")).unwrap();
    assert!(ghost.output.is_none());
    let limit = omuzaki_render_api::limits::MAX_LIGHTS;
    assert_eq!(out.lights.len(), limit, "the lights fill the limit");
    let rig = 1 + def.lighting.lights.len();
    assert_eq!(out.props.dropped_lights, 12 - (limit - rig), "the rest are dropped and counted");
}

fn stage(renderer: &mut Counted, built: BuiltCharacter) -> Stage {
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::splat(32), alpha: AlphaEncoding::Straight }).unwrap();
    let scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 1.0, 3.0),
        Vec3::new(0.0, 0.8, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 40.0, near: 0.05, far: 20.0 },
    ));
    Stage::new(renderer, built, UploadOptions::default(), scene, target, Quality::offline(), StageClock::fixed(60.0))
        .expect("the stage starts")
}

/// The meshes every prop of `out` builds.
fn parts(out: &ResolvedScene) -> usize {
    out.props.builds.props.iter().filter_map(|b| b.output.as_ref()).map(|o| o.parts.len()).sum()
}

#[test]
fn a_furnished_room_uploads_once_draws_every_frame_without_allocating_and_a_param_edit_uploads_one_prop() {
    let built = built();
    let figures = figures(&built, [0.0, 0.0]);
    let mut def = room();
    let mut renderer = Counted { inner: MockRenderer::new(), meshes_made: 0, canned: None };
    let mut stage = stage(&mut renderer, built);
    let mut out = ResolvedScene::new();
    resolve(&mut out, &def, &figures);
    let owed = stage.set_scene(&out, Mat4::IDENTITY);
    assert!(owed.props.is_some(), "a scene with props owes their upload");
    let made = renderer.meshes_made;
    stage.pay_scene(&mut renderer).unwrap();
    assert_eq!(renderer.meshes_made - made, parts(&out), "one mesh per part, nothing else");
    assert!(stage.owed().is_empty());

    let input = FrameInput { dt: 1.0 / 60.0, ..FrameInput::default() };
    renderer.canned = Some(stage.tick(&mut renderer, &input).unwrap().report);
    stage.tick(&mut renderer, &input).unwrap();
    let base = stage.scene().instances.len();
    let (frame, allocations) = counted(|| (0..60).try_for_each(|_| stage.tick(&mut renderer, &input).map(drop)));
    frame.unwrap();
    assert_eq!(allocations, 0, "60 frames with ten props allocate nothing");
    assert_eq!(stage.scene().instances.len(), base);
    renderer.canned = None;

    // A param edit: the shelf fills up. Exactly its parts are made again.
    let shelf = def.props.get_mut(&id("shelf")).unwrap();
    shelf.params = omuzaki_stage_api::prop_kind("shelf").unwrap().check(&json!({"books": 1.0})).unwrap();
    resolve(&mut out, &def, &figures);
    stage.set_scene(&out, Mat4::IDENTITY);
    let made = renderer.meshes_made;
    stage.pay_scene(&mut renderer).unwrap();
    let shelf_parts = out.props.builds.props.iter().find(|b| b.id == id("shelf")).unwrap();
    assert_eq!(renderer.meshes_made - made, shelf_parts.output.as_ref().unwrap().parts.len());

    // Hiding one prop draws its parts no more; removing every prop releases them all.
    def.props.get_mut(&id("rug")).unwrap().visible = false;
    resolve(&mut out, &def, &figures);
    assert!(stage.set_scene(&out, Mat4::IDENTITY).is_empty(), "hiding is a value");
    stage.tick(&mut renderer, &input).unwrap();
    assert_eq!(stage.scene().instances.len(), base - 1, "the rug is one part");
    let live = renderer.stats();
    def.props.clear();
    resolve(&mut out, &def, &figures);
    stage.set_scene(&out, Mat4::IDENTITY);
    stage.pay_scene(&mut renderer).unwrap();
    let after = renderer.stats();
    assert!(after.meshes < live.meshes, "the props' meshes are released: {} then {}", live.meshes, after.meshes);
    stage.release(&mut renderer).unwrap();
}

/// A wave: the right arm raised and swinging.
fn wave(frame: u16) -> HumanoidPose {
    let t = f32::from(frame) / 60.0;
    let mut pose = HumanoidPose::REST;
    pose.set(HumanoidBone::RightUpperArm, Quat::from_rotation_z(1.9 + 0.35 * (t * 9.0).sin()));
    pose.set(HumanoidBone::RightLowerArm, Quat::from_rotation_y(0.5 * (t * 7.0).cos()));
    pose.set(HumanoidBone::RightHand, Quat::from_rotation_x(0.3 * (t * 11.0).sin()));
    pose
}

#[test]
fn a_cup_in_the_right_hand_follows_a_wave() {
    let built = built();
    let figures = figures(&built, [0.0, 0.0]);
    let mut def = room();
    def.props.retain(|k, _| *k == id("cup"));
    let mut renderer = Counted { inner: MockRenderer::new(), meshes_made: 0, canned: None };
    let mut stage = stage(&mut renderer, built);
    let mut out = ResolvedScene::new();
    resolve(&mut out, &def, &figures);
    stage.set_scene(&out, Mat4::IDENTITY);
    stage.pay_scene(&mut renderer).unwrap();
    let local = def.props[&id("cup")].local();
    let parts = parts(&out);
    let mut moved = 0.0_f32;
    let mut last = None;
    for frame in 0..90 {
        let input = FrameInput { pose: wave(frame), dt: 1.0 / 60.0, ..FrameInput::default() };
        stage.tick(&mut renderer, &input).unwrap();
        // The CPU reference: the hand's world matrix as the rig solved it, times the cup's own transform.
        let hand = stage.bone_world(HumanoidBone::RightHand).expect("the mock character has a right hand");
        let reference = hand * local;
        let drawn = &stage.scene().instances[stage.scene().instances.len() - parts..];
        for instance in drawn {
            let error = (instance.transform - reference).to_cols_array().iter().map(|v| v.abs()).fold(0.0, f32::max);
            assert!(error < 1e-5, "frame {frame}: the cup is {error} off the hand");
        }
        let at = reference.transform_point3(Vec3::ZERO);
        if let Some(before) = last {
            moved += at.distance(before);
        }
        last = Some(at);
    }
    assert!(moved > 0.2, "the wave carries the cup: {moved} m");
    stage.release(&mut renderer).unwrap();
}
