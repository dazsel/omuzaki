//! The walking skeleton, end to end: the mock character, the face painter and the **render mock**, joined by the
//! stage. This is the proof that the three contracts fit: parts build meshes and materials, `StageCharacter` uploads
//! them, the face paints into dynamic textures through the renderer's `Painter`, a material composites them as
//! `Overlay`s, and the renderer produces the picture. (It started life as `omuzaki-char-mock/tests/stage.rs`; a mock may
//! not depend on this App crate, so the test moved here with the upload code it used to carry.)
//!
//! Also here: the upload, release and atlas-resize behaviours of `StageCharacter` on the mock renderer.
//!
//! Set `OMUZAKI_STAGE_PNG=/some/dir` to write the pictures as PNGs.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, MaterialName, PoseSource, RegistryBuilder,
    TextureName,
};
use omuzaki_char_mock::{MockLive, RestPose, mock_character};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_face_api::{DrawList, FaceLayerId, FaceLayerSpec, FaceRasterizer, Raster};
use omuzaki_face_mock::{MockPainter, MockRasterizer};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Capabilities, FrameReport, Image, Instance, JointRange, Light, Material,
    MaterialId, MeshData, MeshId, MeshOptions, PaintTarget, Painter, PixelRect, Projection, Quality, RenderError,
    Renderer, RendererExt, ResourceRef, ResourceStats, Scene, TargetDesc, TargetId, TextureDesc, TextureId,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::{StageCharacter, StageError, UploadOptions};

const SIZE: UVec2 = UVec2::new(360, 360);
/// Face texture density for the test: 1 200 px/m keeps the rasterised layers small.
const FACE_PX_PER_M: f32 = 1200.0;
const UPLOAD: UploadOptions = UploadOptions { face_px_per_m: FACE_PX_PER_M };

/// What `omuzaki-face` will do: wrap a face painter's draw list and a rasteriser in the renderer's `Painter`.
struct FaceLayerPainter<'a> {
    spec: FaceLayerSpec,
    list: &'a DrawList,
}

impl Painter for FaceLayerPainter<'_> {
    fn paint(&mut self, target: &mut dyn PaintTarget) -> Result<(), RenderError> {
        let desc = *target.desc();
        let mut raster = Raster::new(1, 1);
        MockRasterizer
            .rasterize(&self.spec, self.list, self.spec.px_per_m, &mut raster)
            .map_err(|e| RenderError::invalid("face layer", e.to_string()))?;
        assert_eq!((raster.width, raster.height), (desc.size.x, desc.size.y), "the texture is the size the spec says");
        target.write_rgba(PixelRect::full(desc.size), &raster.pixels)
    }
}

/// The mock renderer with two additions: it remembers the most resources that were ever live at once (so a test can
/// tell that a failed upload really had created something before it failed), and it can be told to refuse one
/// `update_material` call.
struct Spy {
    inner: MockRenderer,
    peak: ResourceStats,
    /// `update_material` calls so far.
    updates: usize,
    /// The index (in `updates` order) of the one `update_material` call to refuse.
    refuse_update: Option<usize>,
}

impl Spy {
    fn new() -> Self {
        Self { inner: MockRenderer::new(), peak: ResourceStats::default(), updates: 0, refuse_update: None }
    }

    fn note_peak(&mut self) {
        let now = self.inner.stats();
        self.peak.meshes = self.peak.meshes.max(now.meshes);
        self.peak.textures = self.peak.textures.max(now.textures);
        self.peak.materials = self.peak.materials.max(now.materials);
    }
}

impl Renderer for Spy {
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }

    fn stats(&self) -> ResourceStats {
        self.inner.stats()
    }

    fn create_mesh(&mut self, mesh: &MeshData) -> Result<MeshId, RenderError> {
        let id = self.inner.create_mesh(mesh);
        self.note_peak();
        id
    }

    fn create_mesh_with(&mut self, mesh: &MeshData, options: &MeshOptions) -> Result<MeshId, RenderError> {
        let id = self.inner.create_mesh_with(mesh, options);
        self.note_peak();
        id
    }

    fn update_vertices(&mut self, mesh: MeshId, positions: &[Vec3], normals: &[Vec3]) -> Result<(), RenderError> {
        self.inner.update_vertices(mesh, positions, normals)
    }

    fn create_texture(&mut self, desc: &TextureDesc, rgba: Option<&[u8]>) -> Result<TextureId, RenderError> {
        let id = self.inner.create_texture(desc, rgba);
        self.note_peak();
        id
    }

    fn paint_texture(&mut self, texture: TextureId, painter: &mut dyn Painter) -> Result<(), RenderError> {
        self.inner.paint_texture(texture, painter)
    }

    fn create_material(&mut self, material: &Material) -> Result<MaterialId, RenderError> {
        let id = self.inner.create_material(material);
        self.note_peak();
        id
    }

    fn update_material(&mut self, id: MaterialId, material: &Material) -> Result<(), RenderError> {
        self.updates += 1;
        if self.refuse_update == Some(self.updates - 1) {
            return Err(RenderError::Backend("refused by the test".to_owned()));
        }
        self.inner.update_material(id, material)
    }

    fn create_target(&mut self, desc: &TargetDesc) -> Result<TargetId, RenderError> {
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

/// A minimal stage: a renderer with a character uploaded by [`StageCharacter`], drawn with the shot this test uses.
struct Stage {
    renderer: Spy,
    character: StageCharacter,
    instances: Vec<Instance>,
    joints: Vec<Mat4>,
}

impl Stage {
    fn new(built: &BuiltCharacter) -> Self {
        let mut renderer = Spy::new();
        let character = StageCharacter::upload(&mut renderer, built, UPLOAD).unwrap();
        // Each mesh's joint list becomes a run of the scene's joint buffer.
        let (mut instances, mut joints) = (Vec::new(), Vec::new());
        for built_mesh in &built.meshes {
            let uploaded = character.mesh(&built_mesh.part, built_mesh.local).expect("every mesh was uploaded");
            let mut instance = Instance::new(uploaded.mesh, uploaded.material, Mat4::IDENTITY);
            instance.casts_shadow = built_mesh.mesh.casts_shadow;
            if built_mesh.mesh.geometry.is_skinned() {
                let start = u32::try_from(joints.len()).unwrap();
                joints.extend(
                    built_mesh.mesh.joint_bones.iter().map(|j| RestPose.skin_matrix(j).expect("the pose has the bone")),
                );
                instance.skin =
                    Some(JointRange { start, len: u32::try_from(built_mesh.mesh.joint_bones.len()).unwrap() });
            }
            instances.push(instance);
        }
        Self { renderer, character, instances, joints }
    }

    /// Repaints the face layers whose picture changed, then renders.
    fn render(&mut self, live: &MockLive, changed: [bool; FaceLayerId::COUNT]) -> Image {
        for layer in FaceLayerId::ALL {
            let Some(face) = self.character.face_layer(layer) else { continue };
            if changed[layer as usize] {
                let mut painter = FaceLayerPainter { spec: face.spec, list: &live.drawing()[layer] };
                self.renderer.paint_texture(face.texture, &mut painter).unwrap();
            }
        }
        let mut scene = Scene::new(Camera::look_at(
            Vec3::new(0.0, 0.8, 0.95),
            Vec3::new(0.0, 0.78, 0.0),
            Vec3::Y,
            Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
        ));
        scene.background = Background::Solid { colour: Colour::hex(0x0018_1220) };
        scene.lights = vec![
            Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 0.9 },
            Light::Directional {
                to_light: Vec3::new(-0.5, 1.0, 2.0),
                colour: Colour::hex(0x00ff_f0e0),
                intensity: 2.0,
                casts_shadow: false,
            },
        ];
        scene.instances = self.instances.clone();
        scene.joints = self.joints.clone();
        self.renderer.render_image(SIZE, AlphaEncoding::Straight, &scene, &Quality::offline()).unwrap()
    }

    /// Releases everything and checks the renderer holds nothing afterwards.
    fn release(self) {
        let Self { mut renderer, character, .. } = self;
        character.release(&mut renderer).unwrap();
        assert_no_live_resources(&renderer);
    }
}

fn assert_no_live_resources(renderer: &dyn Renderer) {
    let left = renderer.stats();
    assert_eq!([left.meshes, left.textures, left.materials, left.targets], [0; 4], "the stage leaks nothing");
}

fn character() -> BuiltCharacter {
    let def = mock_character().unwrap();
    RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap()
}

fn live(built: BuiltCharacter) -> MockLive {
    let painter = Box::new(MockPainter::new(built.face.as_ref().unwrap().projection));
    MockLive::new(built, painter)
}

fn save(image: &Image, name: &str) {
    if let Some(dir) = std::env::var_os("OMUZAKI_STAGE_PNG") {
        image::save_buffer(
            std::path::Path::new(&dir).join(name),
            &image.rgba8,
            image.size.x,
            image.size.y,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}

fn pixels_where(image: &Image, test: impl Fn([u8; 4]) -> bool) -> usize {
    image.rgba8.chunks(4).filter(|p| test([p[0], p[1], p[2], p[3]])).count()
}

#[test]
fn the_painted_face_shows_on_the_head_and_follows_the_channels() {
    let built = character();
    assert_eq!(built.face_textures(FACE_PX_PER_M).len(), 2, "the face on the skin and the brows on the hair");
    let mut stage = Stage::new(&built);
    let mut live = live(built);

    // Neutral face.
    let neutral_buf = ChannelBuf::NEUTRAL;
    let first = live.tick(1.0 / 60.0, &RestPose, &neutral_buf).unwrap();
    let neutral = stage.render(&live, first.face_changed);
    save(&neutral, "mock-character-neutral.png");

    // The irises are the only cool-coloured pixels in the picture (the skin treatment and the warm key light pull
    // the mock's blue toward green, but skin and hair stay red-dominant).
    let blue = |p: [u8; 4]| p[1] > p[0].saturating_add(10) && p[3] == 255;
    let skin = |p: [u8; 4]| p[0] > 150 && p[0] > p[2].saturating_add(15) && p[1] > 100;
    assert!(pixels_where(&neutral, blue) > 100, "the painted irises reach the screen through the overlay");
    assert!(pixels_where(&neutral, skin) > 5000, "the head is there, lit like skin");

    // A smiling, talking face: only the face region should change, and the unchanged layers are not repainted.
    let mut talking = ChannelBuf::NEUTRAL;
    talking.set(Channel::JAW_OPEN, 0.7);
    talking.set(Channel::MOUTH_SMILE_LEFT, 0.8);
    talking.set(Channel::MOUTH_SMILE_RIGHT, 0.8);
    talking.set(Channel::EYE_LOOK_OUT_LEFT, 0.6);
    let second = live.tick(1.0 / 60.0, &RestPose, &talking).unwrap();
    assert_eq!(
        second.face_changed,
        [true, false, false],
        "the mouth and eyes move the base layer; the brows haven't moved"
    );
    let happy = stage.render(&live, second.face_changed);
    save(&happy, "mock-character-talking.png");

    let differing: Vec<usize> = neutral
        .rgba8
        .chunks(4)
        .zip(happy.rgba8.chunks(4))
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i)
        .collect();
    assert!(differing.len() > 200, "the face changed on screen: {} pixels", differing.len());
    let width = SIZE.x as usize;
    let (rows, cols): (Vec<usize>, Vec<usize>) = differing.iter().map(|i| (i / width, i % width)).unzip();
    let (top, bottom) = (*rows.iter().min().unwrap(), *rows.iter().max().unwrap());
    let (left, right) = (*cols.iter().min().unwrap(), *cols.iter().max().unwrap());
    // The eyes sit at about row 250 and the mouth at about 300; the head top (row 50) and the sides don't change.
    assert!(
        top > 200 && bottom < 330 && left > 60 && right < 300,
        "only the face region changes: rows {top}..{bottom}, columns {left}..{right}"
    );
    stage.release();
}

#[test]
fn the_hair_samples_the_brow_layer_and_the_head_the_face_layer() {
    let built = character();
    let mut stage = Stage::new(&built);
    let mut live = live(built);
    let buf = ChannelBuf::NEUTRAL;
    let report = live.tick(1.0 / 60.0, &RestPose, &buf).unwrap();
    let with_brows = stage.render(&live, report.face_changed);
    // Raise the brows: only the Base and Brows layers change, and the picture differs near the hair line.
    let mut raised = ChannelBuf::NEUTRAL;
    raised.set(Channel::BROW_INNER_UP, 1.0);
    let second = live.tick(1.0 / 60.0, &RestPose, &raised).unwrap();
    assert_eq!(second.face_changed, [true, true, false]);
    let image = stage.render(&live, second.face_changed);
    assert_ne!(with_brows, image);
    stage.release();
}

#[test]
fn upload_records_every_handle_by_name_and_by_part_and_index() {
    let built = character();
    let mut renderer = MockRenderer::new();
    let stage = StageCharacter::upload(&mut renderer, &built, UPLOAD).unwrap();

    assert!(built.meshes.iter().any(|m| m.mesh.dynamic), "the mock hair is dynamic, so the flag is exercised");
    assert_eq!(stage.meshes().len(), built.meshes.len());
    for (built_mesh, uploaded) in built.meshes.iter().zip(stage.meshes()) {
        assert_eq!(stage.mesh(&built_mesh.part, built_mesh.local), Some(uploaded), "found by (part, local index)");
        assert_eq!((&uploaded.part, uploaded.local), (&built_mesh.part, built_mesh.local), "in build order");
        assert_eq!(uploaded.dynamic, built_mesh.mesh.dynamic, "dynamic meshes are flagged");
        assert_eq!(Some(uploaded.material), stage.material(&built_mesh.mesh.material), "the mesh's own material");
    }
    assert!(built.materials.keys().all(|name| stage.material(name).is_some()), "materials by name");
    assert!(built.textures.keys().all(|name| stage.texture(name).is_some()), "textures by name");
    assert_eq!(stage.material(&MaterialName::new("no-such-material").unwrap()), None);
    assert_eq!(
        stage.face_layers().map(|l| l.spec.id).collect::<Vec<_>>(),
        [FaceLayerId::Base, FaceLayerId::Brows],
        "one texture per composited face layer"
    );

    let live = renderer.stats();
    assert_eq!(live.meshes as usize, built.meshes.len());
    assert_eq!(live.materials as usize, built.materials.len());
    assert_eq!(live.textures as usize, built.textures.len() + 2);
    stage.release(&mut renderer).unwrap();
    assert_no_live_resources(&renderer);
}

#[test]
fn a_failing_upload_releases_what_it_created() {
    let mut built = character();
    // A mesh the renderer refuses, last in build order: by then the textures, materials and every other mesh exist.
    let last = built.meshes.last_mut().unwrap();
    last.mesh.geometry.indices.extend([u32::MAX; 3]);
    let bad = last.mesh.name.clone();
    let mut renderer = Spy::new();

    let error = StageCharacter::upload(&mut renderer, &built, UPLOAD).unwrap_err();

    assert!(matches!(&error, StageError::Render { what, .. } if what.contains(&bad)), "names the mesh: {error}");
    assert_eq!(renderer.peak.meshes as usize, built.meshes.len() - 1, "every mesh before the bad one was created");
    assert!(renderer.peak.materials > 0 && renderer.peak.textures > 0, "and the materials and textures they use");
    assert_no_live_resources(&renderer);
}

#[test]
fn names_that_do_not_resolve_fail_cleanly() {
    let mut built = character();
    let last = built.meshes.last_mut().unwrap();
    last.mesh.material = MaterialName::new("missing-material").unwrap();
    let mut renderer = Spy::new();
    let error = StageCharacter::upload(&mut renderer, &built, UPLOAD).unwrap_err();
    assert!(matches!(&error, StageError::UnknownMaterial { material, .. } if material.as_str() == "missing-material"));
    assert!(renderer.peak.materials > 0, "the failure came after the materials were created");
    assert_no_live_resources(&renderer);

    let mut built = character();
    let material = built.materials.iter_mut().next().unwrap().1;
    material.base_texture = Some(TextureName::new("missing-texture").unwrap());
    let error = StageCharacter::upload(&mut renderer, &built, UPLOAD).unwrap_err();
    assert!(matches!(error, StageError::Material { .. }), "{error}");
    assert_no_live_resources(&renderer);
}

#[test]
fn an_unusable_face_density_creates_nothing() {
    let built = character();
    let mut renderer = MockRenderer::new();
    for face_px_per_m in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        let error = StageCharacter::upload(&mut renderer, &built, UploadOptions { face_px_per_m }).unwrap_err();
        assert!(matches!(error, StageError::FaceDensity { .. }), "{error}");
    }
    assert_eq!(renderer.stats(), ResourceStats::default());
}

#[cfg(debug_assertions)] // the leak guard is a debug assertion: loud in tests, a log line in release builds
#[test]
#[should_panic(expected = "dropped without release")]
fn dropping_a_character_without_releasing_it_fails_the_test() {
    let built = character();
    let mut renderer = MockRenderer::new();
    let _leaked = StageCharacter::upload(&mut renderer, &built, UPLOAD).unwrap();
}

/// Whether a pixel (by index into the 360 px picture) is where the face layers can reach: brows to chin. The head top
/// is at about row 50 and the eyes at about row 250.
fn in_face_region(index: usize) -> bool {
    let (row, col) = (index / SIZE.x as usize, index % SIZE.x as usize);
    (150..330).contains(&row) && (60..300).contains(&col)
}

#[test]
fn resizing_the_face_atlas_changes_only_texture_sharpness() {
    let built = character();
    let mut stage = Stage::new(&built);
    let mut live = live(built);
    let first = live.tick(1.0 / 60.0, &RestPose, &ChannelBuf::NEUTRAL).unwrap();
    let full = stage.render(&live, first.face_changed);
    let (full_stats, full_sizes) = (stage.renderer.stats(), layer_sizes(&stage));

    stage.character.resize_face(&mut stage.renderer, &live.built, FACE_PX_PER_M / 2.0).unwrap();
    let half_stats = stage.renderer.stats();
    assert_eq!(stage.character.face_px_per_m(), FACE_PX_PER_M / 2.0);
    assert_eq!(
        (half_stats.textures, half_stats.materials, half_stats.meshes),
        (full_stats.textures, full_stats.materials, full_stats.meshes),
        "the old textures are gone: nothing was added"
    );
    assert!(half_stats.gpu_bytes < full_stats.gpu_bytes, "and the new ones are smaller");
    for ((layer, (w, h)), (_, (full_w, full_h))) in layer_sizes(&stage).into_iter().zip(full_sizes.iter().copied()) {
        assert!(w.abs_diff(full_w / 2) <= 1 && h.abs_diff(full_h / 2) <= 1, "{layer:?} is half size: {w}x{h}");
    }

    // The new textures start empty; the painter fills them at the new density.
    let half = stage.render(&live, [true; FaceLayerId::COUNT]);
    let differing: Vec<usize> = full
        .rgba8
        .chunks(4)
        .zip(half.rgba8.chunks(4))
        .enumerate()
        .filter(|(_, (a, b))| a != b)
        .map(|(i, _)| i)
        .collect();
    save(&full, "mock-character-atlas-full.png");
    save(&half, "mock-character-atlas-half.png");
    assert!(!differing.is_empty(), "a smaller atlas is a softer picture");
    assert!(differing.iter().all(|&i| in_face_region(i)), "only the face region differs");
    // Measured: 1.8 % of the pixels differ (the edges of the irises, brows and mouth), mean step 0.2 of 255.
    assert!(differing.len() * 40 < full.rgba8.len() / 4, "only edges move: {} pixels differ", differing.len());
    let total: u64 = full.rgba8.iter().zip(&half.rgba8).map(|(a, b)| u64::from(a.abs_diff(*b))).sum();
    assert!(total < full.rgba8.len() as u64 / 2, "the picture is the same one: summed channel difference {total}");

    // Back to the original density: the very same picture.
    stage.character.resize_face(&mut stage.renderer, &live.built, FACE_PX_PER_M).unwrap();
    assert_eq!(stage.render(&live, [true; FaceLayerId::COUNT]), full);
    assert_eq!(stage.renderer.stats().textures, full_stats.textures);
    stage.release();
}

fn layer_sizes(stage: &Stage) -> Vec<(FaceLayerId, (u32, u32))> {
    stage.character.face_layers().map(|l| (l.spec.id, l.spec.size_px())).collect()
}

#[test]
fn a_refused_resize_leaves_the_old_atlas_in_place() {
    let built = character();
    let mut stage = Stage::new(&built);
    let mut live = live(built);
    let first = live.tick(1.0 / 60.0, &RestPose, &ChannelBuf::NEUTRAL).unwrap();
    let before = stage.render(&live, first.face_changed);
    let (stats, textures) =
        (stage.renderer.stats(), stage.character.face_layers().map(|l| l.texture).collect::<Vec<_>>());

    // The head and the hair both composite a face layer, so a resize updates two materials; refuse the second.
    let updates = stage.renderer.updates;
    stage.renderer.refuse_update = Some(updates + 1);
    let error = stage.character.resize_face(&mut stage.renderer, &live.built, FACE_PX_PER_M / 2.0).unwrap_err();

    assert!(matches!(error, StageError::Render { .. }), "{error}");
    assert_eq!(stage.renderer.updates - updates, 3, "two updates, the second refused, then the first one undone");
    assert_eq!(stage.character.face_px_per_m(), FACE_PX_PER_M);
    assert_eq!(stage.character.face_layers().map(|l| l.texture).collect::<Vec<_>>(), textures);
    assert_eq!(stage.renderer.stats().textures, stats.textures, "the new textures were released again");
    // Nothing is repainted: the materials point at the old textures, which kept their pixels.
    assert_eq!(stage.render(&live, [false; FaceLayerId::COUNT]), before);
    stage.release();
}
