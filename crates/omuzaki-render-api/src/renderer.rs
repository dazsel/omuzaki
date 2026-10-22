//! The [`Renderer`] trait: the one entry point every surface, system and test uses to draw.

use std::any::Any;

use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::id::Slug;

use crate::{
    AlphaEncoding, EnvironmentDesc, EnvironmentId, FrameReport, Image, Material, MaterialId, MeshData, MeshId,
    MeshOptions, Pass, PixelRect, Quality, RenderError, ResourceRef, ResourceStats, Scene, TargetDesc, TargetId,
    TextureDesc, TextureId,
};

/// What a renderer can do, so callers adapt instead of guessing.
// One flag per independent capability is the point of this struct; a state machine would not model them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// A human-readable name for logs and bug reports, for example `"mock-cpu"` or `"wgpu Vulkan (NVIDIA RTX 4090)"`.
    pub backend: String,
    /// The largest texture or target side. At least [`limits::GUARANTEED_TEXTURE_SIZE`](crate::limits).
    pub max_texture_size: u32,
    /// The largest `samples` the renderer honours without falling back to fewer.
    pub max_samples: u8,
    /// The passes this renderer can run.
    pub passes: Vec<Pass>,
    /// The names of the shading programs this renderer knows ([`Shading::Program`](crate::Shading::Program)). A material
    /// naming any other program is drawn with the toon fallback instead of failing.
    pub shading_programs: Vec<Slug>,
    /// Whether [`FrameReport::gpu`] is ever filled in.
    pub gpu_timings: bool,
    /// Whether rendering the same inputs twice gives identical bytes. True for the mock; real GPUs promise it only to
    /// within a rounding step, so golden compares use a tolerance for them.
    pub bit_exact: bool,
    /// Whether [`MeshUsage::Dynamic`](crate::MeshUsage) meshes and [`Renderer::update_vertices`] work.
    pub dynamic_meshes: bool,
    /// Whether meshes can be created with a [`MorphSet`](crate::MorphSet) and instances morph them.
    pub morph_targets: bool,
    /// The most morph targets one mesh may have; 0 if `morph_targets` is false.
    pub max_morph_targets: u32,
    /// The `MToon` groups this renderer draws: [`MtoonSupport::OUTLINE_STYLE`] on the wgpu and mock renderers.
    pub mtoon: MtoonSupport,
}

/// The `MToon` feature groups of `docs/design/byom.md` D7 that a renderer draws, as a bitset, so an importer can tell
/// the author what will look different instead of guessing. A material asking for a group the renderer lacks is
/// drawn without it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct MtoonSupport(u8);

impl MtoonSupport {
    /// No `MToon` group.
    pub const NONE: Self = Self(0);
    /// The matcap texture.
    pub const MATCAP: Self = Self(1);
    /// Global-illumination equalisation.
    pub const GI_EQUALISATION: Self = Self(1 << 1);
    /// UV animation (scroll and rotation).
    pub const UV_ANIMATION: Self = Self(1 << 2);
    /// The render-queue offset (draw order among transparent materials).
    pub const RENDER_QUEUE_OFFSET: Self = Self(1 << 3);
    /// Outline width in screen space.
    pub const OUTLINE_WIDTH_SCREEN: Self = Self(1 << 4);
    /// Mixing the rim light with the scene lighting.
    pub const RIM_LIGHTING_MIX: Self = Self(1 << 5);
    /// Mixing the outline colour with the scene lighting.
    pub const OUTLINE_LIGHTING_MIX: Self = Self(1 << 6);
    /// The outline width multiplier texture.
    pub const OUTLINE_WIDTH_TEXTURE: Self = Self(1 << 7);
    /// The outline groups `Material::outline_style` carries (#647): screen-space widths, the lighting mix and the width
    /// texture. Every renderer that implements the style draws all three.
    pub const OUTLINE_STYLE: Self =
        Self(Self::OUTLINE_WIDTH_SCREEN.0 | Self::OUTLINE_LIGHTING_MIX.0 | Self::OUTLINE_WIDTH_TEXTURE.0);

    /// True if every group in `group` is drawn (so `contains(NONE)` is always true).
    pub const fn contains(self, group: Self) -> bool {
        self.0 & group.0 == group.0
    }

    /// This set plus `group`.
    #[must_use]
    pub const fn with(self, group: Self) -> Self {
        Self(self.0 | group.0)
    }
}

impl Capabilities {
    /// True if the renderer can run `pass`.
    pub fn supports(&self, pass: Pass) -> bool {
        self.passes.contains(&pass)
    }
}

/// The destination a [`Painter`] draws into: one dynamic texture, for the length of one call.
pub trait PaintTarget {
    /// The texture's description (size, colour space, alpha encoding). Painters write pixels that match it.
    fn desc(&self) -> &TextureDesc;

    /// Writes `rgba` (tightly packed RGBA8, `rect.byte_len()` bytes) into `rect`. The portable path: it works on every
    /// renderer, so CPU painters, tests and the mock all use it.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] if `rect` doesn't fit the texture or `rgba` has the wrong length.
    fn write_rgba(&mut self, rect: PixelRect, rgba: &[u8]) -> Result<(), RenderError>;

    /// The renderer's own handle to the texture, for GPU painters that draw into it without a CPU round trip (the
    /// face painter's ≤ 0.5 ms budget needs this). A painter downcasts it to the concrete type its sibling renderer
    /// documents, and falls back to [`write_rgba`](PaintTarget::write_rgba) if the downcast fails. No backend type
    /// appears in this contract: the two implementation crates agree on the type out of band
    /// (`docs/design/render-api.md`, "GPU interop"). For the wgpu renderer that is a tuple with the renderer's command
    /// encoder: the painter records into it and the renderer submits it when the painter returns, so the painter's GPU
    /// work is part of the renderer's frame and of its GPU timings.
    ///
    /// A painter that draws through the handle can't be followed by the renderer, so after a call the whole texture
    /// counts as changed, unless the painter says what it changed with [`mark_damage`](PaintTarget::mark_damage).
    fn native(&mut self) -> &mut dyn Any;

    /// Reports that this paint changed only `rect` of what it drew through [`native`](PaintTarget::native), so the
    /// renderer refreshes only that part of the texture (its mip chain, say). Calls add up (the union counts); an empty
    /// rectangle reports nothing, and a rectangle reaching outside the texture is clipped to it. Pixels written with
    /// [`write_rgba`](PaintTarget::write_rgba) count on their own and need no call. The default ignores it: a renderer
    /// that refreshes nothing, or always everything, has no use for it.
    fn mark_damage(&mut self, rect: PixelRect) {
        let _ = rect;
    }
}

/// Something that fills a dynamic texture: the face painter (`omuzaki-face`), a video frame source, a debug overlay.
pub trait Painter {
    /// Draws into `target`. Called synchronously from [`Renderer::paint_texture`]; the renderer orders the work
    /// before any later `render`, so the painter never waits on the GPU.
    ///
    /// # Errors
    /// Whatever stops the painter; the texture then keeps its previous contents.
    fn paint(&mut self, target: &mut dyn PaintTarget) -> Result<(), RenderError>;
}

/// A closure is the smallest painter.
impl<F> Painter for F
where
    F: FnMut(&mut dyn PaintTarget) -> Result<(), RenderError>,
{
    fn paint(&mut self, target: &mut dyn PaintTarget) -> Result<(), RenderError> {
        self(target)
    }
}

/// The renderer: resources in, frames out.
///
/// # Lifetimes
/// Meshes, textures, materials and targets are *resources*: created once, referenced by `Copy` handles, and released
/// explicitly. Rules every implementation follows:
/// - **Creation is immediate.** A handle can be used by the very next call; uploads are ordered before any later draw.
/// - **Release is deferred but handles die at once.** After [`release`](Renderer::release) the handle is invalid
///   (`UnknownHandle`, never a panic or a reused slot: handles carry a generation); the GPU memory is freed once no
///   frame in flight uses it.
/// - **Dependencies are checked at release.** A texture a live material uses can't be released (`InUse`): release the
///   material first. Scenes are values, not resources, so a scene naming a released mesh or material fails at
///   `render` with `UnknownHandle`.
/// - **Explicit, not RAII.** Handles are `Copy` and sit inside [`Scene`]s that are rebuilt every frame; a drop guard
///   would force reference counting into that hot path. Leaks are caught instead by [`Renderer::stats`], which tests
///   assert returns to its baseline.
/// - **Device loss is terminal.** After `DeviceLost` every handle is invalid. The app builds a new renderer and
///   uploads again from its CPU copies (the store, the face painter's state), which is why this API never hands back
///   GPU-only data except through [`read_target`](Renderer::read_target).
/// - **One thread.** All calls come from the render thread (spec §8); `&mut self` makes that a compile-time rule.
///
/// # Determinism
/// `render` is a pure function of the resources' contents, the [`Scene`] and the [`Quality`]. In
/// [`QualityMode::Offline`](crate::QualityMode) it never depends on earlier frames or on wall time; randomness (grain,
/// sample jitter) derives from `Scene::frame.index`.
pub trait Renderer: Send {
    /// What this renderer can do. Constant for its lifetime.
    fn capabilities(&self) -> &Capabilities;

    /// What it currently holds.
    fn stats(&self) -> ResourceStats;

    /// Uploads a mesh.
    ///
    /// # Errors
    /// [`RenderError::Invalid`] (see [`MeshData::validate`]), `OutOfMemory` or `DeviceLost`.
    fn create_mesh(&mut self, mesh: &MeshData) -> Result<MeshId, RenderError>;

    /// Uploads a mesh with options: a [`MeshUsage::Dynamic`](crate::MeshUsage) hint and/or sparse morph targets.
    ///
    /// The default forwards default options to [`create_mesh`](Renderer::create_mesh) and says
    /// [`RenderError::Unsupported`] for anything else, so a renderer that lacks these features still compiles; check
    /// [`Capabilities::dynamic_meshes`] and [`Capabilities::morph_targets`] first.
    ///
    /// # Errors
    /// As [`create_mesh`](Renderer::create_mesh), plus `Invalid` for a [`MorphSet`](crate::MorphSet) that doesn't fit
    /// the mesh ([`MorphSet::validate`](crate::MorphSet::validate)), `LimitExceeded` above
    /// [`Capabilities::max_morph_targets`], and `Unsupported`.
    fn create_mesh_with(&mut self, mesh: &MeshData, options: &MeshOptions) -> Result<MeshId, RenderError> {
        if *options == MeshOptions::default() {
            self.create_mesh(mesh)
        } else {
            Err(RenderError::Unsupported("mesh options (dynamic meshes and morph targets)"))
        }
    }

    /// Replaces the positions and normals of a [`MeshUsage::Dynamic`](crate::MeshUsage) mesh: cloth, soft bodies,
    /// CPU-deformed hair. The topology, UVs, colours, skinning data and morph targets stay; morphs and skinning apply
    /// on top of the new vertices, as they do on the creation-time ones.
    ///
    /// The default says [`RenderError::Unsupported`].
    ///
    /// # Errors
    /// `UnknownHandle`; `Invalid` for a [`Static`](crate::MeshUsage::Static) mesh, a `positions` or `normals` length
    /// that isn't the mesh's vertex count, or a non-finite value; `Unsupported`; `DeviceLost`.
    fn update_vertices(&mut self, mesh: MeshId, positions: &[Vec3], normals: &[Vec3]) -> Result<(), RenderError> {
        let _ = (mesh, positions, normals);
        Err(RenderError::Unsupported("dynamic meshes"))
    }

    /// Creates a texture, optionally with initial pixels (`desc.byte_len()` bytes of RGBA8, rows top to bottom).
    /// With `None` it starts fully transparent black.
    ///
    /// # Errors
    /// `Invalid` for a bad description or a wrong pixel count, `LimitExceeded` above `max_texture_size`,
    /// `OutOfMemory` or `DeviceLost`.
    fn create_texture(&mut self, desc: &TextureDesc, rgba: Option<&[u8]>) -> Result<TextureId, RenderError>;

    /// Lets `painter` draw into a [`TextureUsage::Dynamic`](crate::TextureUsage) texture. The contents persist
    /// between frames, so a painter runs only when its picture changed. GPU time is reported under
    /// [`Pass::PaintTextures`] in the next frame's report.
    ///
    /// # Errors
    /// `UnknownHandle`, `Invalid` if the texture is static, or whatever the painter returns.
    fn paint_texture(&mut self, texture: TextureId, painter: &mut dyn Painter) -> Result<(), RenderError>;

    /// Creates a material. Every texture it names must be live.
    ///
    /// # Errors
    /// `Invalid` (see [`Material::validate`]), `UnknownHandle` for a dead texture, or `DeviceLost`.
    fn create_material(&mut self, material: &Material) -> Result<MaterialId, RenderError>;

    /// Replaces a material's contents in place, keeping its handle: palette swaps, expression-driven uniforms, and
    /// pointing an overlay at a resized face atlas.
    ///
    /// # Errors
    /// As [`create_material`](Renderer::create_material), plus `UnknownHandle` for `id`.
    fn update_material(&mut self, id: MaterialId, material: &Material) -> Result<(), RenderError>;

    /// Creates an environment (image-based light) for [`Scene::environment`]. The renderer derives the diffuse
    /// irradiance and the prefiltered specular chain here, so a frame filters nothing.
    ///
    /// The default says [`RenderError::Unsupported`], so a renderer that doesn't light with environments yet still
    /// compiles; a scene that names no environment is unaffected.
    ///
    /// # Errors
    /// `Invalid` (see [`EnvironmentDesc::validate`]), `LimitExceeded` for an HDRI above `max_texture_size`,
    /// `Unsupported`, `OutOfMemory` or `DeviceLost`.
    fn create_environment(&mut self, desc: &EnvironmentDesc) -> Result<EnvironmentId, RenderError> {
        let _ = desc;
        Err(RenderError::Unsupported("environments"))
    }

    /// Creates a render target. A renderer can have several (editor viewport, stream output, thumbnails) sharing all
    /// the same meshes, textures and materials.
    ///
    /// # Errors
    /// `Invalid` for a zero side, `LimitExceeded` above `max_texture_size`, `OutOfMemory` or `DeviceLost`.
    fn create_target(&mut self, desc: &TargetDesc) -> Result<TargetId, RenderError>;

    /// Changes a target's size (a window resize). Its previous contents are discarded.
    ///
    /// # Errors
    /// As [`create_target`](Renderer::create_target), plus `UnknownHandle`.
    fn resize_target(&mut self, target: TargetId, size: UVec2) -> Result<(), RenderError>;

    /// Releases a resource; see the lifetime rules above.
    ///
    /// # Errors
    /// `UnknownHandle` if it is already released (a double release is an error, not a no-op), `InUse` for a texture
    /// some live material still reads.
    fn release(&mut self, resource: ResourceRef) -> Result<(), RenderError>;

    /// Draws `scene` into `target`, running the passes of [`Pass::ALL`] in order, and says how long each took.
    ///
    /// # Errors
    /// `Invalid` or `LimitExceeded` for a bad scene or quality ([`Scene::validate`], [`Quality::validate`]),
    /// `UnknownHandle` for a dead mesh, material or target, or `DeviceLost`. On error the target keeps its previous
    /// frame.
    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError>;

    /// Copies the last frame rendered into `target` to the CPU. It waits for the GPU, so it is for tests, goldens,
    /// thumbnails and CPU outputs, not for the live path (live outputs share the target's texture instead, see
    /// `omuzaki-stream-api`).
    ///
    /// # Errors
    /// `UnknownHandle`, or `Invalid` if nothing has been rendered into the target yet.
    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError>;

    /// The native GPU handles of `target`'s output, so the app's viewport and the live outputs use the rendered frame
    /// without a copy to the CPU (the rendered-frame contract, #629). `None` from a renderer without a GPU (the mock),
    /// whose frames reach outputs through [`read_target`](Renderer::read_target); that is the default.
    ///
    /// The value is the renderer's documented convention, read with `downcast_ref`, so no GPU type enters this crate.
    /// The wgpu renderer's is `omuzaki_render::NativeTarget`, the tuple `(wgpu::Device, wgpu::Queue, wgpu::Texture)`:
    /// the device and queue it renders with, and the output texture, sRGB-encoded RGBA8 at the target's size in the
    /// target's alpha encoding, usable for sampling and as a copy source. This is the form `omuzaki-stream-api`'s
    /// `Frame::gpu` names. The next [`render`](Renderer::render) into the target draws into the same texture, so a
    /// consumer samples or copies it first (a copy submitted after one `render` and before the next sees that frame,
    /// by queue order), and [`resize_target`](Renderer::resize_target) replaces it.
    ///
    /// # Errors
    /// `UnknownHandle` for a target that is not live, from a renderer that has native handles.
    fn target_native(&self, target: TargetId) -> Result<Option<&dyn Any>, RenderError> {
        let _ = target;
        Ok(None)
    }
}

/// Conveniences built from the required methods, kept off [`Renderer`] so it stays small and every implementation
/// gets them for free.
pub trait RendererExt: Renderer {
    /// Renders one frame headless and returns its pixels: the call goldens, thumbnails and `render.preview` use. It
    /// creates a throwaway target, so the output size is independent of any window.
    ///
    /// # Errors
    /// Anything [`create_target`](Renderer::create_target), [`render`](Renderer::render) or
    /// [`read_target`](Renderer::read_target) returns. The target is released either way.
    fn render_image(
        &mut self,
        size: UVec2,
        alpha: AlphaEncoding,
        scene: &Scene,
        quality: &Quality,
    ) -> Result<Image, RenderError> {
        let target = self.create_target(&TargetDesc { size, alpha })?;
        let image = self.render(target, scene, quality).and_then(|_| self.read_target(target));
        let released = self.release(target.into());
        let image = image?;
        released.map(|()| image)
    }

    /// Overwrites `rect` of a dynamic texture with `rgba`: [`paint_texture`](Renderer::paint_texture) with the
    /// simplest painter.
    ///
    /// # Errors
    /// As [`paint_texture`](Renderer::paint_texture) and [`PaintTarget::write_rgba`].
    fn update_texture(&mut self, texture: TextureId, rect: PixelRect, rgba: &[u8]) -> Result<(), RenderError> {
        self.paint_texture(texture, &mut |target: &mut dyn PaintTarget| target.write_rgba(rect, rgba))
    }
}

impl<R: Renderer + ?Sized> RendererExt for R {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mtoon_support_is_a_bitset_of_independent_groups() {
        let groups = [
            MtoonSupport::MATCAP,
            MtoonSupport::GI_EQUALISATION,
            MtoonSupport::UV_ANIMATION,
            MtoonSupport::RENDER_QUEUE_OFFSET,
            MtoonSupport::OUTLINE_WIDTH_SCREEN,
            MtoonSupport::RIM_LIGHTING_MIX,
            MtoonSupport::OUTLINE_LIGHTING_MIX,
        ];
        assert_eq!(MtoonSupport::default(), MtoonSupport::NONE);
        for (i, group) in groups.into_iter().enumerate() {
            assert!(!MtoonSupport::NONE.contains(group), "group {i} starts off");
            assert!(MtoonSupport::NONE.contains(MtoonSupport::NONE));
            let only = MtoonSupport::NONE.with(group);
            assert!(only.contains(group));
            for (j, other) in groups.into_iter().enumerate().filter(|&(j, _)| j != i) {
                assert!(!only.contains(other), "group {i} doesn't switch on group {j}");
            }
        }
        let all = groups.into_iter().fold(MtoonSupport::NONE, MtoonSupport::with);
        assert!(groups.into_iter().all(|g| all.contains(g)));
        assert!(all.contains(MtoonSupport::MATCAP.with(MtoonSupport::UV_ANIMATION)), "contains checks every group");
        assert!(!MtoonSupport::MATCAP.contains(MtoonSupport::MATCAP.with(MtoonSupport::UV_ANIMATION)));
    }
}
