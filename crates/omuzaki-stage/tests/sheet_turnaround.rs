//! The turnaround sheet (EX-10): four views of the mock character, framing metadata, tile stitching and the poses.
//!
//! The renderer is the render mock, which is bit-exact, so tiled and single renders are compared strictly. With the
//! `wgpu` feature, the last test draws the same sheet on the software adapter (lavapipe).
//!
//! Set `OMUZAKI_STAGE_PNG=/some/dir` to write each view as a PNG.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation
)]

use std::sync::{Arc, Mutex};

use omuzaki_anim_api::HumanoidBone;
use omuzaki_anim_api::Tick;
use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, DynamicMesh, JointRef, PartRuntime,
    PoseSource, RegistryBuilder, RuntimeCost, StepCtx, VariantId, VariantSet, resolve_variant,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_face::FaceSystem;
use omuzaki_face_api::{FaceDrawing, FaceError, FaceInput, FaceLayerSpec, FacePainter};
use omuzaki_face_mock::{MockPainter, MockRasterizer};
use omuzaki_render_api::{
    AlphaEncoding, Background, Bloom, Camera, Capabilities, ColourGrade, DepthOfField, EnvironmentDesc, EnvironmentUse,
    FilmGrain, Fog, FrameReport, Image, Light, Material, MaterialId, MeshData, MeshId, MeshOptions, Painter, Post,
    Projection, Quality, RenderError, Renderer, ResourceRef, ResourceStats, Scene, TargetDesc, TargetId, TextureDesc,
    TextureId, Tonemap,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::frame::face::{CharacterSource, FaceRig, InlineExecutor};
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::sheet::turnaround::{
    EYE_LINE_FRACTION, HEAD_CENTRE_FRACTION, Lens, MARGIN, SHEET_TICK, SheetPose, TurnaroundError, TurnaroundSheet,
    TurnaroundSpec, View, hero_pose, render_turnaround,
};
use omuzaki_stage::{StageError, UploadOptions};

const CELL: UVec2 = UVec2::new(120, 240);

fn built() -> BuiltCharacter {
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("mock").expect("a valid id"), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

fn stage_on(renderer: &mut dyn Renderer) -> Stage {
    stage_of(renderer, built())
}

/// Wraps a runtime with a history the mock hair lacks: its first mesh drifts sideways by a little for every step since
/// the last reset, as a spring chain carries its swing. Only a reset (or the same number of steps) gives the same shape.
struct Drifting {
    inner: Box<dyn PartRuntime>,
    /// Metres of drift per step.
    per_step: Vec3,
    steps: u32,
    shifted: Vec<Vec3>,
    revision: u64,
}

impl PartRuntime for Drifting {
    fn step(&mut self, ctx: &StepCtx<'_>) {
        self.inner.step(ctx);
        self.steps += 1;
        self.revision += 1;
        if let Some(view) = self.inner.dynamic_mesh(0) {
            let drift = self.per_step * self.steps as f32;
            self.shifted.clear();
            self.shifted.extend(view.positions.iter().map(|p| *p + drift));
        }
    }

    fn reset(&mut self, pose: &dyn PoseSource) {
        self.inner.reset(pose);
        self.steps = 0;
        self.shifted.clear();
        self.revision += 1;
    }

    fn dynamic_count(&self) -> usize {
        self.inner.dynamic_count()
    }

    fn dynamic_mesh(&self, index: usize) -> Option<DynamicMesh<'_>> {
        let view = self.inner.dynamic_mesh(index)?;
        if index == 0 && !self.shifted.is_empty() {
            return Some(DynamicMesh { positions: &self.shifted, revision: self.revision, ..view });
        }
        Some(DynamicMesh { revision: view.revision + self.revision, ..view })
    }

    fn cost(&self) -> RuntimeCost {
        self.inner.cost()
    }
}

/// The mock character with every runtime wrapped in [`Drifting`] sideways.
fn drifting() -> BuiltCharacter {
    drifting_by(Vec3::X * 1e-3)
}

/// The mock character with every runtime wrapped in [`Drifting`], `per_step` metres per step.
fn drifting_by(per_step: Vec3) -> BuiltCharacter {
    let mut character = built();
    for slot in &mut character.runtimes {
        let placeholder: Box<dyn PartRuntime> =
            Box::new(Drifting { inner: Box::new(NoRuntime), per_step, steps: 0, shifted: Vec::new(), revision: 0 });
        let inner = std::mem::replace(&mut slot.runtime, placeholder);
        slot.runtime = Box::new(Drifting { inner, per_step, steps: 0, shifted: Vec::new(), revision: 0 });
    }
    character
}

/// Stands in while a runtime is moved out of its slot.
struct NoRuntime;

impl PartRuntime for NoRuntime {
    fn step(&mut self, _: &StepCtx<'_>) {}
    fn reset(&mut self, _: &dyn PoseSource) {}
    fn dynamic_count(&self) -> usize {
        0
    }
    fn dynamic_mesh(&self, _: usize) -> Option<DynamicMesh<'_>> {
        None
    }
    fn cost(&self) -> RuntimeCost {
        RuntimeCost::default()
    }
}

fn stage_of(renderer: &mut dyn Renderer, built: BuiltCharacter) -> Stage {
    stage_clocked(renderer, built, StageClock::fixed(60.0))
}

fn stage_clocked(renderer: &mut dyn Renderer, built: BuiltCharacter, clock: StageClock) -> Stage {
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let options = UploadOptions { face_px_per_m: 2000.0 };
    Stage::new(renderer, built, options, Scene::new(Camera::default()), target, Quality::live(), clock)
        .expect("the mock character uploads")
}

/// The mock renderer that can be told to refuse new targets.
struct NoTargets {
    inner: MockRenderer,
    refuse: bool,
    refuse_release: bool,
    /// Updates seen so far, and the one (counting from 0) at which `update_vertices` starts to fail.
    updates: usize,
    fail_updates_from: Option<usize>,
}

impl NoTargets {
    fn new() -> Self {
        Self { inner: MockRenderer::new(), refuse: false, refuse_release: false, updates: 0, fail_updates_from: None }
    }
}

impl Renderer for NoTargets {
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    fn stats(&self) -> ResourceStats {
        self.inner.stats()
    }
    fn create_mesh(&mut self, mesh: &MeshData) -> Result<MeshId, RenderError> {
        self.inner.create_mesh(mesh)
    }
    fn create_mesh_with(&mut self, mesh: &MeshData, options: &MeshOptions) -> Result<MeshId, RenderError> {
        self.inner.create_mesh_with(mesh, options)
    }
    fn update_vertices(&mut self, mesh: MeshId, positions: &[Vec3], normals: &[Vec3]) -> Result<(), RenderError> {
        self.updates += 1;
        if self.fail_updates_from.is_some_and(|from| self.updates > from) {
            return Err(RenderError::Backend("vertex update refused by the test".to_owned()));
        }
        self.inner.update_vertices(mesh, positions, normals)
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
        if self.refuse {
            return Err(RenderError::Backend("refused by the test".to_owned()));
        }
        self.inner.create_target(desc)
    }
    fn resize_target(&mut self, target: TargetId, size: UVec2) -> Result<(), RenderError> {
        self.inner.resize_target(target, size)
    }
    fn release(&mut self, resource: ResourceRef) -> Result<(), RenderError> {
        if self.refuse_release {
            return Err(RenderError::Backend("release refused by the test".to_owned()));
        }
        self.inner.release(resource)
    }
    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError> {
        self.inner.render(target, scene, quality)
    }
    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        self.inner.read_target(target)
    }
}

/// Whether two scenes have the same render settings. The sheet's ticks refill the instances and the frame number, so
/// those are not the caller's to get back.
fn same_settings(a: &Scene, b: &Scene) -> bool {
    (a.camera, &a.lights, a.background, a.fog, a.environment, a.outline_light, a.post)
        == (b.camera, &b.lights, b.background, b.fog, b.environment, b.outline_light, b.post)
}

fn spec() -> TurnaroundSpec {
    TurnaroundSpec::new(CELL).with_lens(Lens::Orthographic).with_settle_steps(4)
}

/// Runs `f` on a stage over a fresh mock renderer, then gives the stage's resources back.
fn with_stage<R>(f: impl FnOnce(&mut Stage, &mut MockRenderer) -> R) -> R {
    with_built(built(), f)
}

/// [`with_stage`] for a character other than the plain mock.
fn with_built<R>(character: BuiltCharacter, f: impl FnOnce(&mut Stage, &mut MockRenderer) -> R) -> R {
    let mut renderer = MockRenderer::new();
    let mut stage = stage_of(&mut renderer, character);
    let result = f(&mut stage, &mut renderer);
    stage.release(&mut renderer).expect("the stage releases");
    result
}

fn sheet(spec: &TurnaroundSpec) -> TurnaroundSheet {
    with_stage(|stage, renderer| render_turnaround(stage, renderer, spec).expect("the sheet renders"))
}

/// Whether the pixel is part of the character.
fn opaque(image: &Image, x: u32, y: u32) -> bool {
    image.pixel(x, y).is_some_and(|p| p[3] > 127)
}

/// The first and last rows with an opaque pixel.
fn extent(image: &Image) -> (u32, u32) {
    let rows: Vec<u32> = (0..image.size.y).filter(|&y| (0..image.size.x).any(|x| opaque(image, x, y))).collect();
    (*rows.first().expect("something is drawn"), *rows.last().expect("something is drawn"))
}

fn dump(name: &str, image: &Image) {
    if let Some(dir) = std::env::var_os("OMUZAKI_STAGE_PNG") {
        let png = image::RgbaImage::from_raw(image.size.x, image.size.y, image.rgba8.clone()).expect("sized");
        png.save(std::path::Path::new(&dir).join(format!("{name}.png"))).expect("the PNG is written");
    }
}

#[test]
fn four_views_in_order_with_one_scale() {
    let sheet = sheet(&spec());
    assert_eq!(sheet.views.iter().map(|v| v.view).collect::<Vec<_>>(), View::ALL);
    let scale = sheet.views[0].framing.px_per_m;
    for v in &sheet.views {
        assert_eq!(v.image.size, CELL);
        assert_eq!(v.framing.px_per_m.to_bits(), scale.to_bits(), "{:?} is shot at the sheet's scale", v.view);
        dump(&format!("turnaround-{:?}", v.view), &v.image);
    }
}

#[test]
fn framing_metadata_matches_the_pixels_and_the_known_head() {
    let built = built();
    let (sole, crown) = built
        .meshes
        .iter()
        .flat_map(|m| m.mesh.geometry.positions.iter())
        .fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
    let head = built.rig.skeleton.humanoid_bone(HumanoidBone::Head).expect("a head bone");
    let joint = built.rig.inverse_bind()[head.index()].inverse().transform_point3(Vec3::ZERO);
    let ppm = CELL.y as f32 / ((crown - sole) * (1.0 + 2.0 * MARGIN));
    let mid = f32::midpoint(crown, sole);
    let row = |y: f32| 0.5 * CELL.y as f32 - (y - mid) * ppm;

    let front = &sheet(&spec()).views[0];
    let f = front.framing;
    assert!((f.px_per_m - ppm).abs() < 1e-3 * ppm, "{} vs {ppm}", f.px_per_m);
    assert!((f.ground_row - row(sole)).abs() < 1e-2 && (f.crown_row - row(crown)).abs() < 1e-2);
    let head_y = joint.y + HEAD_CENTRE_FRACTION * (crown - joint.y);
    assert!((f.head_centre.y - row(head_y)).abs() < 1.0, "head row {} vs {}", f.head_centre.y, row(head_y));
    assert!((f.head_centre.x - (0.5 * CELL.x as f32 + joint.x * ppm)).abs() < 1.0);
    let eye_y = joint.y + EYE_LINE_FRACTION * (crown - joint.y);
    assert!((f.eye_line_row - row(eye_y)).abs() < 1.0);
    assert!(f.eye_line_row < f.head_centre.y, "the eyes are above the middle of the head");

    // The rows are where the pixels are: the crown and the soles touch the rows the metadata reports.
    let (top, bottom) = extent(&front.image);
    assert!((top as f32 - f.crown_row).abs() <= 1.5, "crown {top} vs {}", f.crown_row);
    assert!((bottom as f32 + 1.0 - f.ground_row).abs() <= 1.5, "soles {bottom} vs {}", f.ground_row);
}

/// Silhouette pixels of `a` with no silhouette pixel of `mirror` (flipped left to right) within one pixel: the
/// rasteriser's fill rule is not mirror-symmetric, so edges may move by a pixel but shapes may not differ.
fn mirror_misses(a: &Image, mirror: &Image) -> u32 {
    let near = |x: u32, y: u32| {
        let (mx, my) = (mirror.size.x as i32 - 1 - x as i32, y as i32);
        (-1..=1).any(|dy| (-1..=1).any(|dx| opaque(mirror, (mx + dx).max(0) as u32, (my + dy).max(0) as u32)))
    };
    (0..a.size.y)
        .flat_map(|y| (0..a.size.x).map(move |x| (x, y)))
        .filter(|&(x, y)| opaque(a, x, y) && !near(x, y))
        .count() as u32
}

#[test]
fn back_mirrors_front_and_the_front_mirrors_itself() {
    let sheet = sheet(&spec());
    let (front, back) = (&sheet.views[0].image, &sheet.views[3].image);
    // The mock's hair tips settle a little differently on the two sides (under 0.3% of the cell), so allow 1%.
    let limit = CELL.x * CELL.y / 100;
    assert!(mirror_misses(front, back) <= limit, "the front has silhouette the back lacks");
    assert!(mirror_misses(back, front) <= limit, "the back has silhouette the front lacks");
    assert!(mirror_misses(front, front) <= limit, "left and right differ");
}

/// Pixels of `a` and `b` that differ by more than 2 in any channel, as `(all, away from tile borders)`. A pixel is on
/// a border when it is within 2 pixels of a line where tiles of `tile` pixels meet.
fn stitch_errors(a: &Image, b: &Image, tile: u32) -> (usize, usize) {
    let border = |v: u32| v % tile < 2 || tile - v % tile <= 2;
    let mut bad = (0, 0);
    for y in 0..a.size.y {
        for x in 0..a.size.x {
            if a.pixel(x, y).zip(b.pixel(x, y)).is_some_and(|(p, q)| p.iter().zip(q).any(|(c, d)| c.abs_diff(d) > 2)) {
                bad.0 += 1;
                bad.1 += usize::from(!border(x) && !border(y));
            }
        }
    }
    bad
}

/// #203's limit: at most 0.1% bad pixels, and `inside_allowed` of them away from the tile borders (none on a
/// bit-exact renderer).
fn assert_stitched(single: &TurnaroundSheet, tiled: &TurnaroundSheet, tile: u32, inside_allowed: usize) {
    for (a, b) in single.views.iter().zip(&tiled.views) {
        let (all, inside) = stitch_errors(&a.image, &b.image, tile);
        let limit = (CELL.x * CELL.y) as usize / 1000;
        assert!(inside <= inside_allowed, "{:?}: {inside} bad pixels away from the tile borders", a.view);
        assert!(all <= limit, "{:?}: {all} bad pixels (limit {limit})", a.view);
    }
}

#[test]
fn tiles_stitch_to_the_single_render() {
    let single = sheet(&spec());
    let tiled = sheet(&spec().with_max_tile(48));
    for (a, b) in single.views.iter().zip(&tiled.views) {
        assert_eq!(a.framing, b.framing, "tiling does not move the character");
    }
    assert_stitched(&single, &tiled, 48, 0);
}

#[test]
fn a_perspective_cell_that_needs_tiles_says_so() {
    let spec = TurnaroundSpec::new(CELL).with_lens(Lens::Telephoto).with_max_tile(64);
    with_stage(|stage, renderer| {
        let error = render_turnaround(stage, renderer, &spec).expect_err("a perspective sub-window is off-axis");
        assert!(matches!(error, TurnaroundError::TilingNeedsOrthographic { tile: 64, .. }), "{error}");
        assert_eq!(renderer.stats().targets, 1, "only the stage's own target is left");
    });
}

#[test]
fn the_long_lens_renders_and_the_scene_is_put_back() {
    let spec = TurnaroundSpec::new(CELL).with_lens(Lens::Telephoto).with_settle_steps(4);
    let sheet = with_stage(|stage, renderer| {
        let before = stage.scene().clone();
        let sheet = render_turnaround(stage, renderer, &spec).expect("the sheet renders");
        assert!(same_settings(stage.scene(), &before), "the caller's settings are back");
        sheet
    });
    let (top, bottom) = extent(&sheet.views[0].image);
    assert!(top > 0 && bottom < CELL.y - 1, "the margin keeps the character inside the cell");
}

#[test]
fn a_refused_target_leaves_the_callers_scene_alone() {
    let mut renderer = NoTargets::new();
    let mut stage = stage_on(&mut renderer);
    // Lights, post and background the sheet would replace, so a leak would show.
    let scene = stage.scene_mut();
    scene.lights = vec![Light::Hemisphere { sky: Colour::WHITE, ground: Colour::BLACK, intensity: 0.3 }];
    scene.background = Background::Solid { colour: Colour::WHITE };
    scene.post.exposure = 0.7;
    let before = stage.scene().clone();
    renderer.refuse = true;
    let error = render_turnaround(&mut stage, &mut renderer, &spec()).expect_err("the target is refused");
    assert!(matches!(error, TurnaroundError::Stage(_)), "{error}");
    assert_eq!(*stage.scene(), before, "camera, lights, post and background come back unchanged");
    renderer.refuse = false;
    stage.release(&mut renderer).expect("the stage releases");
}

#[test]
fn the_callers_fog_does_not_tint_the_sheet() {
    let clear = sheet(&spec());
    let (foggy, restored) = with_stage(|stage, renderer| {
        stage.scene_mut().fog = Some(Fog { colour: Colour::WHITE, near: 0.0, far: 0.5 });
        let before = stage.scene().clone();
        let sheet = render_turnaround(stage, renderer, &spec()).expect("the sheet renders");
        (sheet, same_settings(stage.scene(), &before))
    });
    assert!(restored, "the caller's fog is back after the sheet");
    for (a, b) in clear.views.iter().zip(&foggy.views) {
        assert_eq!(a.image.rgba8, b.image.rgba8, "{:?} is shot without fog", a.view);
    }
}

#[test]
fn the_callers_environment_light_does_not_light_the_sheet() {
    let clear = sheet(&spec());
    let (lit, restored) = with_stage(|stage, renderer| {
        let sky = EnvironmentDesc::Sky { zenith: Colour::WHITE, horizon: Colour::WHITE, ground: Colour::WHITE };
        let id = renderer.create_environment(&sky).expect("the mock makes a sky");
        let mut use_ = EnvironmentUse::new(id);
        use_.intensity = 4.0;
        stage.scene_mut().environment = Some(use_);
        let before = stage.scene().clone();
        let sheet = render_turnaround(stage, renderer, &spec()).expect("the sheet renders");
        let restored = same_settings(stage.scene(), &before);
        stage.scene_mut().environment = None;
        renderer.release(id.into()).expect("the sky is released");
        (sheet, restored)
    });
    assert!(restored, "the caller's environment is back after the sheet");
    for (a, b) in clear.views.iter().zip(&lit.views) {
        assert_eq!(a.image.rgba8, b.image.rgba8, "{:?} is lit by the sheet's rig alone", a.view);
    }
}

/// The class behind the fog, environment and outline findings: every render setting the caller can put on the scene.
/// Covers `camera`, `lights`, `background`, `fog`, `environment`, `outline_light` and every field of `post`
/// (`exposure`, `tonemap`, `bloom`, `depth_of_field`, `grain`, `grade`); `frame` is set by the stage on each tick, and
/// `instances`, `joints` and `morph_weights` are the character's own.
#[test]
fn nothing_the_caller_sets_on_the_scene_reaches_the_sheet() {
    let clear = sheet(&spec());
    let (loud, restored) = with_stage(|stage, renderer| {
        let sky = EnvironmentDesc::Sky { zenith: Colour::WHITE, horizon: Colour::WHITE, ground: Colour::WHITE };
        let id = renderer.create_environment(&sky).expect("the mock makes a sky");
        let scene = stage.scene_mut();
        scene.camera = Camera::look_at(
            Vec3::new(3.0, 1.0, 3.0),
            Vec3::ZERO,
            Vec3::Y,
            Projection::Orthographic { height: 9.0, near: 0.1, far: 40.0 },
        );
        scene.lights = vec![Light::Hemisphere { sky: Colour::WHITE, ground: Colour::BLACK, intensity: 0.3 }];
        scene.background = Background::Solid { colour: Colour::WHITE };
        scene.fog = Some(Fog { colour: Colour::WHITE, near: 0.0, far: 0.5 });
        scene.environment = Some(EnvironmentUse::new(id));
        scene.outline_light = 0.2;
        scene.post = Post {
            exposure: 2.5,
            tonemap: Tonemap::None,
            bloom: Some(Bloom { strength: 2.0, radius: 0.5, threshold: 0.1 }),
            depth_of_field: Some(DepthOfField { focus_distance: 1.0, focus_range: 0.1, max_blur: 4.0 }),
            grain: Some(FilmGrain { strength: 0.5 }),
            grade: Some(ColourGrade { saturation: 0.0, contrast: 2.0 }),
        };
        let before = stage.scene().clone();
        let sheet = render_turnaround(stage, renderer, &spec()).expect("the sheet renders");
        let restored = same_settings(stage.scene(), &before);
        stage.scene_mut().environment = None;
        renderer.release(id.into()).expect("the sky is released");
        (sheet, restored)
    });
    assert!(restored, "the caller's scene is back after the sheet");
    for (a, b) in clear.views.iter().zip(&loud.views) {
        assert_eq!(a.image.rgba8, b.image.rgba8, "{:?} is drawn from a neutral scene", a.view);
    }
}

/// The row of the eye line in the front view of a sheet of `built`, from its head joint and its height.
fn expected_eye_row(built: &BuiltCharacter) -> f32 {
    let (sole, crown) = built
        .meshes
        .iter()
        .flat_map(|m| m.mesh.geometry.positions.iter())
        .fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
    let head = built.rig.bone(&JointRef::HEAD).expect("a head bone").index();
    let joint = built.rig.inverse_bind()[head].inverse().transform_point3(Vec3::ZERO);
    let ppm = CELL.y as f32 / ((crown - sole) * (1.0 + 2.0 * MARGIN));
    let eye = joint.y + EYE_LINE_FRACTION * (crown - joint.y);
    0.5 * CELL.y as f32 - (eye - f32::midpoint(crown, sole)) * ppm
}

/// A camera that sees the stage's head at `coverage` of the frame's height: what makes the variant service stage the
/// close-up (as in `frame_face.rs`).
fn camera_for(stage: &Stage, coverage: f64) -> Camera {
    let built = stage.built();
    let face = built.face.as_ref().expect("the mock has a face");
    let head_height = f64::from(face.projection.rect.y1 - face.projection.rect.y0);
    let bone = built.rig.bone(&JointRef::HEAD).expect("a head bone").index();
    let bind = built.rig.inverse_bind()[bone].inverse().w_axis.truncate();
    let head = stage.skin().get(bone).map_or(bind, |skin| skin.transform_point3(bind));
    let distance = head_height / (coverage * 2.0 * (15.0_f64.to_radians()).tan());
    Camera::look_at(
        head + Vec3::new(0.0, 0.0, distance as f32),
        head,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.01, far: 50.0 },
    )
}

/// A stage on the mock character in its wide variant, with a face rig that can switch to the close-up (which has a
/// longer torso, so its rig and height differ). It has run a few frames at wide framing.
fn variant_stage(renderer: &mut dyn Renderer) -> Stage {
    let variants: VariantSet = serde_json::from_str(
        r#"{
            "game": { "shot": { "min": 0.0, "max": 0.35 } },
            "close-up": { "extends": "game", "shot": { "min": 0.35, "max": 5.0 },
                          "overrides": { "parts/body/params/proportions/torso_length": 1.25 } }
        }"#,
    )
    .expect("variants");
    let base = omuzaki_char_mock::mock_character().expect("the fixture parses");
    let id = |s: &str| VariantId::new(s).expect("a valid id");
    let builder = Arc::new(CachingBuilder::new());
    let options = BuildOptions { variant: Some(id("game")), ..BuildOptions::default() };
    let services = BuildServices::default();
    let def = resolve_variant(&base, &variants, &id("game")).expect("the variant resolves");
    let built = builder.build(&CharId::new("mock").expect("an id"), &def, &options, &services).expect("builds");
    let source = CharacterSource::new(builder, base, variants, options, services);
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::splat(32), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let mut stage = Stage::new(
        renderer,
        built,
        UploadOptions { face_px_per_m: 2000.0 },
        Scene::new(Camera::default()),
        target,
        Quality::live(),
        StageClock::fixed(60.0),
    )
    .expect("the stage starts");
    let rig = FaceRig::new(Box::new(|projection, _| {
        FaceSystem::new(Box::new(MockPainter::new(*projection)), Box::new(MockRasterizer))
    }))
    .with_executor(Box::new(InlineExecutor))
    .with_source(source);
    stage.set_face(renderer, rig).expect("the face binds");
    stage.scene_mut().camera = camera_for(&stage, 0.15);
    for _ in 0..5 {
        stage.tick(renderer, &FrameInput::default()).expect("warm-up frame");
    }
    stage
}

/// Stages the close-up on `stage`: the swap happens on its next tick.
fn stage_close_up(stage: &mut Stage, renderer: &mut dyn Renderer) {
    stage.scene_mut().camera = camera_for(stage, 0.8);
    stage.service_variants(renderer, &FrameInput::default()).expect("the close-up is staged");
    assert_eq!(stage.built().variant, Some(VariantId::new("game").expect("an id")), "staged, not yet swapped");
}

#[test]
fn a_variant_staged_before_the_call_is_framed_as_the_new_character() {
    let id = |s: &str| VariantId::new(s).expect("a valid id");
    let mut renderer = MockRenderer::new();
    let mut stage = variant_stage(&mut renderer);
    stage_close_up(&mut stage, &mut renderer);
    let wide = expected_eye_row(stage.built());

    let sheet = render_turnaround(&mut stage, &mut renderer, &spec()).expect("the sheet renders");
    assert_eq!(stage.built().variant, Some(id("close-up")), "the sheet's first tick swapped the variant in");
    let close = expected_eye_row(stage.built());
    assert!((close - wide).abs() > 2.0, "the variants' head joints differ, or this test aims at nothing");
    // The caller's settings are back but the scene still holds the new character's instances: after the retired
    // character is released the stage's own scene must still draw.
    stage.service_variants(&mut renderer, &FrameInput::default()).expect("the retired character is released");
    renderer.render(stage.target(), stage.scene(), &Quality::live()).expect("the stage's scene names live meshes");
    for v in &sheet.views {
        let row = v.framing.eye_line_row;
        assert!(
            (row - close).abs() < 1.0,
            "{:?}: the eye line is on row {row}, the swapped-in character's is {close} (the old one's is {wide})",
            v.view
        );
    }
    stage.service_variants(&mut renderer, &FrameInput::default()).expect("the retired character is released");
    stage.release(&mut renderer).expect("the stage releases");
}

fn pixels(sheet: &TurnaroundSheet) -> Vec<&Vec<u8>> {
    sheet.views.iter().map(|v| &v.image.rgba8).collect()
}

#[test]
fn settle_then_redraw_shows_the_settled_hair() {
    let mut renderer = MockRenderer::new();
    let mut stage = stage_on(&mut renderer);
    let input = FrameInput::default();
    stage.scene_mut().camera = Camera::look_at(
        Vec3::new(0.0, 0.0, 6.0),
        Vec3::ZERO,
        Vec3::Y,
        Projection::Orthographic { height: 3.0, near: 0.1, far: 20.0 },
    );
    stage.tick(&mut renderer, &input).expect("a frame");
    let before = renderer.read_target(stage.target()).expect("the frame");
    stage.settle(37);
    let after = stage.redraw(&mut renderer, &input).expect("the still");
    assert!(after.uploads > 0, "the settled meshes are uploaded: {}", after.uploads);
    let settled = renderer.read_target(stage.target()).expect("the still");
    assert_ne!(before.rgba8, settled.rgba8, "the still shows the settled hair, not the last tick's");
    let again = stage.redraw(&mut renderer, &input).expect("the same still");
    assert_eq!(again.uploads, 0, "nothing moved since");
    assert_eq!(settled.rgba8, renderer.read_target(stage.target()).expect("the still").rgba8);
    stage.release(&mut renderer).expect("the stage releases");
}

#[test]
fn settling_from_rest_forgets_the_frames_before_it() {
    let mut renderer = MockRenderer::new();
    let mut busy = stage_of(&mut renderer, drifting());
    let mut fresh = stage_of(&mut renderer, drifting());
    for stage in [&mut busy, &mut fresh] {
        stage.scene_mut().camera = Camera::look_at(
            Vec3::new(0.0, 0.0, 6.0),
            Vec3::ZERO,
            Vec3::Y,
            Projection::Orthographic { height: 3.0, near: 0.1, far: 20.0 },
        );
    }
    let mut moving = FrameInput::default();
    for frame in 0..25 {
        moving.pose = hero_pose(&busy.built().rig.bind);
        moving.wind = Vec3::new(frame as f32, 0.0, 0.0);
        busy.tick(&mut renderer, &moving).expect("a busy frame");
    }
    let last = FrameInput::default();
    let mut stills = Vec::new();
    for stage in [&mut busy, &mut fresh] {
        stage.tick(&mut renderer, &last).expect("the frame to settle under");
        stage.settle_from_rest(30);
        stage.redraw(&mut renderer, &last).expect("the still");
        stills.push(renderer.read_target(stage.target()).expect("the still").rgba8);
    }
    assert_eq!(stills[0], stills[1], "a stage that was animated settles to the same still as a fresh one");
    busy.release(&mut renderer).expect("released");
    fresh.release(&mut renderer).expect("released");
}

#[test]
fn a_sheet_does_not_depend_on_what_the_stage_did_before() {
    let fresh = with_built(drifting(), |stage, renderer| render_turnaround(stage, renderer, &spec()).expect("a sheet"));
    let (first, second, busy) = with_built(drifting(), |stage, renderer| {
        let first = render_turnaround(stage, renderer, &spec()).expect("the first sheet");
        let second = render_turnaround(stage, renderer, &spec()).expect("the second sheet");
        let mut moving = FrameInput::default();
        for frame in 0..25 {
            moving.pose = hero_pose(&stage.built().rig.bind);
            moving.wind = Vec3::new(frame as f32, 0.0, 0.0);
            stage.tick(renderer, &moving).expect("a busy frame");
        }
        let busy = render_turnaround(stage, renderer, &spec()).expect("the sheet after animation");
        (first, second, busy)
    });
    assert_eq!(pixels(&fresh), pixels(&first), "the first sheet of a used stage is a fresh stage's");
    assert_eq!(pixels(&fresh), pixels(&second), "back to back on one stage");
    assert_eq!(pixels(&fresh), pixels(&busy), "a stage animated beforehand gives the fresh stage's sheet");
    assert_eq!(fresh.views[0].framing, busy.views[0].framing);
}

#[test]
fn a_pending_variant_gives_the_sheet_a_swapped_stage_would() {
    let mut renderer = MockRenderer::new();
    let mut pending = variant_stage(&mut renderer);
    stage_close_up(&mut pending, &mut renderer);
    let mut swapped = variant_stage(&mut renderer);
    stage_close_up(&mut swapped, &mut renderer);
    swapped.tick(&mut renderer, &FrameInput::default()).expect("the swapping tick");
    assert_eq!(swapped.built().variant, Some(VariantId::new("close-up").expect("an id")));
    let a = render_turnaround(&mut pending, &mut renderer, &spec()).expect("the pending stage's sheet");
    let b = render_turnaround(&mut swapped, &mut renderer, &spec()).expect("the swapped stage's sheet");
    assert_eq!(pixels(&a), pixels(&b));
    for stage in [pending, swapped] {
        stage.release(&mut renderer).expect("released");
    }
}

#[test]
fn a_tick_that_fails_after_a_variant_swap_leaves_a_scene_that_names_live_meshes() {
    let mut renderer = NoTargets::new();
    let mut stage = variant_stage(&mut renderer);
    stage_close_up(&mut stage, &mut renderer);
    // The swapping tick makes two vertex updates: the swap's own refresh, then the frame's dynamic upload. The second
    // one fails, after the swap is committed and before the scene is refilled.
    renderer.fail_updates_from = Some(renderer.updates + 1);
    let error = render_turnaround(&mut stage, &mut renderer, &spec()).expect_err("the upload is refused");
    assert!(matches!(error, TurnaroundError::Stage(_)), "{error}");
    renderer.fail_updates_from = None;
    assert_eq!(stage.built().variant, Some(VariantId::new("close-up").expect("an id")), "the swap was committed");
    // The retired character's meshes are released; the scene must not name them.
    stage.service_variants(&mut renderer, &FrameInput::default()).expect("the retired character is released");
    renderer.render(stage.target(), stage.scene(), &Quality::live()).expect("the stage's scene names live meshes");
    stage.release(&mut renderer).expect("the stage releases");
}

#[test]
fn a_spec_with_nothing_to_draw_is_refused_before_any_work() {
    for spec in
        [TurnaroundSpec::new(UVec2::new(0, 240)), TurnaroundSpec::new(UVec2::new(120, 0)), spec().with_max_tile(0)]
    {
        assert!(matches!(spec.validate(), Err(TurnaroundError::Invalid(_))), "{spec:?}");
        with_stage(|stage, renderer| {
            let before = stage.scene().clone();
            let targets = renderer.stats().targets;
            let error = render_turnaround(stage, renderer, &spec).expect_err("the spec is unusable");
            assert!(matches!(error, TurnaroundError::Invalid(_)), "{error}");
            assert_eq!((*stage.scene() == before, renderer.stats().targets), (true, targets), "nothing was touched");
        });
    }
    assert!(spec().validate().is_ok());
}

#[test]
fn a_cell_too_big_to_allocate_is_refused_with_checked_arithmetic() {
    for cell in [UVec2::splat(u32::MAX), UVec2::new(u32::MAX, 2), UVec2::new(40_000, 40_000)] {
        let spec = TurnaroundSpec::new(cell);
        assert!(matches!(spec.validate(), Err(TurnaroundError::Invalid(_))), "{cell}");
        with_stage(|stage, renderer| {
            let error = render_turnaround(stage, renderer, &spec).expect_err("the cell is too big");
            assert!(matches!(error, TurnaroundError::Invalid(_)), "{error}");
        });
    }
    assert!(TurnaroundSpec::new(UVec2::new(3508, 2480)).validate().is_ok(), "a 300 dpi A4 cell is fine");
}

#[test]
fn a_failed_release_never_hides_the_drawing_error() {
    let mut renderer = NoTargets::new();
    let mut stage = stage_on(&mut renderer);
    // Drawing fails (a telephoto cell that needs tiles) and the release fails too: the drawing error comes back.
    let tiles = TurnaroundSpec::new(CELL).with_lens(Lens::Telephoto).with_max_tile(64);
    renderer.refuse_release = true;
    let error = render_turnaround(&mut stage, &mut renderer, &tiles).expect_err("drawing fails");
    assert!(matches!(error, TurnaroundError::TilingNeedsOrthographic { .. }), "{error}");
    // Drawing succeeds and the release fails: that failure is the result.
    let error = render_turnaround(&mut stage, &mut renderer, &spec()).expect_err("the release fails");
    assert!(matches!(error, TurnaroundError::Stage(StageError::Render { .. })), "{error}");
    renderer.refuse_release = false;
    stage.release(&mut renderer).expect("the stage releases");
}

#[test]
fn settling_from_rest_advances_the_springs_by_the_requested_steps_whatever_the_clock() {
    let skins = |clock: StageClock, steps: u32| {
        let mut renderer = MockRenderer::new();
        let mut stage = stage_clocked(&mut renderer, built(), clock);
        let input = FrameInput { dt: 1.0 / 30.0, ..FrameInput::default() };
        for _ in 0..3 {
            stage.tick(&mut renderer, &input).expect("a frame");
        }
        stage.settle_from_rest(steps);
        let skin = stage.skin().to_vec();
        stage.release(&mut renderer).expect("released");
        skin
    };
    for steps in [0, 1, 5] {
        assert_eq!(
            skins(StageClock::fixed(60.0), steps),
            skins(StageClock::live(), steps),
            "{steps} steps move the springs the same on a fixed and a live clock"
        );
    }
    // The mock's hair chain is not inert: steps move it, so the equalities above are not vacuous.
    assert_ne!(skins(StageClock::fixed(60.0), 0), skins(StageClock::fixed(60.0), 5));
}

/// The topmost and bottom-most rows of `built`'s vertices turned by `yaw_deg`, projected by hand through a camera
/// `distance` metres in front of the middle of the character's height (`ortho` or the 12 degree lens), in a cell of
/// `CELL`: an independent calculation of what the sheet reports.
fn rows_by_hand(built: &BuiltCharacter, yaw_deg: f32, ortho: bool) -> (f32, f32) {
    let points: Vec<Vec3> = built.meshes.iter().flat_map(|m| m.mesh.geometry.positions.iter().copied()).collect();
    let (sole, crown) = points.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
    let framed = (crown - sole) * (1.0 + 2.0 * MARGIN);
    let mid = f32::midpoint(crown, sole);
    let half = 6.0_f32.to_radians().tan();
    let distance = 0.5 * framed / half;
    let turn = yaw_deg.to_radians();
    let rows = points.iter().map(|p| {
        let z = -p.x * turn.sin() + p.z * turn.cos();
        let up = if ortho { (p.y - mid) / (0.5 * framed) } else { (p.y - mid) / ((distance - z) * half) };
        0.5 * CELL.y as f32 * (1.0 - up)
    });
    rows.fold((f32::MAX, f32::MIN), |(top, bottom), r| (top.min(r), bottom.max(r)))
}

#[test]
fn the_crown_and_soles_are_projected_where_the_geometry_is_in_every_lens() {
    // The whole character stands 40 cm toward the camera, so its extremes are nowhere near the axis the camera
    // looks along: points on the axis (z = 0) would be rows off in a perspective lens.
    let shifted = || {
        let mut character = built();
        // No runtimes: the hair stays as built, so what the renderer draws is exactly the shifted data.
        character.runtimes.clear();
        for mesh in &mut character.meshes {
            mesh.mesh.dynamic = false;
            for p in &mut mesh.mesh.geometry.positions {
                p.z += 0.4;
            }
        }
        character
    };
    let character = shifted();
    let expected = |view: View, ortho: bool| rows_by_hand(&character, view.yaw_deg(), ortho);
    let mut seen = Vec::new();
    for lens in [Lens::Telephoto, Lens::Orthographic] {
        let sheet = with_built(shifted(), |stage, renderer| {
            render_turnaround(stage, renderer, &spec().with_lens(lens)).expect("the sheet renders")
        });
        for v in &sheet.views {
            let (top, bottom) = expected(v.view, lens == Lens::Orthographic);
            assert!(
                (v.framing.crown_row - top).abs() < 0.05,
                "{lens:?} {:?}: crown {} vs {top}",
                v.view,
                v.framing.crown_row
            );
            assert!(
                (v.framing.ground_row - bottom).abs() < 0.05,
                "{lens:?} {:?}: soles {} vs {bottom}",
                v.view,
                v.framing.ground_row
            );
            seen.push((lens, v.view, v.framing.crown_row));
        }
    }
    // Not vacuous: the telephoto crown moves with the view's depth (front vs back differ), the orthographic one doesn't.
    let crown = |lens, view| seen.iter().find(|s| s.0 == lens && s.1 == view).map(|s| s.2).expect("seen");
    assert!((crown(Lens::Telephoto, View::Front) - crown(Lens::Telephoto, View::Back)).abs() > 2.0);
    assert!((crown(Lens::Orthographic, View::Front) - crown(Lens::Orthographic, View::Back)).abs() < 0.05);
}

/// A face painter that notes the time of every paint.
struct TimeRecorder {
    inner: MockPainter,
    times: Arc<Mutex<Vec<f64>>>,
}

impl FacePainter for TimeRecorder {
    fn layers(&self) -> &[FaceLayerSpec] {
        self.inner.layers()
    }

    fn paint(&mut self, input: &FaceInput<'_>, out: &mut FaceDrawing) -> Result<(), FaceError> {
        self.times.lock().expect("the notes").push(input.time.0);
        self.inner.paint(input, out)
    }
}

#[test]
fn the_sheet_is_drawn_at_one_time_whatever_the_clock_and_the_frames_before() {
    let run = |clock: StageClock, prior: u32| {
        let mut renderer = MockRenderer::new();
        let times = Arc::new(Mutex::new(Vec::new()));
        let mut stage = stage_clocked(&mut renderer, built(), clock);
        let noted = Arc::clone(&times);
        let rig = FaceRig::new(Box::new(move |projection, _| {
            let painter = TimeRecorder { inner: MockPainter::new(*projection), times: Arc::clone(&noted) };
            FaceSystem::new(Box::new(painter), Box::new(MockRasterizer))
        }))
        .with_executor(Box::new(InlineExecutor));
        stage.set_face(&mut renderer, rig).expect("the face binds");
        for _ in 0..prior {
            stage.tick(&mut renderer, &FrameInput { dt: 1.0 / 30.0, ..FrameInput::default() }).expect("a frame");
        }
        times.lock().expect("the notes").clear();
        let sheet = render_turnaround(&mut stage, &mut renderer, &spec()).expect("the sheet renders");
        let seen = times.lock().expect("the notes").clone();
        // The frame after the sheet is the one the clock would have given without it.
        let next = stage.tick(&mut renderer, &FrameInput::default()).expect("a frame").tick.index;
        stage.release(&mut renderer).expect("released");
        (sheet, seen, next)
    };
    let (fixed, fixed_times, fixed_next) = run(StageClock::fixed(60.0), 0);
    let (live, live_times, _) = run(StageClock::live(), 12);
    let (slow, slow_times, slow_next) = run(StageClock::fixed(24.0), 7);
    assert!(fixed_times.len() >= 4, "the face is painted for the views: {fixed_times:?}");
    assert!(fixed_times.iter().all(|t| t.to_bits() == SHEET_TICK.time.0.to_bits()), "one sheet time: {fixed_times:?}");
    assert!(live_times.iter().all(|t| t.to_bits() == SHEET_TICK.time.0.to_bits()), "{live_times:?}");
    assert!(slow_times.iter().all(|t| t.to_bits() == SHEET_TICK.time.0.to_bits()), "{slow_times:?}");
    assert_eq!(pixels(&fixed), pixels(&live), "a fixed clock and a live clock with earlier frames agree");
    assert_eq!(pixels(&fixed), pixels(&slow));
    assert_eq!(fixed_next, 0, "the sheet did not move the clock: the next frame is the first");
    assert_eq!(slow_next, 7, "and after seven frames the next one is the eighth");
}

/// The first and last rows with an opaque pixel of the first view, as `(top, bottom + 1)`.
fn silhouette(sheet: &TurnaroundSheet) -> (f32, f32) {
    let (top, bottom) = extent(&sheet.views[0].image);
    (top as f32, bottom as f32 + 1.0)
}

#[test]
fn the_rows_follow_a_runtime_that_deforms_the_hair() {
    let draw = |character: BuiltCharacter| {
        with_built(character, |stage, renderer| render_turnaround(stage, renderer, &spec()).expect("a sheet"))
    };
    let still = draw(drifting_by(Vec3::ZERO));
    // The hair rises 5 mm for each of the four settle steps: 2 cm, a few rows, and the framing was made without it.
    let lifted = draw(drifting_by(Vec3::Y * 0.005));
    let (a, b) = (still.views[0].framing, lifted.views[0].framing);
    assert!(
        (a.crown_row - b.crown_row).abs() > 1.0 || (a.ground_row - b.ground_row).abs() > 1.0,
        "the deformation moves the extremes, or this test aims at nothing: {a:?} vs {b:?}"
    );
    let (top, bottom) = silhouette(&lifted);
    assert!((b.crown_row - top).abs() <= 1.5, "crown {} vs the picture's {top}", b.crown_row);
    assert!((b.ground_row - bottom).abs() <= 1.5, "soles {} vs the picture's {bottom}", b.ground_row);
}

#[test]
fn the_rows_follow_a_pose_that_moves_skinned_geometry() {
    let draw = |pose: SheetPose| {
        with_stage(|stage, renderer| render_turnaround(stage, renderer, &spec().with_pose(pose)).expect("a sheet"))
    };
    let rest = draw(SheetPose::Rest);
    let mut lowered = built().rig.bind;
    lowered.hips_offset = Vec3::new(0.0, -0.02, 0.0);
    let posed = draw(SheetPose::custom(lowered));
    let (a, b) = (rest.views[0].framing, posed.views[0].framing);
    assert!(
        (a.crown_row - b.crown_row).abs() > 1.0 || (a.ground_row - b.ground_row).abs() > 1.0,
        "the pose moves the extremes, or this test aims at nothing: {a:?} vs {b:?}"
    );
    let (top, bottom) = silhouette(&posed);
    assert!((b.crown_row - top).abs() <= 1.5, "crown {} vs the picture's {top}", b.crown_row);
    assert!((b.ground_row - bottom).abs() <= 1.5, "soles {} vs the picture's {bottom}", b.ground_row);
}

#[test]
fn the_sheet_gives_back_an_outer_pin_and_the_frame_info() {
    let mut renderer = MockRenderer::new();
    let mut stage = stage_clocked(&mut renderer, built(), StageClock::fixed(60.0));
    for _ in 0..3 {
        stage.tick(&mut renderer, &FrameInput::default()).expect("a frame");
    }
    let frame = stage.scene().frame;
    assert_eq!(frame.index, 2, "three frames ran");

    // A sheet leaves the scene's frame info as the stage's last frame set it.
    render_turnaround(&mut stage, &mut renderer, &spec()).expect("the sheet renders");
    assert_eq!(stage.scene().frame, frame, "the frame info is the last frame's again");

    // A caller's own pin is still in force after the sheet: the next tick runs at the pinned tick, not the clock's.
    let outer = Tick { index: 99, time: Seconds(9.0), dt: 0.0 };
    let pin = stage.pin_time(outer);
    render_turnaround(&mut stage, &mut renderer, &spec()).expect("the sheet renders under a pin");
    let next = stage.tick(&mut renderer, &FrameInput::default()).expect("a frame").tick;
    assert_eq!(next, outer, "the outer pin survived the sheet");
    stage.unpin_time(&pin);
    let after = stage.tick(&mut renderer, &FrameInput::default()).expect("a frame").tick;
    assert_eq!(after.index, 3, "with the pin gone the clock carries on where it was");
    stage.release(&mut renderer).expect("released");
}

#[test]
fn a_pose_that_leaves_the_frame_fails_closed_and_the_sheet_still_gives_everything_back() {
    let mut renderer = MockRenderer::new();
    let mut stage = stage_clocked(&mut renderer, built(), StageClock::fixed(60.0));
    for _ in 0..2 {
        stage.tick(&mut renderer, &FrameInput::default()).expect("a frame");
    }
    let before = stage.scene().clone();
    let targets = renderer.stats().targets;
    // Sitting: the hips drop half a metre, far below the sole the shot was framed for.
    let mut sitting = built().rig.bind;
    sitting.hips_offset = Vec3::new(0.0, -0.5, 0.0);
    let error = render_turnaround(&mut stage, &mut renderer, &spec().with_pose(SheetPose::custom(sitting)))
        .expect_err("the pose does not fit");
    assert!(matches!(error, TurnaroundError::Unsupported(_)), "{error}");
    assert!(error.to_string().contains("#1854"), "the message names the follow-up: {error}");
    // Every field the sheet saves is back: the settings, the frame info, the pin, the target.
    assert!(same_settings(stage.scene(), &before));
    assert_eq!(stage.scene().frame, before.frame);
    assert_eq!(renderer.stats().targets, targets, "the sheet's target is released");
    let next = stage.tick(&mut renderer, &FrameInput::default()).expect("a frame").tick;
    assert_eq!(next.index, 2, "no pin is left: the clock carries on");
    // A pose that does fit still renders.
    let mut nudged = built().rig.bind;
    nudged.hips_offset = Vec3::new(0.0, -0.01, 0.0);
    render_turnaround(&mut stage, &mut renderer, &spec().with_pose(SheetPose::custom(nudged))).expect("it fits");
    stage.release(&mut renderer).expect("released");
}

#[test]
fn the_hero_pose_differs_from_the_rest_pose_and_is_stable() {
    let rest = sheet(&spec());
    let hero = sheet(&spec().with_pose(SheetPose::Hero));
    assert_eq!(hero.pose, SheetPose::Hero);
    assert_ne!(rest.views[0].image.rgba8, hero.views[0].image.rgba8);
    assert_eq!(hero.views[0].image.rgba8, sheet(&spec().with_pose(SheetPose::Hero)).views[0].image.rgba8);
    let bind = built().rig.bind;
    assert_eq!(hero_pose(&bind), hero_pose(&bind));
    assert_ne!(hero_pose(&bind), bind);
}

#[cfg(feature = "wgpu")]
#[test]
fn the_sheet_renders_on_the_software_adapter() {
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    // A fresh stage for each sheet: hair settled for a few steps would otherwise start the second sheet elsewhere.
    let render = |spec: &TurnaroundSpec| {
        let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
            .expect("a software adapter (install mesa-vulkan-drivers)");
        let mut stage = stage_on(&mut renderer);
        let sheet = render_turnaround(&mut stage, &mut renderer, spec).expect("the sheet renders");
        stage.release(&mut renderer).expect("the stage releases");
        sheet
    };
    let (single, tiled) = (render(&spec()), render(&spec().with_max_tile(48)));
    // On a GPU the moved tile camera rounds a hair strand's edge differently now and then, flipping a sample's
    // coverage in a handful of scattered pixels (7 of 28 800 measured on lavapipe), so only the total is held.
    assert_stitched(&single, &tiled, 48, usize::MAX);
}
