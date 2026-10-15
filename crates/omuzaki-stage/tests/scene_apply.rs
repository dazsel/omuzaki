//! `scene::apply`: the fill of the renderer's `Scene` from a `ResolvedScene` allocates nothing once reserved, the
//! applier owes the ground and the skin floor only when they change, and the mailbox hands over the newest scene.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
// The counting allocator is the one unsafe impl: it forwards to `System` unchanged (as in tests/scene_resolve.rs).
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_render_api::{Background, Camera, Colour, Light, Scene};
use omuzaki_stage::scene::apply::{Applier, Mailbox, Owed, fill, reserve};
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage::scene::resolve::{CheckedScene, Figure, ResolvedScene};
use omuzaki_stage_api::{
    Anchor, CameraDef, CameraId, CastMember, Framing, GroundKind, LightDef, LightId, LightShape, MemberId, SceneDef,
    SceneSettings,
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

const ASPECT: f32 = 16.0 / 9.0;

fn member() -> MemberId {
    MemberId::new("kiri").unwrap()
}

fn figures() -> BTreeMap<MemberId, Figure> {
    omuzaki_stage::link();
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    let built = RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds");
    let landmarks = Landmarks::measure(&built).unwrap();
    BTreeMap::from([(member(), Figure::new(&CastMember::new(CharId::new("mock").unwrap()), landmarks))])
}

/// A member, two cameras and three lights.
fn scene() -> SceneDef {
    let mut def = SceneDef::new(SceneSettings::named("Test"));
    def.cast.insert(member(), CastMember::new(CharId::new("mock").unwrap()));
    for (name, framing) in [("front", Framing::Full), ("close", Framing::Bust)] {
        def.cameras.insert(CameraId::new(name).unwrap(), CameraDef::preset(framing));
    }
    for (name, azimuth) in [("key", 30.0), ("fill", -50.0), ("rim", 150.0)] {
        let light = LightDef {
            role: None,
            colour: Colour::WHITE,
            intensity: 1.0,
            shadow: false,
            anchor: Anchor::Subject,
            shape: LightShape::Directional { azimuth, elevation: 40.0 },
        };
        def.lighting.lights.insert(LightId::new(name).unwrap(), light);
    }
    def
}

fn resolve(def: &SceneDef) -> ResolvedScene {
    let mut out = ResolvedScene::new();
    out.resolve_into(CheckedScene::new(def).unwrap(), &figures(), ASPECT).unwrap();
    out
}

/// What a caller does: apply, do the owed work (which succeeds here), commit.
fn settle(applier: &mut Applier, resolved: &ResolvedScene, target: &mut Scene) -> Owed {
    let owed = applier.apply(resolved, target);
    applier.commit(&owed);
    owed
}

fn empty_scene() -> Scene {
    let mut scene = Scene::new(Camera::default());
    reserve(&mut scene);
    scene
}

#[test]
fn fill_copies_the_resolved_values_into_the_scene() {
    let mut def = scene();
    def.scene.camera = Some(CameraId::new("close").unwrap());
    let resolved = resolve(&def);
    let mut target = empty_scene();
    fill(&resolved, &mut target);
    assert_eq!(target.camera, resolved.cameras[resolved.active.unwrap()].1);
    assert_eq!(target.lights, resolved.lights.as_slice());
    assert_eq!(target.background, resolved.background);
    assert_eq!(target.post, resolved.post);
    assert_eq!(target.outline_light.to_bits(), resolved.outline_light.to_bits());
}

#[test]
fn fill_allocates_nothing_over_600_frames() {
    let resolved = resolve(&scene());
    let mut target = empty_scene();
    let ((), allocations) = counted(|| {
        for _ in 0..600 {
            fill(&resolved, &mut target);
        }
    });
    assert_eq!(allocations, 0);
    assert_eq!(target.lights.len(), 4, "the ambient and three lights");
}

#[test]
fn applying_allocates_nothing_for_a_value_edit_or_an_unchanged_scene() {
    let (mut def, figures) = (scene(), figures());
    let mut resolved = ResolvedScene::new();
    resolved.resolve_into(CheckedScene::new(&def).unwrap(), &figures, ASPECT).unwrap();
    let (mut applier, mut target) = (Applier::new(), empty_scene());
    settle(&mut applier, &resolved, &mut target);
    def.cameras.get_mut(&CameraId::new("close").unwrap()).unwrap().yaw = 20.0;
    let proof = CheckedScene::new(&def).unwrap();
    let ((changes, owed), allocations) = counted(|| {
        let changes = resolved.resolve_into(proof, &figures, ASPECT).unwrap();
        (changes, settle(&mut applier, &resolved, &mut target))
    });
    assert!(changes.cameras && changes.value_only(), "{changes:?}");
    assert!(owed.is_empty(), "{owed:?}");
    assert_eq!(allocations, 0);
    let (_, allocations) = counted(|| settle(&mut applier, &resolved, &mut target));
    assert_eq!(allocations, 0);
}

#[test]
fn a_camera_yaw_edit_and_a_light_colour_edit_show_in_the_next_scene() {
    let (mut def, figures) = (scene(), figures());
    let mut resolved = ResolvedScene::new();
    resolved.resolve_into(CheckedScene::new(&def).unwrap(), &figures, ASPECT).unwrap();
    let (mut applier, mut target) = (Applier::new(), empty_scene());
    settle(&mut applier, &resolved, &mut target);
    let (camera_before, lights_before) = (target.camera, target.lights.clone());

    def.cameras.get_mut(&CameraId::new("close").unwrap()).unwrap().yaw = 25.0;
    def.lighting.lights.get_mut(&LightId::new("key").unwrap()).unwrap().colour = Colour::rgb(255, 0, 0);
    resolved.resolve_into(CheckedScene::new(&def).unwrap(), &figures, ASPECT).unwrap();
    settle(&mut applier, &resolved, &mut target);
    assert_ne!(target.camera, camera_before, "the yaw moved the camera");
    assert_ne!(target.lights, lights_before, "the colour changed a light");
    assert!(
        target
            .lights
            .iter()
            .any(|l| matches!(l, Light::Directional { colour, .. } if *colour == Colour::rgb(255, 0, 0)))
    );
}

#[test]
fn a_scene_without_cameras_keeps_the_stages_camera() {
    let mut def = scene();
    def.cameras.clear();
    def.scene.camera = None;
    let resolved = resolve(&def);
    let mut target = empty_scene();
    let before = target.camera;
    fill(&resolved, &mut target);
    assert_eq!(target.camera, before);
}

#[test]
fn the_ground_and_the_skin_floor_are_owed_only_when_they_change() {
    let mut resolved = resolve(&scene());
    let (mut applier, mut target) = (Applier::new(), empty_scene());

    let first = settle(&mut applier, &resolved, &mut target);
    assert_eq!(first.ground, Some(resolved.ground), "the first scene has a shadow ground to upload");
    assert_eq!(first.skin_floor, None, "no override, none applied");
    assert!(settle(&mut applier, &resolved, &mut target).is_empty(), "the same scene owes nothing");

    resolved.ground.radius_m += 1.0;
    let grown = settle(&mut applier, &resolved, &mut target);
    assert_eq!(grown.ground, Some(resolved.ground), "a new radius is a new disc");
    resolved.ground.opacity = 0.6;
    assert_eq!(settle(&mut applier, &resolved, &mut target).ground, Some(resolved.ground), "so is a new opacity");

    resolved.skin_floor = Some(0.4);
    assert_eq!(settle(&mut applier, &resolved, &mut target).skin_floor, Some(Some(0.4)), "set");
    assert!(settle(&mut applier, &resolved, &mut target).is_empty());
    resolved.skin_floor = Some(0.5);
    assert_eq!(settle(&mut applier, &resolved, &mut target).skin_floor, Some(Some(0.5)), "changed");
    resolved.skin_floor = None;
    assert_eq!(
        settle(&mut applier, &resolved, &mut target).skin_floor,
        Some(None),
        "cleared: back to each material's own"
    );

    resolved.ground.kind = GroundKind::None;
    let released = settle(&mut applier, &resolved, &mut target);
    assert_eq!(released.ground.map(|g| g.kind), Some(GroundKind::None), "no ground releases the disc");
    assert!(settle(&mut applier, &resolved, &mut target).is_empty());
}

#[test]
fn the_mailbox_hands_over_the_newest_scene_once() {
    let mailbox = Mailbox::new();
    let mut mine = ResolvedScene::new();
    assert!(!mailbox.take_into(&mut mine), "nothing posted yet");

    let mut def = scene();
    let first = resolve(&def);
    def.lighting.background = Colour::rgb(1, 2, 3);
    let second = resolve(&def);
    assert_ne!(first.background, second.background);
    mailbox.post(first);
    mailbox.post(second.clone());
    assert!(mailbox.take_into(&mut mine));
    assert_eq!(mine, second, "the newest wins");
    assert_eq!(mine.background, Background::Solid { colour: Colour::rgb(1, 2, 3) });
    assert!(!mailbox.take_into(&mut mine), "it is handed over once");
}

#[test]
fn taking_from_the_mailbox_allocates_nothing() {
    let mailbox = Mailbox::new();
    let mut mine = resolve(&scene());
    mailbox.post(resolve(&scene()));
    let (taken, allocations) = counted(|| (mailbox.take_into(&mut mine), mailbox.take_into(&mut mine)));
    assert_eq!(taken, (true, false));
    assert_eq!(allocations, 0);
}

#[test]
fn posts_from_other_threads_arrive() {
    let mailbox = std::sync::Arc::new(Mailbox::new());
    let posted = resolve(&scene());
    let sender = {
        let (mailbox, posted) = (std::sync::Arc::clone(&mailbox), posted.clone());
        std::thread::spawn(move || mailbox.post(posted))
    };
    sender.join().unwrap();
    let mut mine = ResolvedScene::new();
    assert!(mailbox.take_into(&mut mine));
    assert_eq!(mine, posted);
}

#[test]
fn work_that_failed_stays_owed_until_it_is_committed() {
    let mut resolved = resolve(&scene());
    resolved.skin_floor = Some(0.4);
    let (mut applier, mut target) = (Applier::new(), empty_scene());
    let owed = applier.apply(&resolved, &mut target);
    assert!(owed.ground.is_some() && owed.skin_floor == Some(Some(0.4)));
    // The renderer refused the disc: nothing is committed, so the next call owes the same work.
    assert_eq!(applier.apply(&resolved, &mut target), owed, "a retry owes what failed");
    applier.commit(&owed);
    assert!(applier.apply(&resolved, &mut target).is_empty(), "committed work is not owed again");
}
