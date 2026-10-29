//! The render services a project command gives the render ops (`render.preview` and the other `render.*` jobs), so a
//! headless agent or a script sees what it made: a headless wgpu renderer, the kit's character builder and its build
//! services. The asset store the PNGs go to is the app core's: the person's, in their data folder, the one the studio
//! uses (`omuzaki store export` writes a render from it to a file).
//!
//! The renderer opens its GPU device on its first call, so `omuzaki ops`, `help` and every op that doesn't render never
//! start one. Without a GPU adapter a render fails, and `omuzaki run` reports it as `unavailable`, saying why
//! ([`explain`]).
//!
//! # Capabilities before the device opens
//! The ops read [`Capabilities`] as a service, fixed when the executor starts, before any device is open, and they read
//! one field of it: `mtoon`, which the wgpu renderer sets the same on every device. So the service is the [`promised`]
//! capabilities: the device-independent fields as `WgpuRenderer` sets them, and the device-dependent ones (the backend's
//! name, the texture side, the samples, the passes, the timings) at the floor every device meets. Opening a device to
//! learn them would start a GPU for every command. The renderer itself answers `capabilities()` with the device's own
//! once it is open, and a test opens the real renderer and pins the promise to it.

use std::sync::{Arc, Mutex, OnceLock};

use omuzaki_char_api::{BuildServices, CharacterBuilder, RegistryBuilder};
use omuzaki_core::glam::Vec3;
use omuzaki_ops_api::{ErrorCode, OpError, ServicesBuilder};
use omuzaki_render::{HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{
    Capabilities, EnvironmentDesc, EnvironmentId, FrameReport, Image, Material, MaterialId, MeshData, MeshId,
    MeshOptions, MtoonSupport, Painter, ParticleSystemDesc, ParticleSystemId, Quality, RenderError, Renderer,
    ResourceRef, ResourceStats, Scene, TargetDesc, TargetId, TextureDesc, TextureId, limits,
};

/// The most morph targets a mesh may have on the wgpu renderer (`omuzaki-render`'s `morph::MAX_TARGETS`), which it
/// does not export; the test `the_promise_matches_the_real_renderer` pins it.
const WGPU_MAX_MORPH_TARGETS: u32 = 256;

/// Why this process's renderer could not open, once it tried and failed: [`explain`] reads it to say so. One `omuzaki`
/// process runs one command, so one renderer.
static NO_GPU: OnceLock<String> = OnceLock::new();

/// `builder` with the render services added: the lazily opened renderer (as `Mutex<Box<dyn Renderer>>`, the form the
/// render jobs borrow it in), its [`promised`] capabilities, the kit's builder and default build services.
///
/// With `warm`, the device opens now, before any job starts: a render job's time cap counts its rendering, and opening
/// a software GPU and compiling its pipelines can take most of a minute on a busy machine. A device that can't open is
/// remembered and reported by the render that needs it, as without `warm`.
pub fn with_render_services(builder: ServicesBuilder, warm: bool) -> ServicesBuilder {
    let mut lazy = LazyRenderer::new(HeadlessOptions::default());
    if warm {
        // A failure is recorded in `NO_GPU`; the job that renders reports it.
        let _ = lazy.open();
    }
    let renderer: Box<dyn Renderer> = Box::new(lazy);
    builder
        .insert::<Mutex<Box<dyn Renderer>>>(Arc::new(Mutex::new(renderer)))
        .insert::<Capabilities>(Arc::new(promised()))
        .insert::<dyn CharacterBuilder>(Arc::new(RegistryBuilder))
        .insert::<BuildServices>(Arc::new(BuildServices::default()))
}

/// Whether `op` is a render job, so a command that runs it opens the GPU before the job starts ([`with_render_services`]).
#[must_use]
pub fn renders(op: &str) -> bool {
    omuzaki_ops_api::registrations().any(|reg| {
        let meta = (reg.meta)();
        meta.name == op && meta.kind == omuzaki_ops_api::OpKind::Job && meta.scope == omuzaki_ops_api::Scope::Render
    })
}

/// What the headless wgpu renderer can do, as far as it is known before its device opens (see the module docs).
#[must_use]
pub fn promised() -> Capabilities {
    Capabilities {
        backend: "wgpu, headless (the device opens on the first render)".to_owned(),
        max_texture_size: limits::GUARANTEED_TEXTURE_SIZE,
        max_samples: 1,
        passes: Vec::new(),
        shading_programs: Vec::new(),
        gpu_timings: false,
        bit_exact: false,
        dynamic_meshes: true,
        morph_targets: true,
        max_morph_targets: WGPU_MAX_MORPH_TARGETS,
        mtoon: MtoonSupport::ALL,
    }
}

/// `error` as the person should read it: an `internal` failure of a command whose renderer could not open is the
/// missing GPU, so it becomes `unavailable`, with the reason and how to get one. Anything else passes through.
#[must_use]
pub fn explain(error: OpError) -> OpError {
    match (error.code, NO_GPU.get()) {
        (ErrorCode::Internal, Some(reason)) => no_gpu(reason),
        _ => error,
    }
}

/// The `unavailable` error for a renderer that could not open because of `reason`.
fn no_gpu(reason: &str) -> OpError {
    let mut error = OpError::unavailable("renderer");
    error.message = format!("there is no GPU to render with: {reason}");
    error.hint = Some(
        "render ops need a GPU adapter: a graphics driver with Vulkan, Metal or DirectX 12, or a software one (Mesa's \
         lavapipe on Linux, WARP on Windows); WGPU_ADAPTER_NAME picks an adapter by name"
            .to_owned(),
    );
    error
}

/// A headless [`WgpuRenderer`] that opens its device on the first call that needs one. Calls that only read
/// (`capabilities`, `stats`, `target_native`) answer for an empty renderer until then.
pub struct LazyRenderer {
    options: HeadlessOptions,
    /// `None` until the first call that needs the device; then the renderer, or why it could not open (kept, so a
    /// later call fails the same way instead of trying again).
    state: Option<Result<WgpuRenderer, String>>,
    promised: Capabilities,
}

impl std::fmt::Debug for LazyRenderer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match &self.state {
            None => "not opened",
            Some(Ok(_)) => "open",
            Some(Err(_)) => "failed to open",
        };
        f.debug_struct("LazyRenderer").field("options", &self.options).field("state", &state).finish_non_exhaustive()
    }
}

impl LazyRenderer {
    /// A renderer that opens on an adapter chosen by `options` when first used.
    #[must_use]
    pub fn new(options: HeadlessOptions) -> Self {
        Self { options, state: None, promised: promised() }
    }

    /// Whether the device has been opened (or tried).
    #[must_use]
    pub fn opened(&self) -> bool {
        self.state.is_some()
    }

    /// The open renderer, opening it now if this is the first call.
    ///
    /// # Errors
    /// `Backend`, with the reason, if no adapter or device is available.
    fn open(&mut self) -> Result<&mut WgpuRenderer, RenderError> {
        let options = self.options;
        let state = self.state.get_or_insert_with(|| {
            WgpuRenderer::headless(&options).map_err(|e| {
                let reason = e.to_string();
                // The first failure is the one reported; a second renderer in the process fails the same way.
                let _ = NO_GPU.set(reason.clone());
                reason
            })
        });
        state.as_mut().map_err(|reason| RenderError::Backend(format!("there is no GPU to render with: {reason}")))
    }

    /// The open renderer, if it is open.
    fn opened_renderer(&self) -> Option<&WgpuRenderer> {
        self.state.as_ref().and_then(|state| state.as_ref().ok())
    }
}

impl Renderer for LazyRenderer {
    fn capabilities(&self) -> &Capabilities {
        self.opened_renderer().map_or(&self.promised, Renderer::capabilities)
    }

    fn stats(&self) -> ResourceStats {
        self.opened_renderer().map(Renderer::stats).unwrap_or_default()
    }

    fn create_mesh(&mut self, mesh: &MeshData) -> Result<MeshId, RenderError> {
        self.open()?.create_mesh(mesh)
    }

    fn create_mesh_with(&mut self, mesh: &MeshData, options: &MeshOptions) -> Result<MeshId, RenderError> {
        self.open()?.create_mesh_with(mesh, options)
    }

    fn update_vertices(&mut self, mesh: MeshId, positions: &[Vec3], normals: &[Vec3]) -> Result<(), RenderError> {
        self.open()?.update_vertices(mesh, positions, normals)
    }

    fn update_colours(&mut self, mesh: MeshId, colours: &[[f32; 4]]) -> Result<(), RenderError> {
        self.open()?.update_colours(mesh, colours)
    }

    fn create_texture(&mut self, desc: &TextureDesc, rgba: Option<&[u8]>) -> Result<TextureId, RenderError> {
        self.open()?.create_texture(desc, rgba)
    }

    fn paint_texture(&mut self, texture: TextureId, painter: &mut dyn Painter) -> Result<(), RenderError> {
        self.open()?.paint_texture(texture, painter)
    }

    fn create_material(&mut self, material: &Material) -> Result<MaterialId, RenderError> {
        self.open()?.create_material(material)
    }

    fn update_material(&mut self, id: MaterialId, material: &Material) -> Result<(), RenderError> {
        self.open()?.update_material(id, material)
    }

    fn create_environment(&mut self, desc: &EnvironmentDesc) -> Result<EnvironmentId, RenderError> {
        self.open()?.create_environment(desc)
    }

    fn create_particle_system(&mut self, desc: &ParticleSystemDesc) -> Result<ParticleSystemId, RenderError> {
        self.open()?.create_particle_system(desc)
    }

    fn update_particle_system(&mut self, id: ParticleSystemId, desc: &ParticleSystemDesc) -> Result<(), RenderError> {
        self.open()?.update_particle_system(id, desc)
    }

    fn create_target(&mut self, desc: &TargetDesc) -> Result<TargetId, RenderError> {
        self.open()?.create_target(desc)
    }

    fn resize_target(&mut self, target: TargetId, size: omuzaki_core::glam::UVec2) -> Result<(), RenderError> {
        self.open()?.resize_target(target, size)
    }

    fn release(&mut self, resource: ResourceRef) -> Result<(), RenderError> {
        self.open()?.release(resource)
    }

    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError> {
        self.open()?.render(target, scene, quality)
    }

    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        self.open()?.read_target(target)
    }

    fn target_native(&self, target: TargetId) -> Result<Option<&dyn std::any::Any>, RenderError> {
        // A renderer that never opened has made no target.
        self.opened_renderer().ok_or(RenderError::UnknownHandle { what: "target" })?.target_native(target)
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_render::AdapterPreference;

    use super::*;

    /// The services carry the renderer without opening it: a command that doesn't render never starts a GPU.
    #[test]
    fn the_services_hold_an_unopened_renderer_and_the_promise() {
        let services = with_render_services(ServicesBuilder::new(), false).build();
        let renderer = services.get::<Mutex<Box<dyn Renderer>>>().expect("the renderer is wired");
        let renderer = renderer.lock().expect("not poisoned");
        assert_eq!(renderer.capabilities(), &promised(), "nothing opened it, so it answers with the promise");
        assert_eq!(renderer.stats(), ResourceStats::default());
        assert_eq!(services.get::<Capabilities>(), Some(&promised()));
        assert!(services.get::<dyn CharacterBuilder>().is_some() && services.get::<BuildServices>().is_some());
        assert!(!LazyRenderer::new(HeadlessOptions::default()).opened());
    }

    /// What the ops read before the device opens is what the opened renderer has, and the floors are floors. Skips
    /// where no GPU adapter exists (lavapipe is one, in CI and the dev VMs).
    #[test]
    fn the_promise_matches_the_real_renderer() {
        let Ok(real) = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }) else {
            eprintln!("skipped: no GPU adapter");
            return;
        };
        let real = real.capabilities();
        let promised = promised();
        assert_eq!(promised.mtoon, real.mtoon);
        assert_eq!(promised.dynamic_meshes, real.dynamic_meshes);
        assert_eq!(promised.morph_targets, real.morph_targets);
        assert_eq!(promised.max_morph_targets, real.max_morph_targets);
        assert_eq!(promised.bit_exact, real.bit_exact);
        assert!(promised.max_texture_size <= real.max_texture_size);
        assert!(promised.max_samples <= real.max_samples);
    }

    /// Once a call opened it, the lazy renderer is the real one: it answers with the device's capabilities.
    #[test]
    fn a_call_opens_the_device_and_then_the_real_capabilities_answer() {
        let mut renderer = LazyRenderer::new(HeadlessOptions { adapter: AdapterPreference::Software });
        let opened = renderer.create_target(&TargetDesc {
            size: omuzaki_core::glam::UVec2::new(16, 16),
            alpha: omuzaki_render_api::AlphaEncoding::default(),
        });
        assert!(renderer.opened());
        match opened {
            Ok(target) => {
                assert_ne!(renderer.capabilities().backend, promised().backend, "the device's own name");
                renderer.release(target.into()).expect("released");
            }
            Err(error) => {
                assert!(error.to_string().contains("no GPU"), "{error}");
                let explained = explain(OpError::internal(error.to_string()));
                assert_eq!(explained.code, ErrorCode::Unavailable, "{explained:?}");
            }
        }
    }

    #[test]
    fn other_errors_pass_through_explain() {
        let invalid = OpError::invalid("size", "too big");
        assert_eq!(explain(invalid.clone()), invalid);
    }
}
