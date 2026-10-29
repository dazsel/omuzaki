//! A scene of the document staged whole (#2401, #2415): every visible member built and drawn on one cast with its
//! touch, the scene resolved around them, and the document followed: a scene edit re-resolves and builds nothing, a
//! member whose character changed is built again alone, a member added, hidden or removed is staged or given back, a
//! new subject stages the cast again, and releasing leaves the renderer as it was.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use omuzaki_char_api::{
    BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef, RegistryBuilder, VariantId,
};
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocRead, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Op, OpCtx, OpError};
use omuzaki_render_api::{
    AlphaEncoding, Capabilities, EnvironmentDesc, EnvironmentId, FrameReport, Image, Material, MaterialId, MeshData,
    MeshId, MeshOptions, Painter, Quality, RenderError, Renderer, ResourceRef, ResourceStats, Scene, TargetDesc,
    TargetId, TextureDesc, TextureId,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::body::Body;
use omuzaki_stage::frame::{FrameInput, StageClock};
use omuzaki_stage::ops::scene::cast::{PlaceCharacter, SetInteract, SetMember, SetSubject};
use omuzaki_stage::ops::scene::create::CreateScene;
use omuzaki_stage::ops::scene::prop::AddProp;
use omuzaki_stage::scene::stage::{BodySource, SceneStage, SceneStageError, SceneStageOptions};
use omuzaki_stage::stages::FrameLoop;
use omuzaki_stage_api::{MemberId, SceneId};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

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

/// Builds kit characters from the document's definitions with the registry's builder, and counts each.
#[derive(Default)]
struct Loads {
    loads: Vec<String>,
}

impl BodySource for Loads {
    fn load(&mut self, doc: &dyn DocRead, id: &CharId, _variant: Option<&VariantId>) -> Result<Body, OpError> {
        self.loads.push(id.to_string());
        let node = doc.get(&omuzaki_char_api::paths::character(id)).ok_or_else(|| OpError::not_found("no such"))?;
        let def = CharacterDef::from_node(node).map_err(|e| OpError::invalid("character", e.to_string()))?;
        let built = RegistryBuilder
            .build(id, &def, &BuildOptions::default(), &BuildServices::default())
            .map_err(|e| OpError::invalid("character", e.to_string()))?;
        Ok(Body::Kit(Box::new(built)))
    }
}

fn character_node(name: &str) -> Value {
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    let mut node = json!({});
    def.to_node(&mut node).expect("the fixture writes");
    node["name"] = json!(name);
    node
}

#[allow(clippy::needless_pass_by_value, reason = "every call site passes a json! literal")]
fn edit<T: Op + DeserializeOwned>(doc: &mut MemDoc, args: Value) -> Value {
    omuzaki_stage::link();
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let op: T = serde_json::from_value(args.clone()).expect("the arguments read");
    let plan = op.plan(&OpCtx::new(doc, &actor, &clock)).unwrap_or_else(|e| panic!("{} {args}: {e:?}", T::meta().name));
    doc.apply(&plan.patch).expect("applies");
    plan.output.unwrap_or(Value::Null)
}

fn member(name: &str) -> MemberId {
    MemberId::new(name).unwrap()
}

/// Characters `kiri` and `aiko`, and scene `main` with kiri as its subject and aiko hugging her, a desk behind them.
fn duo() -> MemDoc {
    let mut doc = MemDoc::from_value(json!({
        "characters": { "kiri": character_node("Kiri"), "aiko": character_node("Aiko") },
        "project": { "default_character": "kiri" },
    }));
    edit::<CreateScene>(&mut doc, json!({ "id": "main" }));
    edit::<PlaceCharacter>(&mut doc, json!({ "character": "aiko", "id": "aiko" }));
    edit::<SetInteract>(&mut doc, json!({ "member": "main", "with": "aiko", "kind": "hug" }));
    edit::<AddProp>(&mut doc, json!({ "kind": "desk", "position": [0.0, 0.0, -1.2] }));
    doc
}

fn options() -> SceneStageOptions {
    SceneStageOptions::new(Quality::offline(), StageClock::fixed(60.0)).with_face(None)
}

fn target(renderer: &mut MockRenderer) -> TargetId {
    renderer.create_target(&TargetDesc { size: UVec2::new(64, 36), alpha: AlphaEncoding::Straight }).unwrap()
}

fn open(doc: &MemDoc, renderer: &mut MockRenderer, bodies: &mut Loads) -> SceneStage {
    let target = target(renderer);
    SceneStage::open(doc, None, bodies, renderer, target, 16.0 / 9.0, options()).expect("the scene stages")
}

fn ids(stage: &SceneStage) -> Vec<&str> {
    stage.members().map(MemberId::as_str).collect()
}

fn frame() -> FrameInput {
    FrameInput { dt: 1.0 / 60.0, ..FrameInput::default() }
}

/// Releases `stage` and its target: a stage is given back, never dropped.
fn done(stage: SceneStage, renderer: &mut dyn Renderer) {
    let target = stage.target();
    stage.release(renderer).unwrap();
    renderer.release(target.into()).unwrap();
}

/// What the renderer holds, without the memory estimate (released resources may stay in it a while).
fn held(renderer: &MockRenderer) -> ResourceStats {
    ResourceStats { gpu_bytes: 0, ..renderer.stats() }
}

#[test]
fn a_scene_of_two_stages_both_members_with_their_touch_and_its_props() {
    let doc = duo();
    let mut renderer = MockRenderer::new();
    let mut bodies = Loads::default();
    let mut stage = open(&doc, &mut renderer, &mut bodies);
    assert_eq!(stage.scene_id().map(SceneId::as_str), Some("main"));
    assert_eq!(ids(&stage), ["aiko", "main"]);
    assert_eq!(stage.subject().as_str(), "main", "the scene's subject leads the cast");
    assert_eq!(bodies.loads.len(), 2, "each member built once: {:?}", bodies.loads);
    assert_eq!(stage.resolved().props.builds.props.len(), 1, "the desk is resolved");

    stage.tick(&mut renderer, &frame()).unwrap();
    let one = stage.cast().lead().scene().instances.len();
    for _ in 0..30 {
        stage.tick(&mut renderer, &frame()).unwrap();
    }
    assert_eq!(stage.scene().instances.len(), one, "a frame draws the same bodies and props as the last");
    let report = stage.cast().touch_report(&member("main")).expect("the hug is solved every frame");
    assert!(report.heads.is_finite(), "the two are measured against each other: {report:?}");
    done(stage, &mut renderer);
}

#[test]
fn a_scene_edit_resolves_again_and_builds_no_body() {
    let mut doc = duo();
    let mut renderer = MockRenderer::new();
    let mut bodies = Loads::default();
    let mut stage = open(&doc, &mut renderer, &mut bodies);
    let synced = stage.sync(&doc, &mut bodies, &mut renderer).unwrap();
    assert!(!synced.changed(), "nothing changed: {synced:?}");

    edit::<AddProp>(&mut doc, json!({ "kind": "lamp", "position": [1.0, 0.0, -1.0] }));
    edit::<SetMember>(&mut doc, json!({ "member": "aiko", "yaw": 30.0 }));
    let synced = stage.sync(&doc, &mut bodies, &mut renderer).unwrap();
    assert!(synced.resolved && !synced.restaged && synced.members.is_empty(), "{synced:?}");
    assert_eq!(bodies.loads.len(), 2, "no body is built for a scene edit");
    assert_eq!(stage.resolved().props.builds.props.len(), 2);
    assert!(stage.owed().is_empty(), "the lamp is uploaded");
    done(stage, &mut renderer);
}

#[test]
fn a_member_whose_character_changes_is_built_again_alone() {
    let mut doc = duo();
    let mut renderer = MockRenderer::new();
    let mut bodies = Loads::default();
    let mut stage = open(&doc, &mut renderer, &mut bodies);
    let before = held(&renderer);
    let name = omuzaki_char_api::paths::character(&CharId::new("aiko").unwrap()).join("name");
    doc.apply(&Patch::new().with(name, Some(json!("Aiko")), Some(json!("Aiko II")))).unwrap();
    let synced = stage.sync(&doc, &mut bodies, &mut renderer).unwrap();
    assert_eq!(synced.members, [member("aiko")], "{synced:?}");
    assert!(!synced.restaged);
    assert_eq!(bodies.loads.last().map(String::as_str), Some("aiko"));
    assert_eq!(bodies.loads.len(), 3);
    assert_eq!(held(&renderer), before, "the old body is given back as the new one is staged");
    stage.tick(&mut renderer, &frame()).unwrap();
    done(stage, &mut renderer);
}

#[test]
fn members_added_hidden_and_removed_are_staged_and_given_back() {
    let mut doc = duo();
    let mut renderer = MockRenderer::new();
    let mut bodies = Loads::default();
    let mut stage = open(&doc, &mut renderer, &mut bodies);
    let two = held(&renderer);

    edit::<PlaceCharacter>(&mut doc, json!({ "character": "kiri", "id": "echo" }));
    let synced = stage.sync(&doc, &mut bodies, &mut renderer).unwrap();
    assert_eq!(synced.members, [member("echo")]);
    assert_eq!(ids(&stage), ["aiko", "echo", "main"]);
    assert!(held(&renderer).meshes > two.meshes);
    stage.tick(&mut renderer, &frame()).unwrap();

    edit::<SetMember>(&mut doc, json!({ "member": "echo", "visible": false }));
    let synced = stage.sync(&doc, &mut bodies, &mut renderer).unwrap();
    assert_eq!(synced.members, [member("echo")]);
    assert_eq!(ids(&stage), ["aiko", "main"], "a hidden member is off the stage");
    assert_eq!(held(&renderer), two, "and holds nothing on the renderer");

    edit::<SetMember>(&mut doc, json!({ "member": "echo", "visible": true }));
    stage.sync(&doc, &mut bodies, &mut renderer).unwrap();
    assert_eq!(ids(&stage), ["aiko", "echo", "main"]);
    stage.tick(&mut renderer, &frame()).unwrap();
    done(stage, &mut renderer);
}

#[test]
fn a_new_subject_stages_the_cast_again_on_the_same_target() {
    let mut doc = duo();
    let mut renderer = MockRenderer::new();
    let mut bodies = Loads::default();
    let mut stage = open(&doc, &mut renderer, &mut bodies);
    let target = stage.target();
    let before = held(&renderer);
    edit::<SetSubject>(&mut doc, json!({ "member": "aiko" }));
    let synced = stage.sync(&doc, &mut bodies, &mut renderer).unwrap();
    assert!(synced.restaged, "{synced:?}");
    assert_eq!(stage.subject().as_str(), "aiko");
    assert_eq!(stage.target(), target, "the picture stays where the caller looks");
    assert_eq!(held(&renderer), before, "the old cast is given back");
    stage.tick(&mut renderer, &frame()).unwrap();
    done(stage, &mut renderer);
}

#[test]
fn releasing_gives_back_everything_but_the_callers_target() {
    let doc = duo();
    let mut renderer = MockRenderer::new();
    let empty = held(&renderer);
    let mut bodies = Loads::default();
    let mut stage = open(&doc, &mut renderer, &mut bodies);
    stage.tick(&mut renderer, &frame()).unwrap();
    let target = stage.target();
    stage.release(&mut renderer).unwrap();
    renderer.release(target.into()).unwrap();
    assert_eq!(held(&renderer), empty);
}

#[test]
fn a_project_with_no_scene_stages_its_character_in_the_default_and_one_with_nobody_is_refused() {
    let doc = MemDoc::from_value(json!({ "characters": { "kiri": character_node("Kiri") } }));
    let mut renderer = MockRenderer::new();
    let mut bodies = Loads::default();
    let stage = open(&doc, &mut renderer, &mut bodies);
    assert_eq!(stage.scene_id(), None, "the built-in default, unsaved");
    assert_eq!(ids(&stage), ["main"]);
    assert!(stage.resolved().active.is_some(), "framed by the default's camera");

    let empty = MemDoc::from_value(json!({}));
    let target = target(&mut renderer);
    let refused = SceneStage::open(&empty, None, &mut bodies, &mut renderer, target, 1.0, options());
    assert!(matches!(refused, Err(SceneStageError::NoCast)), "{refused:?}");
    done(stage, &mut renderer);
}

#[test]
fn a_touch_turned_off_frees_both() {
    let mut doc = duo();
    let mut renderer = MockRenderer::new();
    let mut bodies = Loads::default();
    let mut stage = open(&doc, &mut renderer, &mut bodies);
    edit::<SetInteract>(&mut doc, json!({ "member": "main", "kind": "off" }));
    let synced = stage.sync(&doc, &mut bodies, &mut renderer).unwrap();
    assert!(synced.resolved);
    stage.tick(&mut renderer, &frame()).unwrap();
    assert!(stage.cast().touch_report(&member("main")).is_none(), "no touch is solved");
    done(stage, &mut renderer);
}

#[test]
fn a_member_whose_character_is_gone_is_left_off_and_named_and_the_rest_is_drawn() {
    let mut doc = duo();
    let mut renderer = MockRenderer::new();
    let mut bodies = Loads::default();
    let mut stage = open(&doc, &mut renderer, &mut bodies);
    let aiko = omuzaki_char_api::paths::character(&CharId::new("aiko").unwrap());
    let node = doc.get(&aiko).cloned();
    doc.apply(&Patch::new().with(aiko, node, None)).unwrap();
    let synced = stage.sync(&doc, &mut bodies, &mut renderer).unwrap();
    assert_eq!(synced.members, [member("aiko")], "taken off: {synced:?}");
    assert_eq!(ids(&stage), ["main"]);
    assert_eq!(stage.skipped().iter().map(|(m, _)| m.as_str()).collect::<Vec<_>>(), ["aiko"], "and named");
    stage.tick(&mut renderer, &frame()).unwrap();
    done(stage, &mut renderer);
}

/// The instances' transforms of `frames` frames of the duo, from a fresh stage each run.
fn run_duo(frames: usize) -> Vec<Vec<[f32; 16]>> {
    let doc = duo();
    let mut renderer = MockRenderer::new();
    let mut stage = open(&doc, &mut renderer, &mut Loads::default());
    let mut out = Vec::with_capacity(frames);
    for _ in 0..frames {
        stage.tick(&mut renderer, &frame()).unwrap();
        out.push(stage.scene().instances.iter().map(|i| i.transform.to_cols_array()).collect());
    }
    done(stage, &mut renderer);
    out
}

/// The mock renderer, which once `canned` holds a report answers `render` with it without drawing and doesn't copy
/// vertex updates: a frame then measures the stage's own work (as `scene_props` and the frame loop's `Recorder` do).
struct Canned {
    inner: MockRenderer,
    canned: Option<FrameReport>,
}

impl Renderer for Canned {
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

#[test]
fn two_members_draw_without_allocating_and_two_runs_are_the_same_frame_for_frame() {
    assert_eq!(run_duo(30), run_duo(30), "the same document draws the same frames");

    let doc = duo();
    let mut renderer = Canned { inner: MockRenderer::new(), canned: None };
    let target = target(&mut renderer.inner);
    let mut stage = SceneStage::open(&doc, None, &mut Loads::default(), &mut renderer, target, 16.0 / 9.0, options())
        .expect("the scene stages");
    for _ in 0..5 {
        stage.tick(&mut renderer, &frame()).unwrap();
    }
    // The mock's draw copies the scene: answer it with a real report, so what is counted is the stage's own frame.
    renderer.canned = Some(renderer.inner.render(target, stage.scene(), &Quality::offline()).unwrap());
    let ((), allocations) = counted(|| {
        for _ in 0..60 {
            stage.tick(&mut renderer, &frame()).unwrap();
        }
    });
    assert_eq!(allocations, 0, "60 frames of a cast of two, touching, allocate nothing");
    done(stage, &mut renderer);
}
