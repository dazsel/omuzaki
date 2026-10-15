//! `ResolvedScene::resolve_into` for lights and cameras: what it reports changed, that an unchanged or value-only
//! resolve allocates nothing, that cameras follow their member, the active camera, and the scene errors.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
// The counting allocator is the one unsafe impl: it forwards to `System` unchanged (as in tests/frame.rs).
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::Vec3;
use omuzaki_render_api::Colour;
use omuzaki_stage::StageError;
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage::scene::resolve::{Changes, Figure, ResolvedScene};
use omuzaki_stage_api::{
    Anchor, CameraDef, CameraId, CastMember, Framing, LightDef, LightId, LightShape, MemberId, SceneDef, SceneSettings,
};

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// The allocator only counts and forwards to `System`, so it adds no obligations beyond the trait's.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Runs `f` and returns its result with the number of allocations it made.
fn counted<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|n| n.set(0));
    WATCHING.with(|w| w.set(true));
    let result = f();
    WATCHING.with(|w| w.set(false));
    (result, ALLOCATIONS.with(Cell::get))
}

fn member(name: &str) -> MemberId {
    MemberId::new(name).unwrap()
}

fn camera(name: &str) -> CameraId {
    CameraId::new(name).unwrap()
}

fn landmarks() -> Landmarks {
    omuzaki_stage::link();
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    let built = RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds");
    Landmarks::measure(&built).unwrap()
}

fn cast_member() -> CastMember {
    CastMember::new(CharId::new("mock").unwrap())
}

fn figures(position: [f32; 2]) -> BTreeMap<MemberId, Figure> {
    let moved = CastMember { position, ..cast_member() };
    BTreeMap::from([(member("kiri"), Figure::new(&moved, landmarks()))])
}

fn light(anchor: Anchor, colour: Colour) -> LightDef {
    LightDef {
        role: None,
        colour,
        intensity: 1.0,
        shadow: false,
        anchor,
        shape: LightShape::Directional { azimuth: 30.0, elevation: 40.0 },
    }
}

/// One member, a `full` and a `bust` camera, the default lighting and two lights.
fn scene() -> SceneDef {
    let mut def = SceneDef::new(SceneSettings::named("Test"));
    def.cast.insert(member("kiri"), cast_member());
    def.cameras.insert(camera("front"), CameraDef::preset(Framing::Full));
    def.cameras.insert(camera("close"), CameraDef::preset(Framing::Bust));
    def.lighting.lights.insert(LightId::new("key").unwrap(), light(Anchor::Subject, Colour::WHITE));
    def.lighting.lights.insert(LightId::new("rim").unwrap(), light(Anchor::Stage, Colour::WHITE));
    def
}

const ASPECT: f32 = 16.0 / 9.0;

fn resolved(def: &SceneDef, figures: &BTreeMap<MemberId, Figure>) -> ResolvedScene {
    let mut out = ResolvedScene::new();
    out.resolve_into(def, figures, ASPECT).unwrap();
    out
}

#[test]
fn an_unchanged_scene_resolves_without_allocating() {
    let (def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    let (changes, allocations) = counted(|| out.resolve_into(&def, &figures, ASPECT).unwrap());
    assert!(changes.is_empty(), "{changes:?}");
    assert_eq!(allocations, 0);
}

#[test]
fn the_first_resolve_reports_everything() {
    let (def, figures) = (scene(), figures([0.0, 0.0]));
    let changes = ResolvedScene::new().resolve_into(&def, &figures, ASPECT).unwrap();
    assert!(changes.lights && changes.cameras && changes.structure);
}

#[test]
fn a_camera_yaw_edit_is_value_only() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    def.cameras.get_mut(&camera("front")).unwrap().yaw = 20.0;
    let (changes, allocations) = counted(|| out.resolve_into(&def, &figures, ASPECT).unwrap());
    assert!(changes.cameras && !changes.structure && !changes.lights && changes.value_only(), "{changes:?}");
    assert_eq!(allocations, 0);
}

#[test]
fn a_light_colour_edit_is_value_only() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    def.lighting.lights.get_mut(&LightId::new("key").unwrap()).unwrap().colour = Colour::rgb(255, 128, 128);
    let (changes, allocations) = counted(|| out.resolve_into(&def, &figures, ASPECT).unwrap());
    assert!(changes.lights && !changes.structure && !changes.cameras && changes.value_only(), "{changes:?}");
    assert_eq!(allocations, 0);
}

#[test]
fn adding_a_camera_is_structural() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    def.cameras.insert(camera("side"), CameraDef { yaw: 90.0, ..CameraDef::preset(Framing::Full) });
    let changes = out.resolve_into(&def, &figures, ASPECT).unwrap();
    assert!(changes.structure && changes.cameras && !changes.value_only());
    assert_eq!(out.cameras.len(), 3);
    assert_eq!(out.resolve_into(&def, &figures, ASPECT).unwrap(), Changes::default());
}

#[test]
fn cameras_follow_their_member() {
    let def = scene();
    let at_origin = resolved(&def, &figures([0.0, 0.0]));
    let moved = resolved(&def, &figures([1.0, 0.0]));
    for (before, after) in at_origin.cameras.iter().zip(&moved.cameras) {
        let shift = after.1.position() - before.1.position();
        assert!((shift - Vec3::X).length() < 1e-5, "{}: {shift}", before.0);
    }
}

#[test]
fn a_free_camera_is_measured_from_its_anchor() {
    let mut def = scene();
    let free = CameraDef {
        framing: Framing::Free,
        anchor: Some(Anchor::Subject),
        position: Some([0.0, 0.5, 3.0]),
        target: Some([0.0, 0.5, 0.0]),
        ..CameraDef::preset(Framing::Free)
    };
    def.cameras.insert(camera("free"), free);
    let figures = figures([1.0, 0.0]);
    let out = resolved(&def, &figures);
    let (_, free) = out.cameras.iter().find(|(id, _)| *id == camera("free")).unwrap();
    let height = figures.values().next().unwrap().height_m;
    assert!((free.position() - Vec3::new(1.0, 0.5 * height, 3.0 * height)).length() < 1e-5, "{}", free.position());
}

#[test]
fn the_active_camera_is_the_scene_camera_else_the_first() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    // Cameras are by id: close, front.
    assert_eq!(resolved(&def, &figures).active, Some(0));
    def.scene.camera = Some(camera("front"));
    assert_eq!(resolved(&def, &figures).active, Some(1));
    def.scene.camera = Some(camera("nope"));
    assert!(matches!(ResolvedScene::new().resolve_into(&def, &figures, ASPECT), Err(StageError::Scene { .. })));
    def.scene.camera = None;
    def.cameras.clear();
    assert_eq!(resolved(&def, &figures).active, None);
}

#[test]
fn an_unknown_member_anchor_is_a_scene_error() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    def.lighting.lights.insert(LightId::new("ghost").unwrap(), light(Anchor::Member(member("nobody")), Colour::WHITE));
    let Err(StageError::Scene { source }) = ResolvedScene::new().resolve_into(&def, &figures, ASPECT) else {
        panic!("expected a scene error");
    };
    assert_eq!(source.path, "lighting.lights.ghost.anchor");
}

#[test]
fn a_preset_without_a_figure_is_a_scene_error() {
    let def = scene();
    let Err(StageError::Scene { source }) = ResolvedScene::new().resolve_into(&def, &BTreeMap::new(), ASPECT) else {
        panic!("expected a scene error");
    };
    assert_eq!(source.path, "cameras.close.subject");
}

#[test]
fn an_error_leaves_the_last_resolve_in_place() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    let before = out.clone();
    def.cameras.get_mut(&camera("front")).unwrap().subject = Anchor::Member(member("nobody"));
    assert!(out.resolve_into(&def, &figures, ASPECT).is_err());
    assert_eq!(out, before);
}
