//! `ResolvedScene::resolve_into` for lights and cameras: what it reports changed, that an unchanged or value-only
//! resolve allocates nothing, that cameras follow their member, the active camera, and the scene errors.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
// The counting allocator is the one unsafe impl: it forwards to `System` unchanged (as in tests/frame.rs).
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::{Affine3A, Vec3};
use omuzaki_render_api::{
    BackdropFit, Background, Bloom, Colour, ColourGrade, DepthOfField, FilmGrain, Fog, Projection, Tonemap,
};
use omuzaki_stage::StageError;
use omuzaki_stage::scene::landmarks::Landmarks;
use omuzaki_stage::scene::resolve::{Changes, CheckedScene, Figure, ResolvedScene};
use omuzaki_stage_api::{
    Anchor, Backdrop, Bars, BloomDef, CameraDef, CameraId, CastMember, DepthOfFieldDef, EnvironmentDef,
    EnvironmentSource, Focus, FocusSubject, FogDef, Framing, GradeDef, GroundKind, LightDef, LightId, LightShape,
    MemberId, SceneDef, SceneSettings,
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

fn checked(def: &SceneDef) -> CheckedScene<'_> {
    CheckedScene::new(def).expect("the scene passes its check")
}

fn resolved(def: &SceneDef, figures: &BTreeMap<MemberId, Figure>) -> ResolvedScene {
    let mut out = ResolvedScene::new();
    out.resolve_into(checked(def), figures, ASPECT).unwrap();
    out
}

#[test]
fn an_unchanged_scene_resolves_without_allocating() {
    let (def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    let proof = checked(&def);
    let (changes, allocations) = counted(|| out.resolve_into(proof, &figures, ASPECT).unwrap());
    assert!(changes.is_empty(), "{changes:?}");
    assert_eq!(allocations, 0);
}

#[test]
fn the_first_resolve_reports_everything() {
    let (def, figures) = (scene(), figures([0.0, 0.0]));
    let changes = ResolvedScene::new().resolve_into(checked(&def), &figures, ASPECT).unwrap();
    assert!(changes.lights && changes.cameras && changes.structure);
}

#[test]
fn a_camera_yaw_edit_is_value_only() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    def.cameras.get_mut(&camera("front")).unwrap().yaw = 20.0;
    let proof = checked(&def);
    let (changes, allocations) = counted(|| out.resolve_into(proof, &figures, ASPECT).unwrap());
    assert!(changes.cameras && !changes.structure && !changes.lights && changes.value_only(), "{changes:?}");
    assert_eq!(allocations, 0);
}

#[test]
fn a_light_colour_edit_is_value_only() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    def.lighting.lights.get_mut(&LightId::new("key").unwrap()).unwrap().colour = Colour::rgb(255, 128, 128);
    let proof = checked(&def);
    let (changes, allocations) = counted(|| out.resolve_into(proof, &figures, ASPECT).unwrap());
    assert!(changes.lights && !changes.structure && !changes.cameras && changes.value_only(), "{changes:?}");
    assert_eq!(allocations, 0);
}

#[test]
fn adding_a_camera_is_structural() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    def.cameras.insert(camera("side"), CameraDef { yaw: 90.0, ..CameraDef::preset(Framing::Full) });
    let changes = out.resolve_into(checked(&def), &figures, ASPECT).unwrap();
    assert!(changes.structure && changes.cameras && !changes.value_only());
    assert_eq!(out.cameras.len(), 3);
    assert_eq!(out.resolve_into(checked(&def), &figures, ASPECT).unwrap(), Changes::default());
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
    // The check refuses it before resolve sees it.
    assert!(matches!(CheckedScene::new(&def), Err(StageError::Scene { .. })));
    def.scene.camera = None;
    def.cameras.clear();
    assert_eq!(resolved(&def, &figures).active, None);
}

#[test]
fn an_unknown_member_anchor_is_a_scene_error() {
    let mut def = scene();
    def.lighting.lights.insert(LightId::new("ghost").unwrap(), light(Anchor::Member(member("nobody")), Colour::WHITE));
    let Err(StageError::Scene { source }) = CheckedScene::new(&def) else {
        panic!("expected a scene error");
    };
    assert_eq!(source.path, "lighting.lights.ghost.anchor");
}

#[test]
fn a_preset_without_a_figure_is_a_scene_error() {
    let def = scene();
    let Err(StageError::Scene { source }) = ResolvedScene::new().resolve_into(checked(&def), &BTreeMap::new(), ASPECT)
    else {
        panic!("expected a scene error");
    };
    // The cast has a subject but no figure is visible: no stand-in is used.
    assert_eq!(source.path, "scene.subject");
}

#[test]
fn a_preset_whose_member_is_hidden_is_a_scene_error() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    def.cast.insert(member("zed"), cast_member());
    def.cameras.get_mut(&camera("close")).unwrap().subject = Anchor::Member(member("zed"));
    let Err(StageError::Scene { source }) = ResolvedScene::new().resolve_into(checked(&def), &figures, ASPECT) else {
        panic!("expected a scene error");
    };
    assert_eq!(source.path, "cameras.close.subject");
}

#[test]
fn an_error_leaves_the_last_resolve_in_place() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    let before = out.clone();
    // A member that is in the cast but has no figure (hidden), so the check passes and resolve itself refuses.
    def.cast.insert(member("hidden"), cast_member());
    def.cameras.get_mut(&camera("front")).unwrap().subject = Anchor::Member(member("hidden"));
    assert!(out.resolve_into(checked(&def), &figures, ASPECT).is_err());
    assert_eq!(out, before);
}

fn dof(focus: Focus) -> DepthOfFieldDef {
    DepthOfFieldDef { focus, range: 0.5, max_blur: 0.02, bokeh: 0.0, bokeh_threshold: None }
}

#[test]
fn each_row_of_the_mapping() {
    let (mut def, figures) = (scene(), figures([2.0, 0.0]));
    let h = figures.values().next().unwrap().height_m;
    def.scene.backdrop = Backdrop::Colour { colour: Colour::rgb(0, 177, 64) };
    def.lighting.exposure = 1.5;
    def.lighting.bloom = Some(BloomDef { strength: 0.4, radius: 0.3, threshold: 0.8 });
    def.lighting.outline_light = 0.6;
    def.lighting.skin_floor = Some(0.35);
    def.scene.post.tonemap = Tonemap::None;
    def.scene.post.grain = Some(0.1);
    def.scene.post.grade = Some(GradeDef { saturation: 1.2, contrast: 0.9 });
    def.scene.post.depth_of_field = Some(dof(Focus::Metres(3.0)));
    def.scene.ground.radius = Some(2.0);
    def.scene.ground.opacity = 0.5;
    let out = resolved(&def, &figures);
    let green = Colour::rgb(0, 177, 64);
    assert_eq!(out.background, Background::Gradient { top: green, bottom: green }, "shown as authored");
    assert_eq!(out.post.exposure, 1.5);
    assert_eq!(out.post.tonemap, Tonemap::None);
    assert_eq!(out.post.bloom, Some(Bloom { strength: 0.4, radius: 0.3, threshold: 0.8 }));
    assert_eq!(out.post.grain, Some(FilmGrain { strength: 0.1 }));
    assert_eq!(out.post.grade, Some(ColourGrade { saturation: 1.2, contrast: 0.9 }));
    assert_eq!(out.post.depth_of_field, Some(DepthOfField::new(3.0, 0.5 * h, 0.02)));
    assert_eq!(out.outline_light, 0.6);
    assert_eq!(out.skin_floor, Some(0.35));
    assert_eq!((out.ground.kind, out.ground.opacity, out.ground.radius_m), (GroundKind::Shadow, 0.5, 2.0 * h));

    // The lighting's background, and bloom switched off by the scene.
    def.scene.backdrop = Backdrop::Lighting {};
    def.lighting.background = Colour::rgb(10, 20, 30);
    def.scene.post.bloom = false;
    let out = resolved(&def, &figures);
    assert_eq!(out.background, Background::Solid { colour: Colour::rgb(10, 20, 30) });
    assert_eq!(out.post.bloom, None);

    // A subject focus is the depth, along the view axis, of the middle of the head box.
    def.scene.post.depth_of_field = Some(dof(Focus::Subject(FocusSubject::Subject)));
    def.scene.ground.radius = None;
    let out = resolved(&def, &figures);
    let figure = figures.values().next().unwrap();
    let head = figure.landmarks.head.unwrap().centre();
    let camera = out.cameras[out.active.unwrap()].1;
    let expected = -camera.view().transform_point3(figure.root.transform_point3(head)).z;
    assert!((out.post.depth_of_field.unwrap().focus_distance - expected).abs() < 1e-5);
    // The absent radius reaches the farthest member plus 0.83 subject units.
    assert!((out.ground.radius_m - (2.0 + 0.83 * h)).abs() < 1e-5, "{}", out.ground.radius_m);
}

#[test]
fn a_subject_focus_without_a_subject_is_a_scene_error() {
    let mut def = SceneDef::new(SceneSettings::named("Empty"));
    def.scene.post.depth_of_field = Some(dof(Focus::Subject(FocusSubject::Subject)));
    let Err(StageError::Scene { source }) = ResolvedScene::new().resolve_into(checked(&def), &BTreeMap::new(), ASPECT)
    else {
        panic!("expected a scene error");
    };
    assert_eq!(source.path, "scene.post.depth_of_field.focus");
}

/// Edits `def` with `edit` and checks that only the flag `pick` reads is set, and that nothing was allocated.
fn only_flag(edit: fn(&mut SceneDef), pick: fn(&Changes) -> bool) {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    edit(&mut def);
    let proof = checked(&def);
    let (changes, allocations) = counted(|| out.resolve_into(proof, &figures, ASPECT).unwrap());
    assert!(pick(&changes), "{changes:?}");
    let flags = [
        changes.lights,
        changes.cameras,
        changes.structure,
        changes.backdrop,
        changes.post,
        changes.ground,
        changes.skin_floor,
        changes.fog,
        changes.environment,
    ];
    assert_eq!(flags.iter().filter(|f| **f).count(), 1, "only one flag: {changes:?}");
    assert_eq!(allocations, 0);
}

#[test]
fn a_post_edit_sets_only_post_and_allocates_nothing() {
    only_flag(|d| d.lighting.exposure = 2.0, |c| c.post);
    only_flag(|d| d.scene.post.grain = Some(0.2), |c| c.post);
}

#[test]
fn a_backdrop_edit_sets_only_backdrop_and_allocates_nothing() {
    only_flag(|d| d.scene.backdrop = Backdrop::Colour { colour: Colour::WHITE }, |c| c.backdrop);
}

#[test]
fn a_ground_edit_sets_only_ground_and_allocates_nothing() {
    only_flag(|d| d.scene.ground.opacity = 0.6, |c| c.ground);
    only_flag(|d| d.scene.ground.radius = Some(4.0), |c| c.ground);
}

#[test]
fn a_skin_floor_edit_sets_only_skin_floor_and_allocates_nothing() {
    only_flag(|d| d.lighting.skin_floor = Some(0.4), |c| c.skin_floor);
}

#[test]
fn a_transparent_backdrop_is_transparent() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    def.scene.backdrop = Backdrop::Transparent {};
    assert_eq!(resolved(&def, &figures).background, Background::Transparent);
}

#[test]
fn a_rolled_camera_turns_the_picture_clockwise_about_its_lens() {
    let mut def = scene();
    def.cameras.insert(camera("level"), CameraDef::preset(Framing::Face));
    def.cameras.insert(camera("dutch"), CameraDef { roll: 90.0, ..CameraDef::preset(Framing::Face) });
    let figures = figures([0.0, 0.0]);
    let out = resolved(&def, &figures);
    let find = |id: &str| out.cameras.iter().find(|(c, _)| *c == camera(id)).map(|(_, c)| c.transform).unwrap();
    let (level, dutch) = (find("level"), find("dutch"));
    // Same place, same view axis; the screen's right now points where its up did, so the world's up shows on the right:
    // the picture turned a quarter clockwise.
    assert!(level.w_axis.abs_diff_eq(dutch.w_axis, 1e-5));
    assert!(level.z_axis.abs_diff_eq(dutch.z_axis, 1e-5));
    assert!(dutch.x_axis.abs_diff_eq(level.y_axis, 1e-5), "{} vs {}", dutch.x_axis, level.y_axis);
    assert!(dutch.y_axis.abs_diff_eq(-level.x_axis, 1e-5));
    def.cameras.insert(camera("bad"), CameraDef { roll: 200.0, ..CameraDef::preset(Framing::Face) });
    assert!(def.check().is_err(), "a roll past 180 degrees is refused");
}

fn free_stage_camera(position: [f32; 3], target: [f32; 3]) -> CameraDef {
    CameraDef {
        framing: Framing::Free,
        anchor: Some(Anchor::Stage),
        position: Some(position),
        target: Some(target),
        ..CameraDef::preset(Framing::Free)
    }
}

#[test]
fn a_subject_focus_is_view_depth_not_distance() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    def.cameras.clear();
    // Level with the feet and 3 m to the side: the head is off the view axis, so its distance exceeds its depth.
    def.cameras.insert(camera("side"), free_stage_camera([3.0, 0.0, 0.0], [0.0, 0.0, 0.0]));
    def.scene.post.depth_of_field = Some(dof(Focus::Subject(FocusSubject::Subject)));
    let out = resolved(&def, &figures);
    let figure = figures.values().next().unwrap();
    let head = figure.root.transform_point3(figure.landmarks.head.unwrap().centre());
    let focus = out.post.depth_of_field.unwrap().focus_distance;
    assert!((focus - (3.0 - head.x)).abs() < 1e-4, "{focus}");
    assert!(focus < out.cameras[0].1.position().distance(head) - 0.1);
}

#[test]
fn the_default_subject_is_the_first_cast_member_even_when_it_is_hidden() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    // `aoi` sorts before `kiri` but has no figure: the subject is unavailable, not `kiri`, and nothing falls back to
    // the unit figure.
    def.cast.insert(member("aoi"), cast_member());
    def.cameras.clear();
    def.cameras.insert(camera("side"), free_stage_camera([3.0, 0.0, 0.0], [0.0, 0.0, 0.0]));
    def.scene.post.depth_of_field = Some(dof(Focus::Subject(FocusSubject::Subject)));
    let Err(StageError::Scene { source }) = ResolvedScene::new().resolve_into(checked(&def), &figures, ASPECT) else {
        panic!("expected a scene error");
    };
    assert_eq!(source.path, "scene.subject");
    // Naming the visible member as the subject resolves it.
    def.scene.subject = Some(member("kiri"));
    assert!(ResolvedScene::new().resolve_into(checked(&def), &figures, ASPECT).is_ok());
}

#[test]
fn a_scene_that_breaks_a_rule_is_refused() {
    let mut def = scene();
    def.lighting.exposure = f32::NAN;
    assert!(matches!(CheckedScene::new(&def), Err(StageError::Scene { .. })));
    def.lighting.exposure = 1.0;
    def.scene.subject = Some(member("nobody"));
    assert!(matches!(CheckedScene::new(&def), Err(StageError::Scene { .. })));
    def.scene.subject = None;
    assert!(CheckedScene::new(&def).is_ok());
}

#[test]
fn a_subject_free_camera_without_a_cast_uses_the_unit_figure_at_the_origin() {
    let mut def = SceneDef::new(SceneSettings::named("Empty"));
    let free = CameraDef { anchor: Some(Anchor::Subject), ..free_stage_camera([0.0, 0.5, 3.0], [0.0, 0.5, 0.0]) };
    def.cameras.insert(camera("free"), free);
    let out = resolved(&def, &BTreeMap::new());
    assert!((out.cameras[0].1.position() - Vec3::new(0.0, 0.5, 3.0)).length() < 1e-5);
}

#[test]
fn a_free_camera_with_an_unusable_projection_is_refused_and_keeps_the_last_scene() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    let before = out.clone();
    // Finite endpoints whose distance overflows `f32`.
    def.cameras.insert(camera("far"), free_stage_camera([0.0, 0.0, 1e20], [0.0, 0.0, 0.0]));
    assert!(out.resolve_into(checked(&def), &figures, ASPECT).is_err());
    assert_eq!(out, before);
}

#[test]
fn a_ground_edit_is_not_value_only() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &figures);
    def.scene.ground.radius = Some(4.0);
    let changes = out.resolve_into(checked(&def), &figures, ASPECT).unwrap();
    assert!(changes.ground && !changes.structure && !changes.value_only(), "{changes:?}");
}

#[test]
fn a_figure_that_isnt_finite_is_refused_and_keeps_the_last_scene() {
    let (def, good) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &good);
    let before = out.clone();
    let mut bad = good.clone();
    bad.get_mut(&member("kiri")).unwrap().root = Affine3A::from_translation(Vec3::splat(f32::NAN));
    let Err(StageError::Scene { source }) = out.resolve_into(checked(&def), &bad, ASPECT) else {
        panic!("expected a scene error");
    };
    assert_eq!(source.path, "cast.kiri");
    assert_eq!(out, before);
}

#[test]
fn a_free_camera_without_a_subject_reaches_ten_unit_heights_past_its_target() {
    let mut def = SceneDef::new(SceneSettings::named("Empty"));
    def.cameras.insert(camera("free"), free_stage_camera([0.0, 0.0, 3.0], [0.0, 0.0, 0.0]));
    let out = resolved(&def, &BTreeMap::new());
    let Projection::Perspective { far, .. } = out.cameras[0].1.projection else { panic!("perspective") };
    assert!((far - 13.0).abs() < 1e-4, "{far}");
}

#[test]
fn a_figure_root_that_is_not_rigid_is_refused() {
    let (def, good) = (scene(), figures([0.0, 0.0]));
    for root in [
        Affine3A::from_scale(Vec3::ZERO),
        Affine3A::from_scale(Vec3::splat(2.0)),
        Affine3A::from_scale(Vec3::new(1.0, 1.0, -1.0)),
        Affine3A::from_mat3(omuzaki_core::glam::Mat3::from_cols(Vec3::X, Vec3::X, Vec3::Z)),
    ] {
        let mut bad = good.clone();
        bad.get_mut(&member("kiri")).unwrap().root = root;
        let Err(StageError::Scene { source }) = ResolvedScene::new().resolve_into(checked(&def), &bad, ASPECT) else {
            panic!("expected a scene error for {root:?}");
        };
        assert_eq!(source.path, "cast.kiri");
    }
    // A moved, turned figure is rigid.
    assert!(ResolvedScene::new().resolve_into(checked(&def), &figures([1.5, -2.0]), ASPECT).is_ok());
}

/// The first scene error's path, for a resolve that must fail and leave `out` alone.
fn refused_path(out: &mut ResolvedScene, def: &SceneDef, figures: &BTreeMap<MemberId, Figure>) -> String {
    let before = out.clone();
    let result = out.resolve_into(checked(def), figures, ASPECT);
    assert_eq!(*out, before, "an error leaves the last scene in place");
    match result {
        Err(StageError::Scene { source }) => source.path.clone(),
        Err(StageError::Framing { .. }) => "framing".to_string(),
        other => panic!("expected an error, got {other:?}"),
    }
}

#[test]
fn a_pitched_or_rolled_figure_root_is_refused() {
    let (def, good) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &good);
    for root in [Affine3A::from_rotation_x(1.0), Affine3A::from_rotation_z(0.5)] {
        let mut bad = good.clone();
        bad.get_mut(&member("kiri")).unwrap().root = root;
        assert_eq!(refused_path(&mut out, &def, &bad), "cast.kiri");
    }
}

#[test]
fn a_ground_radius_that_overflows_is_refused() {
    let mut def = scene();
    def.cameras.clear();
    let huge = figures([f32::MAX, f32::MAX]);
    let mut out = resolved(&def, &figures([0.0, 0.0]));
    assert_eq!(refused_path(&mut out, &def, &huge), "scene.ground");
}

#[test]
fn nan_landmarks_are_refused_before_the_subject_focus_uses_them() {
    let (mut def, good) = (scene(), figures([0.0, 0.0]));
    def.cameras.clear();
    def.cameras.insert(camera("side"), free_stage_camera([3.0, 0.0, 0.0], [0.0, 0.0, 0.0]));
    def.scene.post.depth_of_field = Some(dof(Focus::Subject(FocusSubject::Subject)));
    let mut out = resolved(&def, &good);
    let mut bad = good.clone();
    let landmarks = &mut bad.get_mut(&member("kiri")).unwrap().landmarks;
    landmarks.head.as_mut().unwrap().min.x = f32::NAN;
    assert_eq!(refused_path(&mut out, &def, &bad), "cast.kiri");
}

/// Extreme finite and non-finite numbers in every figure field and every position: a resolve either fails and keeps
/// the last scene, or succeeds with nothing non-finite in what it produced.
#[test]
fn extreme_figures_never_resolve_to_non_finite_values() {
    const EXTREMES: [f32; 8] = [0.0, -1.0, 1e-30, 1e30, f32::MAX, f32::MIN, f32::NAN, f32::INFINITY];
    let mut def = scene();
    def.cameras.insert(camera("free"), free_stage_camera([0.0, 0.5, 3.0], [0.0, 0.5, 0.0]));
    def.scene.post.depth_of_field = Some(dof(Focus::Subject(FocusSubject::Subject)));
    let good = figures([0.0, 0.0]);
    let mut out = resolved(&def, &good);
    let mut tried = Vec::new();
    for &value in &EXTREMES {
        let mut cases = vec![figures([value, 0.0]), figures([value, value]), figures([2.0, value])];
        let edits: [fn(&mut Figure, f32); 5] = [
            |f, v| f.height_m = v,
            |f, v| f.landmarks.height_m = v,
            |f, v| f.landmarks.body.max.y = v,
            |f, v| f.landmarks.head.as_mut().unwrap().min.z = v,
            |f, v| f.root = Affine3A::from_translation(Vec3::splat(v)),
        ];
        for edit in edits {
            let mut figures = good.clone();
            edit(figures.get_mut(&member("kiri")).unwrap(), value);
            cases.push(figures);
        }
        tried.extend(cases);
    }
    for figures in &tried {
        let before = out.clone();
        match out.resolve_into(checked(&def), figures, ASPECT) {
            Ok(_) => {
                let text = format!("{out:?}");
                assert!(!text.contains("NaN") && !text.contains("inf"), "{figures:?} gave {text}");
                for (_, camera) in &out.cameras {
                    camera.validate().expect("a resolved camera is usable");
                }
                out = resolved(&def, &good);
            }
            Err(_) => assert_eq!(out, before, "{figures:?}"),
        }
    }
}

#[test]
fn a_point_light_range_that_underflows_to_zero_is_refused() {
    // 1e-30 m of range on a figure 1e-30 m tall is 1e-60 m: zero in f32, which the render contract rejects (#2020).
    let mut def = scene();
    let point = LightDef {
        shape: LightShape::Point { position: [0.0, 0.5, 0.5], range: 1e-30 },
        ..light(Anchor::Subject, Colour::WHITE)
    };
    def.lighting.lights.insert(LightId::new("lamp").unwrap(), point);
    let mut out = resolved(&def, &figures([0.0, 0.0]));
    let mut tiny = figures([0.0, 0.0]);
    tiny.get_mut(&member("kiri")).unwrap().height_m = 1e-30;
    assert_eq!(refused_path(&mut out, &def, &tiny), "lighting.lights");
    // The same lamp at a normal height resolves with a positive range.
    assert!(resolved(&def, &figures([0.0, 0.0])).lights.as_slice().iter().all(|l| match l {
        omuzaki_render_api::Light::Point { range, .. } => *range > 0.0,
        _ => true,
    }));
}

#[test]
fn a_figure_root_lifted_in_y_is_refused() {
    let (def, good) = (scene(), figures([0.0, 0.0]));
    let mut out = resolved(&def, &good);
    for lift in [0.5, -0.5, 1e-3] {
        let mut bad = good.clone();
        bad.get_mut(&member("kiri")).unwrap().root = Affine3A::from_translation(Vec3::new(0.0, lift, 0.0));
        assert_eq!(refused_path(&mut out, &def, &bad), "cast.kiri", "lift {lift}");
    }
    // A float-sized wobble is not a lift.
    let mut level = good.clone();
    level.get_mut(&member("kiri")).unwrap().root = Affine3A::from_translation(Vec3::new(1.0, 1e-6, 1.0));
    assert!(out.resolve_into(checked(&def), &level, ASPECT).is_ok());
}

// ---- phase 2: fog, the environment and its backdrop (#931) ----

fn fog() -> FogDef {
    FogDef { colour: Colour::hex(0x002a_2238), near: 2.0, far: 9.0 }
}

#[test]
fn fog_is_in_subject_units_at_two_statures() {
    let mut def = scene();
    def.lighting.fog = Some(fog());
    for height_m in [0.966, 1.714] {
        let mut figures = figures([0.0, 0.0]);
        for figure in figures.values_mut() {
            figure.height_m = height_m;
        }
        let got = resolved(&def, &figures).fog.unwrap();
        let want = Fog { colour: Colour::hex(0x002a_2238), near: 2.0 * height_m, far: 9.0 * height_m };
        assert!(
            got.colour == want.colour && (got.near - want.near).abs() < 1e-5 && (got.far - want.far).abs() < 1e-5,
            "{height_m} m: {got:?} vs {want:?}"
        );
    }
}

#[test]
fn the_environment_backdrop_shows_the_environment_or_the_rigs_colour_without_one() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    def.scene.backdrop = Backdrop::Environment {};
    def.lighting.background = Colour::hex(0x0040_3020);
    assert_eq!(resolved(&def, &figures).background, Background::Solid { colour: Colour::hex(0x0040_3020) });
    def.lighting.environment = Some(EnvironmentDef { intensity: 0.6, rotation: 45.0, ..EnvironmentDef::default() });
    let out = resolved(&def, &figures);
    assert_eq!(out.background, Background::Environment);
    let environment = out.environment.unwrap();
    assert_eq!((environment.intensity, environment.rotation_deg, environment.toon_ramp), (0.6, 45.0, true));
    assert_eq!(environment.source, EnvironmentDef::default().source);
}

#[test]
fn fog_and_environment_values_are_value_only_and_allocate_nothing() {
    fn with_environment(d: &mut SceneDef) {
        d.lighting.environment = Some(EnvironmentDef::default());
        d.lighting.fog = Some(fog());
    }
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    with_environment(&mut def);
    let mut out = resolved(&def, &figures);
    for (edit, pick) in [
        (
            (|d: &mut SceneDef| d.lighting.fog.as_mut().unwrap().far = 12.0) as fn(&mut SceneDef),
            (|c: &Changes| c.fog) as fn(&Changes) -> bool,
        ),
        (|d| d.lighting.environment.as_mut().unwrap().intensity = 0.3, |c| c.environment && !c.sky),
        (|d| d.lighting.environment.as_mut().unwrap().rotation = 90.0, |c| c.environment && !c.sky),
    ] {
        edit(&mut def);
        let proof = checked(&def);
        let (changes, allocations) = counted(|| out.resolve_into(proof, &figures, ASPECT).unwrap());
        assert!(pick(&changes) && changes.value_only(), "{changes:?}");
        assert_eq!(allocations, 0);
    }
}

#[test]
fn a_new_sky_is_not_value_only() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    def.lighting.environment = Some(EnvironmentDef::default());
    let mut out = resolved(&def, &figures);
    def.lighting.environment = Some(EnvironmentDef {
        source: EnvironmentSource::Sky { zenith: Colour::WHITE, horizon: Colour::WHITE, ground: Colour::BLACK },
        ..EnvironmentDef::default()
    });
    let changes = out.resolve_into(checked(&def), &figures, ASPECT).unwrap();
    assert!(changes.environment && changes.sky && !changes.value_only(), "{changes:?}");
    def.lighting.environment = None;
    let changes = out.resolve_into(checked(&def), &figures, ASPECT).unwrap();
    assert!(changes.sky && out.environment.is_none(), "removing it is a change of source too");
}

#[test]
fn a_gradient_and_an_image_resolve_to_pictures_with_the_rigs_colour_in_the_bars_by_default() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let (top, bottom) = (Colour::hex(0x001d_2b4f), Colour::hex(0x00e8_a87c));
    def.scene.backdrop = Backdrop::Gradient { top, bottom };
    assert_eq!(resolved(&def, &figures).background, Background::Gradient { top, bottom });

    let asset = omuzaki_store_api::ContentHash::of(b"a picture");
    def.lighting.background = Colour::hex(0x0040_3020);
    def.scene.backdrop = Backdrop::Image { asset, fit: BackdropFit::Contain, bars: None };
    let out = resolved(&def, &figures);
    let rig = Colour::hex(0x0040_3020);
    assert_eq!(out.background, Background::Gradient { top: rig, bottom: rig }, "the bars until the stage has it");
    let image = out.backdrop_image.unwrap();
    assert_eq!((image.asset, image.fit, image.top, image.bottom), (asset, BackdropFit::Contain, rig, rig));

    def.scene.backdrop = Backdrop::Image { asset, fit: BackdropFit::Cover, bars: Some(Bars { top, bottom }) };
    let image = resolved(&def, &figures).backdrop_image.unwrap();
    assert_eq!((image.fit, image.top, image.bottom), (BackdropFit::Cover, top, bottom));
}

#[test]
fn an_images_fit_and_bars_are_values_and_its_asset_is_not() {
    let (mut def, figures) = (scene(), figures([0.0, 0.0]));
    let asset = omuzaki_store_api::ContentHash::of(b"a picture");
    def.scene.backdrop = Backdrop::Image { asset, fit: BackdropFit::Cover, bars: None };
    let mut out = resolved(&def, &figures);
    def.scene.backdrop = Backdrop::Image { asset, fit: BackdropFit::Contain, bars: None };
    let proof = checked(&def);
    let (changes, allocations) = counted(|| out.resolve_into(proof, &figures, ASPECT).unwrap());
    assert!(changes.backdrop && !changes.image && changes.value_only(), "{changes:?}");
    assert_eq!(allocations, 0);

    let other = omuzaki_store_api::ContentHash::of(b"another picture");
    def.scene.backdrop = Backdrop::Image { asset: other, fit: BackdropFit::Contain, bars: None };
    let changes = out.resolve_into(checked(&def), &figures, ASPECT).unwrap();
    assert!(changes.image && !changes.value_only(), "{changes:?}");
    def.scene.backdrop = Backdrop::Transparent {};
    let changes = out.resolve_into(checked(&def), &figures, ASPECT).unwrap();
    assert!(changes.image && out.backdrop_image.is_none(), "removing it is a change of picture too");
}
