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

use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Capabilities, FrameReport, Image, Light, Material, MaterialId, MeshData, MeshId,
    MeshOptions, Painter, Quality, RenderError, Renderer, ResourceRef, ResourceStats, Scene, TargetDesc, TargetId,
    TextureDesc, TextureId,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::{Stage, StageClock};
use omuzaki_stage::sheet::turnaround::{
    EYE_LINE_FRACTION, HEAD_CENTRE_FRACTION, Lens, MARGIN, SheetPose, TurnaroundError, TurnaroundSheet, TurnaroundSpec,
    View, hero_pose, render_turnaround,
};

const CELL: UVec2 = UVec2::new(120, 240);

fn built() -> BuiltCharacter {
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("mock").expect("a valid id"), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}

fn stage_on(renderer: &mut dyn Renderer) -> Stage {
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let options = UploadOptions { face_px_per_m: 600.0 };
    Stage::new(
        renderer,
        built(),
        options,
        Scene::new(Camera::default()),
        target,
        Quality::live(),
        StageClock::fixed(60.0),
    )
    .expect("the mock character uploads")
}

/// The mock renderer that can be told to refuse new targets.
struct NoTargets {
    inner: MockRenderer,
    refuse: bool,
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
        self.inner.release(resource)
    }
    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError> {
        self.inner.render(target, scene, quality)
    }
    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        self.inner.read_target(target)
    }
}

fn spec() -> TurnaroundSpec {
    TurnaroundSpec::new(CELL).with_lens(Lens::Orthographic).with_settle_steps(4)
}

/// Runs `f` on a stage over a fresh mock renderer, then gives the stage's resources back.
fn with_stage<R>(f: impl FnOnce(&mut Stage, &mut MockRenderer) -> R) -> R {
    let mut renderer = MockRenderer::new();
    let mut stage = stage_on(&mut renderer);
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
    let head = built.rig.skeleton.humanoid_bone(omuzaki_anim_api::HumanoidBone::Head).expect("a head bone");
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

#[test]
fn tiles_stitch_to_the_single_render() {
    let single = sheet(&spec());
    let tiled = sheet(&spec().with_max_tile(48));
    for (a, b) in single.views.iter().zip(&tiled.views) {
        assert_eq!(a.framing, b.framing, "tiling does not move the character");
        let bad = a.image.rgba8.iter().zip(&b.image.rgba8).filter(|(p, q)| p.abs_diff(**q) > 2).count();
        let limit = a.image.rgba8.len() / 1000;
        assert!(bad <= limit, "{:?}: {bad} channels differ by more than 2 (limit {limit})", a.view);
    }
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
        assert_eq!(*stage.scene(), before);
        sheet
    });
    let (top, bottom) = extent(&sheet.views[0].image);
    assert!(top > 0 && bottom < CELL.y - 1, "the margin keeps the character inside the cell");
}

#[test]
fn a_refused_target_leaves_the_callers_scene_alone() {
    let mut renderer = NoTargets { inner: MockRenderer::new(), refuse: false };
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
    for (a, b) in single.views.iter().zip(&tiled.views) {
        let bad = a.image.rgba8.iter().zip(&b.image.rgba8).filter(|(p, q)| p.abs_diff(**q) > 2).count();
        assert!(bad <= a.image.rgba8.len() / 1000, "{:?}: {bad} channels differ after stitching", a.view);
    }
}
