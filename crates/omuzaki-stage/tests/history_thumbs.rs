//! History thumbnails (#233, VC-01): an executor runs edits to the mock character while [`Thumbnails`] follows its
//! history. A burst of commits renders a handful of pictures at most and every transaction resolves to one; the same
//! document gives the same picture; a renderer that records its callers sees only the worker's thread; and the index
//! is back after the project is reopened.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[path = "render/support.rs"]
mod support;

use std::any::Any;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_char_api::{BuildServices, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Actor, Grants, Mode, Request, Runner, Services, ServicesBuilder, TxId};
use omuzaki_render_api::{
    Capabilities, EnvironmentDesc, EnvironmentId, FrameReport, Image, Material, MaterialId, MeshData, MeshId,
    MeshOptions, Painter, Quality, RenderError, Renderer, ResourceRef, ResourceStats, Scene, TargetDesc, TargetId,
    TextureDesc, TextureId,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::history::index::INDEX_FILE;
use omuzaki_stage::history::thumbs::{SIZE, THREAD, Thumbnails};
use omuzaki_store_api::Store;
use omuzaki_store_mock::MemStore;
use serde_json::json;

/// Long enough for a debug build on a slow runner; the tests wait on the index, not on the clock.
const WAIT: Duration = Duration::from_secs(60);

/// The mock renderer, noting the thread of every frame it renders.
struct Recording {
    inner: MockRenderer,
    threads: Arc<Mutex<Vec<String>>>,
}

impl Recording {
    fn note(&self) {
        let name = std::thread::current().name().unwrap_or("unnamed").to_owned();
        self.threads.lock().unwrap().push(name);
    }
}

impl Renderer for Recording {
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    fn stats(&self) -> ResourceStats {
        self.inner.stats()
    }
    fn create_mesh(&mut self, mesh: &MeshData) -> Result<MeshId, RenderError> {
        self.note();
        self.inner.create_mesh(mesh)
    }
    fn create_mesh_with(&mut self, mesh: &MeshData, options: &MeshOptions) -> Result<MeshId, RenderError> {
        self.note();
        self.inner.create_mesh_with(mesh, options)
    }
    fn update_vertices(&mut self, mesh: MeshId, positions: &[Vec3], normals: &[Vec3]) -> Result<(), RenderError> {
        self.inner.update_vertices(mesh, positions, normals)
    }
    fn create_texture(&mut self, desc: &TextureDesc, rgba: Option<&[u8]>) -> Result<TextureId, RenderError> {
        self.note();
        self.inner.create_texture(desc, rgba)
    }
    fn paint_texture(&mut self, texture: TextureId, painter: &mut dyn Painter) -> Result<(), RenderError> {
        self.note();
        self.inner.paint_texture(texture, painter)
    }
    fn create_material(&mut self, material: &Material) -> Result<MaterialId, RenderError> {
        self.inner.create_material(material)
    }
    fn update_material(&mut self, id: MaterialId, material: &Material) -> Result<(), RenderError> {
        self.inner.update_material(id, material)
    }
    fn create_environment(&mut self, desc: &EnvironmentDesc) -> Result<EnvironmentId, RenderError> {
        self.inner.create_environment(desc)
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
        self.note();
        self.inner.render(target, scene, quality)
    }
    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        self.inner.read_target(target)
    }
    fn target_native(&self, target: TargetId) -> Result<Option<&dyn Any>, RenderError> {
        self.inner.target_native(target)
    }
}

/// An executor over the mock character, a thumbnailer following it in `project`, and handles on what they share.
struct Rig {
    exec: Arc<Executor>,
    thumbs: Thumbnails,
    store: Arc<MemStore>,
    threads: Arc<Mutex<Vec<String>>>,
}

fn services(store: &Arc<MemStore>, threads: &Arc<Mutex<Vec<String>>>) -> Services {
    let renderer: Box<dyn Renderer> = Box::new(Recording { inner: MockRenderer::new(), threads: threads.clone() });
    ServicesBuilder::new()
        .insert::<Mutex<Box<dyn Renderer>>>(Arc::new(Mutex::new(renderer)))
        .insert::<dyn CharacterBuilder>(Arc::new(RegistryBuilder))
        .insert::<BuildServices>(Arc::new(BuildServices::default()))
        .insert::<dyn Store>(store.clone())
        .build()
}

fn rig(project: &Path) -> Rig {
    let (store, threads) = (Arc::new(MemStore::new()), Arc::new(Mutex::new(Vec::new())));
    let services = services(&store, &threads);
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Arc::new(Executor::new(Box::new(support::mock_doc().0), clock).unwrap().with_services(services.clone()));
    let thumbs = Thumbnails::start(exec.clone(), services, project).unwrap();
    Rig { exec, thumbs, store, threads }
}

impl Rig {
    /// Sets how loosely the mock's vest hangs, as a person in the inspector would; returns the transaction.
    fn ease(&self, ease: f64) -> TxId {
        let request = Request {
            op: "part.set_param".to_owned(),
            args: json!({ "character": "mock", "part": "vest", "path": "ease", "value": ease }),
            mode: Mode::Apply,
            actor: Actor::Person { name: "test".to_owned() },
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        self.exec.run(request).expect("the edit applies").applied.expect("applied").tx
    }

    fn rendered_on(&self) -> Vec<String> {
        self.threads.lock().unwrap().clone()
    }
}

#[test]
fn a_burst_of_a_hundred_commits_renders_a_few_pictures_and_every_commit_resolves() {
    let project = tempfile::tempdir().unwrap();
    let rig = rig(project.path());
    let txs: Vec<TxId> = (0..100).map(|i| rig.ease(if i % 2 == 0 { 0.02 } else { 0.03 })).collect();
    let last = rig.thumbs.wait_for(*txs.last().unwrap(), WAIT).expect("the burst's last commit has a thumbnail");
    let renders = rig.thumbs.renders();
    println!("{renders} renders for a burst of 100 commits");
    assert!((1..=5).contains(&renders), "{renders} renders for a burst of 100 commits");
    let hashes: Vec<String> = txs.iter().map(|tx| rig.thumbs.get(*tx).expect("every commit resolves")).collect();
    assert_eq!(hashes.last(), Some(&last));
    // The picture is a 96x96 thumbnail at 2x, in the store.
    let png = rig.store.get(&last.parse().unwrap()).expect("stored");
    let image = image::load_from_memory(&png).unwrap();
    assert_eq!([image.width(), image.height()], SIZE);
    // Every frame and upload ran on the worker, none on this thread (which stands for the UI and the frame thread).
    let threads = rig.rendered_on();
    assert!(!threads.is_empty(), "the recording renderer saw the thumbnails");
    assert!(threads.iter().all(|t| t == THREAD), "only the worker renders: {threads:?}");
}

#[test]
fn the_same_document_gives_the_same_picture() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (first, second) = (rig(one.path()), rig(two.path()));
    let picture = |rig: &Rig| {
        let tx = rig.ease(0.04);
        rig.thumbs.wait_for(tx, WAIT).expect("a thumbnail")
    };
    let a = picture(&first);
    assert_eq!(a, picture(&second), "two projects with the same document give the same bytes");
    // A different document gives a different picture.
    let tx = first.ease(0.001);
    assert_ne!(first.thumbs.wait_for(tx, WAIT).expect("a thumbnail"), a);
}

#[test]
fn the_index_survives_reopening_the_project() {
    let project = tempfile::tempdir().unwrap();
    let rig_before = rig(project.path());
    let txs = [rig_before.ease(0.01), rig_before.ease(0.02)];
    let third = rig_before.ease(0.03);
    let landed = rig_before.thumbs.wait_for(third, WAIT).expect("a thumbnail");
    let before: Vec<String> = txs.iter().map(|tx| rig_before.thumbs.get(*tx).unwrap()).collect();
    let Rig { thumbs, exec, .. } = rig_before;
    thumbs.stop();
    drop(exec);
    #[allow(clippy::disallowed_methods, reason = "the test looks at the index file it expects")]
    let text = std::fs::read_to_string(project.path().join(INDEX_FILE)).expect("the index is on disk");
    assert!(text.contains(&landed), "{text}");

    // Reopened, before any edit: the earlier transactions still resolve, and nothing rendered to find them.
    let reopened = rig(project.path());
    let after: Vec<String> = txs.iter().map(|tx| reopened.thumbs.get(*tx).expect("kept")).collect();
    assert_eq!(after, before);
    assert_eq!(reopened.thumbs.get(third), Some(landed));
    assert_eq!(reopened.thumbs.renders(), 0);
}
