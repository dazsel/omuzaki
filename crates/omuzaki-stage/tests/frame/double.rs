//! A recording `Renderer` double for the frame loop's test and benchmark: it forwards to the render mock, except that
//! `render` returns a canned report without drawing (and `update_vertices` does not copy), so the stage's own cost can be measured, and every
//! `update_vertices` call is logged.

use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_render_api::{
    AlphaEncoding, Camera, Capabilities, FrameReport, Image, Material, MaterialId, MeshData, MeshId, MeshOptions,
    Painter, Quality, RenderError, Renderer, ResourceRef, ResourceStats, Scene, TargetDesc, TargetId, TextureDesc,
    TextureId,
};
use omuzaki_render_mock::MockRenderer;

/// The mock renderer with a log of vertex updates and, optionally, no drawing.
pub struct Recorder {
    pub inner: MockRenderer,
    /// The mesh of every `update_vertices` call, in order. Reserve it before measuring allocations.
    pub updates: Vec<MeshId>,
    /// The report `render` returns when `draw` is off.
    pub canned: FrameReport,
    /// Whether `render` really draws (through the mock) or only answers `canned`.
    pub draw: bool,
}

impl Recorder {
    /// A recorder over a fresh mock renderer. `draw` off needs one real frame first, which this takes.
    pub fn new(draw: bool) -> Self {
        let mut inner = MockRenderer::new();
        let target = inner.create_target(&TargetDesc { size: UVec2::new(8, 8), alpha: AlphaEncoding::default() }).ok();
        let canned = target
            .and_then(|t| inner.render(t, &Scene::new(Camera::default()), &Quality::live()).ok())
            .expect("the mock renders an empty scene");
        if let Some(t) = target {
            inner.release(t.into()).expect("the target exists");
        }
        Self { inner, updates: Vec::with_capacity(1 << 16), canned, draw }
    }
}

impl Renderer for Recorder {
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
        self.updates.push(mesh);
        // The mock copies the vertices into a fresh buffer; a run that only measures the stage skips that.
        if self.draw { self.inner.update_vertices(mesh, positions, normals) } else { Ok(()) }
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

    fn release(&mut self, resource: ResourceRef) -> Result<(), RenderError> {
        self.inner.release(resource)
    }

    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError> {
        if self.draw { self.inner.render(target, scene, quality) } else { Ok(self.canned) }
    }

    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        self.inner.read_target(target)
    }
}

/// The mock character, built with the reference builder.
pub fn built() -> omuzaki_char_api::BuiltCharacter {
    use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, RegistryBuilder};
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    RegistryBuilder
        .build(&CharId::new("mock").expect("a valid id"), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds")
}
