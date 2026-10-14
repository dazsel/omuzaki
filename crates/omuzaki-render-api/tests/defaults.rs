//! A renderer that implements only the required methods still compiles, and the new optional ones behave as the
//! contract says: default options forward to `create_mesh`, everything else is `Unsupported`.

use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_render_api::{
    Capabilities, FrameReport, Image, Material, MaterialId, MeshData, MeshId, MeshOptions, MeshUsage, MtoonSupport,
    Painter, Quality, RenderError, Renderer, ResourceRef, ResourceStats, Scene, TargetDesc, TargetId, TextureDesc,
    TextureId,
};

/// Counts `create_mesh` calls and refuses everything else.
struct Minimal {
    caps: Capabilities,
    meshes: u32,
}

impl Minimal {
    fn new() -> Self {
        let caps = Capabilities {
            backend: "minimal".into(),
            max_texture_size: 4096,
            max_samples: 1,
            passes: Vec::new(),
            shading_programs: Vec::new(),
            gpu_timings: false,
            bit_exact: false,
            dynamic_meshes: false,
            morph_targets: false,
            max_morph_targets: 0,
            mtoon: MtoonSupport::NONE,
        };
        Self { caps, meshes: 0 }
    }
}

fn no<T>() -> Result<T, RenderError> {
    Err(RenderError::Backend("not part of this test".into()))
}

impl Renderer for Minimal {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
    fn stats(&self) -> ResourceStats {
        ResourceStats::default()
    }
    fn create_mesh(&mut self, _: &MeshData) -> Result<MeshId, RenderError> {
        self.meshes += 1;
        Ok(MeshId::from_parts(self.meshes, 1))
    }
    fn create_texture(&mut self, _: &TextureDesc, _: Option<&[u8]>) -> Result<TextureId, RenderError> {
        no()
    }
    fn paint_texture(&mut self, _: TextureId, _: &mut dyn Painter) -> Result<(), RenderError> {
        no()
    }
    fn create_material(&mut self, _: &Material) -> Result<MaterialId, RenderError> {
        no()
    }
    fn update_material(&mut self, _: MaterialId, _: &Material) -> Result<(), RenderError> {
        no()
    }
    fn create_target(&mut self, _: &TargetDesc) -> Result<TargetId, RenderError> {
        no()
    }
    fn resize_target(&mut self, _: TargetId, _: UVec2) -> Result<(), RenderError> {
        no()
    }
    fn release(&mut self, _: ResourceRef) -> Result<(), RenderError> {
        no()
    }
    fn render(&mut self, _: TargetId, _: &Scene, _: &Quality) -> Result<FrameReport, RenderError> {
        no()
    }
    fn read_target(&mut self, _: TargetId) -> Result<Image, RenderError> {
        no()
    }
}

fn triangle() -> MeshData {
    MeshData {
        positions: vec![Vec3::ZERO, Vec3::X, Vec3::Y],
        normals: vec![Vec3::Z; 3],
        indices: vec![0, 1, 2],
        ..MeshData::default()
    }
}

#[test]
fn default_options_forward_to_create_mesh() {
    let mut r = Minimal::new();
    assert!(r.create_mesh_with(&triangle(), &MeshOptions::default()).is_ok());
    assert_eq!(r.meshes, 1);
}

#[test]
fn other_options_and_vertex_updates_are_unsupported_by_default() {
    let mut r = Minimal::new();
    let dynamic = MeshOptions { usage: MeshUsage::Dynamic, morphs: None };
    assert!(matches!(r.create_mesh_with(&triangle(), &dynamic), Err(RenderError::Unsupported(_))));
    assert_eq!(r.meshes, 0, "nothing was created");
    let mesh = MeshId::from_parts(0, 1);
    assert!(matches!(r.update_vertices(mesh, &[], &[]), Err(RenderError::Unsupported(_))));
}
