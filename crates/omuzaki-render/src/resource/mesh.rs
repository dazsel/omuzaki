//! Mesh upload: two vertex streams with a fixed layout (what moves, and what does not), and a `u32` index buffer.
//!
//! A single fixed layout (with neutral defaults for what a mesh lacks) lets every pass share one vertex input and one
//! set of pipelines. The spike proved this for the whole character, hair included.
//!
//! Beside the vertex buffer sits a second, smaller one holding each vertex's *smooth normal*: the normals of every
//! vertex at the same position, averaged. The outline pass pushes vertices along it, because the shading normals of a
//! hard-edged mesh differ at a corner and would tear the hull apart there.
//!
//! A dynamic mesh finds those points once, when it is created: [`GpuMesh::update_vertices`] cannot change which
//! vertices form a triangle, so the vertices welded at creation stay welded and the seams of the bind pose are the seams
//! of every frame. A frame then only re-averages the normals (`resmooth`), which costs a pass over the vertices instead
//! of a weld.

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render_api::{MeshData, MeshOptions, MeshUsage, RenderError};
use wgpu::util::DeviceExt;

use crate::gpu::Gpu;
use crate::morph::{MeshMorphs, MorphInput};
use crate::program::AttributeSpec;
use crate::shadow::bounds::MeshBounds;

/// The part of a vertex that moves: what `update_vertices` and morph targets rewrite. Vertex buffer slot 0, 24 bytes.
///
/// A vertex is read from two streams (`Vertex` in `shader/common.wgsl`) so that rewriting a mesh uploads only these 24
/// bytes of its 72, a third of the traffic, and a morphed copy of a mesh shares the rest ([`Surface`]) with the original.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct Shape {
    pub(crate) position: [f32; 3],
    pub(crate) normal: [f32; 3],
}

/// The part of a vertex that stays put: UVs, colours and skin data, with neutral defaults for what a mesh lacks. Vertex
/// buffer slot 1, 48 bytes.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct Surface {
    uv: [f32; 2],
    colour: [f32; 4],
    pub(crate) joints: [u16; 4],
    pub(crate) weights: [f32; 4],
}

const SHAPE_ATTRIBUTES: [wgpu::VertexAttribute; 2] = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3];
const SURFACE_ATTRIBUTES: [wgpu::VertexAttribute; 4] =
    wgpu::vertex_attr_array![2 => Float32x2, 3 => Float32x4, 4 => Uint16x4, 5 => Float32x4];

/// The two vertex buffer layouts (slots 0 and 1) every scene pipeline starts with.
pub(crate) fn vertex_layouts() -> [Option<wgpu::VertexBufferLayout<'static>>; 2] {
    let stream = |stride: usize, attributes| wgpu::VertexBufferLayout {
        array_stride: stride as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes,
    };
    [Some(stream(size_of::<Shape>(), &SHAPE_ATTRIBUTES)), Some(stream(size_of::<Surface>(), &SURFACE_ATTRIBUTES))]
}

const SMOOTH_NORMAL_ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![6 => Float32x3];

/// The layout of the smooth-normal stream (vertex buffer slot 2, shader location 6). Only the outline pass binds it.
pub(crate) fn smooth_normal_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: size_of::<[f32; 3]>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &SMOOTH_NORMAL_ATTRIBUTES,
    }
}

/// The vertex buffer slot after the two vertex streams: where the extra streams (and the smooth normals) start.
const FIRST_FREE_SLOT: u32 = 2;
/// Where the extra streams start: [`FIRST_FREE_SLOT`] and shader location 6, then upward (`Extras` in
/// `shader/toon.wgsl`).
const EXTRA_FIRST_SLOT: u32 = FIRST_FREE_SLOT;
const EXTRA_FIRST_LOCATION: u32 = 6;

/// Vertex buffers a scene pipeline binds at most: the two vertex streams and every extra attribute stream. The device
/// must allow this many (`caps::REQUIREMENTS`); the outline pass binds fewer.
pub(crate) const MAX_VERTEX_BUFFERS: usize = FIRST_FREE_SLOT as usize + crate::program::MAX_ATTRIBUTES;

/// The vertex attribute of extra stream `slot` (0-based) holding `components` floats.
pub(crate) fn extra_attribute(slot: usize, components: u8) -> wgpu::VertexAttribute {
    let format = match components {
        1 => wgpu::VertexFormat::Float32,
        2 => wgpu::VertexFormat::Float32x2,
        3 => wgpu::VertexFormat::Float32x3,
        _ => wgpu::VertexFormat::Float32x4,
    };
    wgpu::VertexAttribute {
        format,
        offset: 0,
        shader_location: EXTRA_FIRST_LOCATION + u32::try_from(slot).unwrap_or(0),
    }
}

/// The layout of extra stream `slot`: `components` floats per vertex when the mesh has the attribute, or stride 0 over
/// one shared buffer of zeros when it does not (the same value for every vertex).
pub(crate) fn extra_layout(
    attribute: &wgpu::VertexAttribute,
    components: u8,
    present: bool,
) -> wgpu::VertexBufferLayout<'_> {
    wgpu::VertexBufferLayout {
        array_stride: if present { u64::from(components) * 4 } else { 0 },
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: std::slice::from_ref(attribute),
    }
}

/// An extra attribute of a mesh, uploaded as its own tightly packed stream.
struct ExtraStream {
    name: Slug,
    components: u8,
    buffer: wgpu::Buffer,
}

/// What a dynamic mesh keeps on the CPU. Where the GPU runs compute, the outline directions are welded by a dispatch
/// ([`crate::morph`]) and a mesh without morph targets keeps no copy of its vertices ([`Shadow::lean`]); otherwise the
/// copy and the directions are what [`GpuMesh::refresh`] works from.
struct Shadow {
    /// The vertices, kept in step with the GPU's. Empty for a lean mesh.
    shapes: Vec<Shape>,
    /// The skin data, for refitting the shadow bounds; empty for a rigid mesh.
    skin: Vec<Surface>,
    /// [`GpuMesh::smooth_normals`]'s contents. Empty where a dispatch welds them.
    smooth: Vec<[f32; 3]>,
    /// The weld slot of every vertex, found once from the positions the mesh was created with: vertices at one point
    /// share a slot. Updates never weld again (see the module documentation).
    welds: Vec<usize>,
    /// How many welded points there are, for the scratch below.
    weld_count: usize,
    /// The first vertex of each slot, against which [`GpuMesh::split_seam`] checks that welded vertices still meet.
    anchors: Vec<usize>,
    /// Scratch for the per-point sums of [`resmooth`], sized once so a refresh allocates nothing. Empty where a dispatch
    /// welds the directions.
    sums: Vec<Vec3>,
    /// How many vertices the mesh has: `shapes.len()`, which a lean mesh does not keep.
    vertices: usize,
    /// The mesh keeps no CPU copy of its vertices: an update interleaves them straight into the upload's staging memory
    /// and refits the bounds from the new positions as it goes, which halves the memory traffic of an update and leaves
    /// only the outline directions deferred. Never set for a mesh with morph targets, whose blend reads the copy.
    lean: bool,
    /// A dispatch welds the outline directions from the GPU's stream ([`GpuMesh::refresh`] says when one is due).
    gpu_weld: bool,
    /// The outline directions lag the vertices; the outline pass is the only reader ([`GpuMesh::refresh`]).
    smooth_stale: bool,
    /// The mesh's shadow bounds lag the vertices; shadow fitting and the blended sort are the readers. Never set on a
    /// lean mesh, which refits them in the update.
    bounds_stale: bool,
}

/// What a frame reads of a dynamic mesh's lagging state ([`GpuMesh::refresh`]).
#[derive(Copy, Clone, Default)]
pub(crate) struct Needs {
    /// The outline pass draws the mesh's own vertex stream.
    pub(crate) hull: bool,
    /// Shadow fitting or the blended sort reads the mesh's bounds.
    pub(crate) bounds: bool,
}

/// The data of a GPU weld of a dynamic mesh's outline directions ([`GpuMesh::weld_target`]).
pub(crate) struct WeldTarget<'a> {
    /// The weld slot of every vertex.
    pub(crate) welds: &'a [usize],
    /// How many slots there are.
    pub(crate) weld_count: usize,
    /// The mesh's shape stream, which the dispatch reads.
    pub(crate) shapes: &'a wgpu::Buffer,
    /// The mesh's outline directions, which it writes.
    pub(crate) directions: &'a wgpu::Buffer,
}

/// A mesh on the GPU.
pub(crate) struct GpuMesh {
    shapes: wgpu::Buffer,
    surface: wgpu::Buffer,
    /// One unit vector per vertex, in the order of `shapes` ([`smooth_by_slots`]).
    smooth_normals: wgpu::Buffer,
    shadow: Option<Shadow>,
    /// Created with [`MeshUsage::Dynamic`].
    dynamic: bool,

    indices: wgpu::Buffer,
    /// The mesh's `extra` attributes, which shading programs read (`program/mod.rs`).
    extras: Vec<ExtraStream>,
    index_count: u32,
    /// Whether the vertices carry skin data. An instance skins only when its mesh does, as in the mock.
    pub(crate) skinned: bool,
    /// How many joints an instance must supply: one past the highest joint any vertex weights.
    pub(crate) joints_needed: u32,
    /// Where the mesh is, for fitting shadow maps (`shadow/`).
    pub(crate) bounds: MeshBounds,
    pub(crate) triangles: u64,
    pub(crate) bytes: u64,
    /// The sparse morph targets, if the mesh was created with any (`morph.rs`).
    morphs: Option<MeshMorphs>,
    /// Counts the changes of the base vertices, so a morphed copy knows when it is stale.
    vertex_version: u64,
}

impl GpuMesh {
    /// Uploads `mesh`, which the caller has validated along with `options`. `compute` says the renderer runs the GPU
    /// path (`morph/gpu.rs`): its compute shaders read the shape stream as storage (to blend morph targets onto it, and
    /// to weld a dynamic mesh's outline directions) and write the outline directions.
    pub(crate) fn upload(
        gpu: &Gpu,
        mesh: &MeshData,
        options: &MeshOptions,
        compute: bool,
    ) -> Result<Self, RenderError> {
        let index_count = u32::try_from(mesh.indices.len())
            .map_err(|_| RenderError::LimitExceeded { what: "indices in a mesh", limit: u64::from(u32::MAX) })?;
        let skinned = mesh.is_skinned();
        let joints_needed = joints_needed(mesh);
        let shapes: Vec<Shape> = mesh
            .positions
            .iter()
            .zip(&mesh.normals)
            .map(|(position, normal)| Shape { position: position.to_array(), normal: normal.to_array() })
            .collect();
        let surface: Vec<Surface> = (0..mesh.positions.len())
            .map(|i| Surface {
                uv: mesh.uvs.get(i).map_or([0.0; 2], Vec2::to_array),
                colour: mesh.colours.get(i).copied().unwrap_or([1.0; 4]),
                joints: mesh.joints.get(i).copied().unwrap_or([0; 4]),
                weights: mesh.weights.get(i).copied().unwrap_or([1.0, 0.0, 0.0, 0.0]),
            })
            .collect();
        let dynamic = options.usage == MeshUsage::Dynamic;
        let mut welder = Welder::with_capacity(mesh.positions.len());
        let mut welds = Vec::with_capacity(mesh.positions.len());
        let weld_count = welder.weld(mesh.positions.iter().copied(), &mut welds);
        let smooth: Vec<[f32; 3]> =
            smooth_by_slots(&welds, weld_count, &mesh.normals).iter().map(Vec3::to_array).collect();
        // A dynamic mesh's vertex buffers are rewritten by `queue.write_buffer`; the rest never are.
        let rewritable = if dynamic { wgpu::BufferUsages::COPY_DST } else { wgpu::BufferUsages::empty() };
        // The GPU morph blend reads the mesh's own shapes as the base its targets apply to, and a dynamic mesh's outline
        // directions are welded from them by a dispatch, into the direction stream.
        let welded_on_gpu = compute && dynamic;
        let lean = welded_on_gpu && options.morphs.is_none();
        let storage = |wanted: bool| if wanted { wgpu::BufferUsages::STORAGE } else { wgpu::BufferUsages::empty() };
        let blendable = storage(compute && options.morphs.is_some() || welded_on_gpu);
        let weldable = storage(welded_on_gpu);
        let buffer = |contents: &[u8], usage| {
            gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("mesh"), contents, usage })
        };
        let extras: Vec<ExtraStream> = mesh
            .extra
            .iter()
            .map(|a| ExtraStream {
                name: a.name.clone(),
                components: a.components,
                buffer: buffer(bytemuck::cast_slice(&a.data), wgpu::BufferUsages::VERTEX),
            })
            .collect();
        let extra_bytes: usize = mesh.extra.iter().map(|a| a.data.len() * size_of::<f32>()).sum();
        // A static morphed mesh keeps its base vertices for the blend; a dynamic one's are the shadow's below.
        let morphs = options.morphs.as_ref().map(|set| MeshMorphs {
            set: set.clone(),
            base: if dynamic { Vec::new() } else { shapes.clone() },
            skin: if dynamic || !skinned { Vec::new() } else { surface.clone() },
            welds: if dynamic { Vec::new() } else { welds.clone() },
            weld_count,
        });
        // A dynamic mesh rewrites its vertices, so it keeps them on the CPU; a static one keeps nothing.
        let shadow = dynamic.then(|| Shadow {
            shapes: if lean { Vec::new() } else { shapes.clone() },
            skin: if skinned { surface.clone() } else { Vec::new() },
            smooth: if welded_on_gpu { Vec::new() } else { smooth.clone() },
            anchors: anchors_of(&welds, weld_count),
            welds,
            weld_count,
            sums: Vec::with_capacity(if welded_on_gpu { 0 } else { shapes.len() }),
            vertices: shapes.len(),
            lean,
            gpu_weld: welded_on_gpu,
            smooth_stale: false,
            bounds_stale: false,
        });
        Ok(Self {
            shapes: buffer(bytemuck::cast_slice(&shapes), wgpu::BufferUsages::VERTEX | rewritable | blendable),
            surface: buffer(bytemuck::cast_slice(&surface), wgpu::BufferUsages::VERTEX),
            smooth_normals: buffer(bytemuck::cast_slice(&smooth), wgpu::BufferUsages::VERTEX | rewritable | weldable),
            shadow,
            dynamic,
            indices: buffer(bytemuck::cast_slice(&mesh.indices), wgpu::BufferUsages::INDEX),
            extras,
            index_count,
            skinned,
            joints_needed,
            bounds: MeshBounds::of(mesh, joints_needed),
            triangles: mesh.triangle_count() as u64,
            bytes: (shapes.len() * (size_of::<Shape>() + size_of::<Surface>() + size_of::<[f32; 3]>())
                + mesh.indices.len() * size_of::<u32>()
                + extra_bytes) as u64,
            morphs,
            vertex_version: 0,
        })
    }

    /// The mesh's own shape stream: the base a morph blend starts from.
    pub(crate) fn shapes_buffer(&self) -> &wgpu::Buffer {
        &self.shapes
    }

    /// Whether [`update_vertices`](Self::update_vertices) is allowed.
    pub(crate) fn is_dynamic(&self) -> bool {
        self.dynamic
    }

    /// How many vertices the mesh has.
    pub(crate) fn vertex_count(&self) -> usize {
        self.shadow.as_ref().map_or(0, |s| s.vertices)
    }

    /// What a blend of the mesh's morph targets needs, if it has targets: the vertices they apply to (the current ones,
    /// for a dynamic mesh), the skin data and welds for the bounds and outline directions of the result.
    pub(crate) fn morph_input(&self) -> Option<MorphInput<'_>> {
        let morphs = self.morphs.as_ref()?;
        Some(match &self.shadow {
            Some(s) => MorphInput {
                set: &morphs.set,
                base: &s.shapes,
                skin: &s.skin,
                welds: &s.welds,
                weld_count: s.weld_count,
                joints_needed: self.joints_needed,
            },
            None => MorphInput {
                set: &morphs.set,
                base: &morphs.base,
                skin: &morphs.skin,
                welds: &morphs.welds,
                weld_count: morphs.weld_count,
                joints_needed: self.joints_needed,
            },
        })
    }

    /// Changes whenever the base vertices do.
    pub(crate) fn vertex_version(&self) -> u64 {
        self.vertex_version
    }

    /// Replaces the positions and normals of a dynamic mesh with one `queue.write_buffer` of its shape stream (24 bytes
    /// a vertex: the UVs, colours and skin data are another stream and stay). The caller has checked that the mesh is
    /// dynamic, that both slices have [`vertex_count`](Self::vertex_count) elements and that all are finite.
    ///
    /// The shadow bounds and the outline directions follow when a frame draws the mesh ([`refresh`](Self::refresh)).
    /// The outline directions keep the welds of creation: vertices that only come together in an update stay apart.
    pub(crate) fn update_vertices(
        &mut self,
        queue: &wgpu::Queue,
        positions: &[Vec3],
        normals: &[Vec3],
    ) -> Result<(), RenderError> {
        let Some(shadow) = self.shadow.as_mut() else { return Ok(()) };
        // Floats in and out, a chunk a vertex, so the loop is plain moves the compiler vectorises.
        if shadow.lean {
            let size = wgpu::BufferSize::new((shadow.vertices * size_of::<Shape>()) as u64)
                .ok_or_else(|| RenderError::Backend("a vertex update of an empty mesh".into()))?;
            let mut staging = queue
                .write_buffer_with(&self.shapes, 0, size)
                .ok_or_else(|| RenderError::Backend("the queue could not stage a vertex update".into()))?;
            let (slots, _) = staging.slice(..).into_chunks::<{ size_of::<Shape>() }>();
            for (slot, (position, normal)) in slots.into_iter().zip(positions.iter().zip(normals)) {
                let [px, py, pz] = position.to_array();
                let [nx, ny, nz] = normal.to_array();
                slot.write(bytemuck::cast([px, py, pz, nx, ny, nz]));
            }
            drop(staging);
            self.bounds.refit_points(self.joints_needed, positions, &shadow.skin);
        } else {
            let (to, _) = bytemuck::cast_slice_mut::<Shape, f32>(&mut shadow.shapes).as_chunks_mut::<6>();
            let (position, _) = bytemuck::cast_slice::<Vec3, f32>(positions).as_chunks::<3>();
            let (normal, _) = bytemuck::cast_slice::<Vec3, f32>(normals).as_chunks::<3>();
            for ((to, [px, py, pz]), [nx, ny, nz]) in to.iter_mut().zip(position).zip(normal) {
                *to = [*px, *py, *pz, *nx, *ny, *nz];
            }
            queue.write_buffer(&self.shapes, 0, bytemuck::cast_slice(&shadow.shapes));
            shadow.bounds_stale = true;
        }
        self.vertex_version += 1;
        shadow.smooth_stale = true;
        Ok(())
    }

    /// The first vertex of `positions` that has moved more than [`SEAM_SLACK`] on some axis away from the vertex it was
    /// welded to at creation, or `None`. Such an update tears a seam the mesh's outline directions still treat as one
    /// point; the renderer refuses it in debug builds. A static mesh has no welds to check.
    pub(crate) fn split_seam(&self, positions: &[Vec3]) -> Option<usize> {
        let shadow = self.shadow.as_ref()?;
        shadow.welds.iter().zip(positions).position(|(&slot, &position)| {
            let anchor = shadow.anchors.get(slot).and_then(|&a| positions.get(a)).copied().unwrap_or(position);
            (anchor - position).abs().max_element() > SEAM_SLACK
        })
    }

    /// Brings what an update left behind up to date, as far as `needs` says a frame reads it: the outline directions if
    /// `needs.hull` (an outline will draw the mesh's own stream), the shadow bounds if `needs.bounds`. What is not
    /// needed stays stale, so a later frame that needs it brings it up to date first.
    ///
    /// True if the outline directions are now owed to a dispatch ([`GpuMesh::weld_target`]): they were welded here on
    /// the CPU otherwise.
    pub(crate) fn refresh(&mut self, queue: &wgpu::Queue, needs: Needs) -> bool {
        let Some(shadow) = self.shadow.as_mut() else { return false };
        let Shadow { shapes, skin, smooth, welds, weld_count, sums, gpu_weld, smooth_stale, bounds_stale, .. } = shadow;
        let mut dispatch = false;
        if needs.hull && *smooth_stale {
            if *gpu_weld {
                dispatch = true;
            } else {
                resmooth(welds, *weld_count, sums, shapes, smooth);
                queue.write_buffer(&self.smooth_normals, 0, bytemuck::cast_slice(smooth));
            }
            *smooth_stale = false;
        }
        if needs.bounds && *bounds_stale {
            self.bounds.refit(self.joints_needed, shapes, skin);
            *bounds_stale = false;
        }
        dispatch
    }

    /// Whether the outline directions and the bounds lag the vertices, for tests that count what a frame refreshed.
    #[cfg(test)]
    pub(crate) fn lags(&self) -> Option<(bool, bool)> {
        self.shadow.as_ref().map(|s| (s.smooth_stale, s.bounds_stale))
    }

    /// A dispatch that welds this mesh's outline directions never ran (its frame did not reach the queue): they lag
    /// again.
    pub(crate) fn directions_lost(&mut self) {
        if let Some(shadow) = self.shadow.as_mut() {
            shadow.smooth_stale = true;
        }
    }

    /// What the GPU weld of a dynamic mesh's outline directions needs, if the mesh is welded that way: the weld table
    /// (a vertex's point, per vertex) with its point count, and the two streams the dispatch reads and writes.
    pub(crate) fn weld_target(&self) -> Option<WeldTarget<'_>> {
        let shadow = self.shadow.as_ref().filter(|s| s.gpu_weld)?;
        Some(WeldTarget {
            welds: &shadow.welds,
            weld_count: shadow.weld_count,
            shapes: &self.shapes,
            directions: &self.smooth_normals,
        })
    }

    /// The extra stream called `name` with exactly `components` floats per vertex, if the mesh has one. A stream with
    /// another component count is treated as absent: the program asked for a different attribute.
    fn extra(&self, name: &str, components: u8) -> Option<&wgpu::Buffer> {
        self.extras.iter().find(|e| e.name.as_str() == name && e.components == components).map(|e| &e.buffer)
    }

    /// Bit `i` is set when the mesh supplies attribute `i` of `attributes`. It selects the pipeline: a missing
    /// attribute is a stride-0 stream, which is a different vertex layout.
    pub(crate) fn extra_mask(&self, attributes: &[AttributeSpec]) -> u8 {
        attributes
            .iter()
            .enumerate()
            .filter(|(_, a)| self.extra(a.name, a.components).is_some())
            .fold(0, |mask, (i, _)| mask | 1 << i)
    }

    /// Draws the mesh as instance number `instance` of the frame's instance buffer.
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, instance: u32, morphed: Option<wgpu::BufferSlice<'_>>) {
        self.bind_vertices(pass, morphed);
        self.draw_indexed(pass, instance);
    }

    /// Like [`draw`](Self::draw), with the program's attributes bound to vertex buffer slots 1 and up and `zeros` in
    /// the slots of attributes the mesh lacks (and of unused slots). Bind order matches [`extra_mask`](Self::extra_mask).
    pub(crate) fn draw_with_extras(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        instance: u32,
        morphed: Option<wgpu::BufferSlice<'_>>,
        attributes: &[AttributeSpec],
        zeros: &wgpu::Buffer,
    ) {
        self.bind_vertices(pass, morphed);
        for slot in 0..crate::program::MAX_ATTRIBUTES {
            let buffer = attributes.get(slot).and_then(|a| self.extra(a.name, a.components)).unwrap_or(zeros);
            pass.set_vertex_buffer(EXTRA_FIRST_SLOT + u32::try_from(slot).unwrap_or(0), buffer.slice(..));
        }
        self.draw_indexed(pass, instance);
    }

    /// Like [`draw`](Self::draw), with the smooth-normal stream bound to vertex buffer slot 1: the mesh's own, or
    /// `morphed`'s `(shapes, smooth normals)` for an instance that morphs.
    pub(crate) fn draw_with_smooth_normals(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        instance: u32,
        morphed: Option<(wgpu::BufferSlice<'_>, wgpu::BufferSlice<'_>)>,
    ) {
        self.bind_vertices(pass, morphed.map(|(shapes, _)| shapes));
        pass.set_vertex_buffer(FIRST_FREE_SLOT, morphed.map_or(self.smooth_normals.slice(..), |(_, smooth)| smooth));
        self.draw_indexed(pass, instance);
    }

    /// Binds the two vertex streams to slots 0 and 1, with `morphed` (an instance's blended shapes) in place of the
    /// mesh's own shape stream.
    fn bind_vertices(&self, pass: &mut wgpu::RenderPass<'_>, morphed: Option<wgpu::BufferSlice<'_>>) {
        pass.set_vertex_buffer(0, morphed.unwrap_or_else(|| self.shapes.slice(..)));
        pass.set_vertex_buffer(1, self.surface.slice(..));
    }

    fn draw_indexed(&self, pass: &mut wgpu::RenderPass<'_>, instance: u32) {
        pass.set_index_buffer(self.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..self.index_count, 0, instance..instance + 1);
    }
}

/// Two vertices closer than this on every axis, in metres, are the same point of the surface. A mesh with split seams
/// repeats a position exactly, but one that went through a unit conversion or a modeller's export can repeat it a few
/// units in the last place apart (1e-7 m is about 1 ulp near 1 m), and exact comparison would then open a gap in the
/// hull.
const WELD_TOLERANCE: f32 = 1e-6;

/// How far, in metres on any axis, a dynamic mesh's vertex may drift from the vertex it was welded to at creation
/// before an update counts as tearing the seam ([`GpuMesh::split_seam`]). Copies of one point deformed by the same
/// skin or simulation round a little differently, so this is a hundred weld tolerances: 0.1 mm.
pub(crate) const SEAM_SLACK: f32 = 100.0 * WELD_TOLERANCE;

/// The first vertex of each of `count` slots: slots are numbered in order of first appearance.
fn anchors_of(slots: &[usize], count: usize) -> Vec<usize> {
    let mut anchors = Vec::with_capacity(count);
    for (vertex, &slot) in slots.iter().enumerate() {
        if slot == anchors.len() {
            anchors.push(vertex);
        }
    }
    anchors
}

/// Each vertex's normal averaged with those of every vertex at the same point (see [`Welder`]), so a hard-edged
/// mesh (a cube has three normals per corner) keeps one direction per corner and an outline pushed along it stays
/// closed. A vertex whose neighbours cancel it out (two sheets back to back) keeps its own normal.
fn smooth_by_slots(slots: &[usize], count: usize, normals: &[Vec3]) -> Vec<Vec3> {
    let mut sums = Vec::with_capacity(count);
    sum_by_slot(slots, count, normals.iter().copied(), &mut sums);
    slots.iter().zip(normals).map(|(&slot, &own)| smoothed(&sums, slot, own)).collect()
}

/// [`smooth_by_slots`] over a dynamic mesh's current vertices into `out`, with `sums` as scratch: no allocation once
/// `sums` has grown to `count`.
pub(crate) fn resmooth(slots: &[usize], count: usize, sums: &mut Vec<Vec3>, shapes: &[Shape], out: &mut [[f32; 3]]) {
    sum_by_slot(slots, count, shapes.iter().map(|v| Vec3::from(v.normal)), sums);
    for ((&slot, shape), out) in slots.iter().zip(shapes).zip(out) {
        *out = smoothed(sums, slot, Vec3::from(shape.normal)).to_array();
    }
}

/// Adds every vertex's normal to the sum of its point.
fn sum_by_slot(slots: &[usize], count: usize, normals: impl Iterator<Item = Vec3>, sums: &mut Vec<Vec3>) {
    sums.clear();
    sums.resize(count, Vec3::ZERO);
    for (&slot, normal) in slots.iter().zip(normals) {
        if let Some(sum) = sums.get_mut(slot) {
            *sum += normal;
        }
    }
}

/// The unit direction of point `slot`, or `own` if the normals there cancel out.
fn smoothed(sums: &[Vec3], slot: usize, own: Vec3) -> Vec3 {
    sums.get(slot).and_then(|sum| sum.try_normalize()).unwrap_or(own)
}

/// The grid cell holding coordinate `v`, for cells [`WELD_CELL`] wide. A float too large for a cell number saturates:
/// such vertices share a cell and the distance test alone decides.
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)]
fn cell_of(v: f32) -> i64 {
    // `floor` without the library call (the baseline x86-64 target has no rounding instruction), once per cell number
    // and six cell numbers per vertex: truncate, then step down for a negative fraction.
    let scaled = v * (1.0 / WELD_CELL);
    // The cast saturates for a coordinate beyond the cell numbers, so the step down saturates too: below `i64::MIN`
    // cells the scaled value is still less than the truncated one.
    let truncated = scaled as i64;
    truncated.saturating_sub(i64::from(scaled < truncated as f32))
}

/// Cells sixteen tolerances wide. A point's neighbours within the tolerance lie in at most two cells per axis, and since
/// the box around it is a sixteenth of a cell wide, it straddles a border on an axis one time in eight: a lookup
/// usually touches one cell and at most eight. Narrower cells make every lookup touch eight.
const WELD_CELL: f32 = 16.0 * WELD_TOLERANCE;

/// Marks the end of a chain of slots in [`Welder::next`], and an empty entry of [`Welder::table`] is 0.
const NO_SLOT: usize = usize::MAX;

/// Welds vertices into slots: vertices within [`WELD_TOLERANCE`] of one another share a slot, and slots are numbered in
/// order of first appearance. A vertex joins the slot of the first vertex found within the tolerance (the nearest, if
/// several; the lower slot on a tie), so a chain of points each within tolerance of the next does not collapse into one.
///
/// The grid is a flat open-addressing table with a cheap hash, so welding a large mesh at creation stays quick, and the
/// buffers are sized once for the mesh, so welding again with the same welder allocates nothing.
struct Welder {
    /// Where each slot's first vertex is.
    firsts: Vec<Vec3>,
    /// The grid cell each slot's first vertex is in.
    cells: Vec<[i64; 3]>,
    /// For each slot, the next older slot in the same cell, or [`NO_SLOT`].
    next: Vec<usize>,
    /// Open addressing by the hash of a cell, linear probing: 0 is empty, otherwise the high half is the rest of the
    /// cell's hash (so that a probe past another cell's entry rarely has to look at [`cells`](Self::cells)) and the low
    /// half is one more than the newest slot in the cell ([`next`](Self::next) chains to the older ones). Always a
    /// power of two long and at most half full.
    table: Vec<u64>,
}

impl Welder {
    /// A welder for meshes of up to `vertices` vertices.
    fn with_capacity(vertices: usize) -> Self {
        Self {
            firsts: Vec::with_capacity(vertices),
            cells: Vec::with_capacity(vertices),
            next: Vec::with_capacity(vertices),
            table: vec![0; (2 * vertices).next_power_of_two().max(16)],
        }
    }

    /// The table entry for `cell`: where it is, or the empty entry where it would go.
    #[allow(clippy::cast_possible_truncation)]
    fn entry(&self, cell: [i64; 3]) -> usize {
        let mask = self.table.len() - 1;
        let hash = hash_cell(cell);
        let mut at = hash as usize & mask;
        while let Some(&held) = self.table.get(at) {
            // The low half is a slot number plus one, so it is never 0 in a held entry.
            if held == 0 || (held >> 32 == hash >> 32 && self.cells.get(slot_of(held)) == Some(&cell)) {
                break;
            }
            at = (at + 1) & mask;
        }
        at
    }

    /// The newest slot in `cell`, or [`NO_SLOT`].
    fn newest_in(&self, cell: [i64; 3]) -> usize {
        match self.table.get(self.entry(cell)) {
            Some(&held) if held != 0 => slot_of(held),
            _ => NO_SLOT,
        }
    }

    /// Doubles the table until it is at most half full with `slots` slots, and files every slot again (oldest first,
    /// so the newest ends up as the head of its cell). Only a mesh with more vertices than the welder was sized for
    /// gets here.
    fn grow(&mut self, slots: usize) {
        let mut len = self.table.len();
        while slots * 2 > len {
            len *= 2;
        }
        self.table.clear();
        self.table.resize(len, 0);
        for slot in 0..self.cells.len() {
            self.file(slot);
        }
    }

    /// Makes `slot`, whose cell is already in [`cells`](Self::cells), the newest of its cell.
    fn file(&mut self, slot: usize) {
        let Some(&cell) = self.cells.get(slot) else { return };
        let at = self.entry(cell);
        let head = self.table.get(at).copied().filter(|&held| held != 0).map_or(NO_SLOT, slot_of);
        if let Some(next) = self.next.get_mut(slot) {
            *next = head;
        }
        if let Some(entry) = self.table.get_mut(at) {
            // A mesh with 4 billion vertices cannot be indexed by the renderer anyway (indices are `u32`).
            *entry = hash_cell(cell) & !0xFFFF_FFFF | u64::from(u32::try_from(slot + 1).unwrap_or(u32::MAX));
        }
    }

    /// Fills `slots` with the slot of every position and returns how many slots there are.
    fn weld(&mut self, positions: impl Iterator<Item = Vec3>, slots: &mut Vec<usize>) -> usize {
        let tolerance = Vec3::splat(WELD_TOLERANCE);
        self.firsts.clear();
        self.cells.clear();
        self.next.clear();
        self.table.fill(0);
        slots.clear();
        for position in positions {
            let [lo, hi] = [position - tolerance, position + tolerance].map(|corner| corner.to_array().map(cell_of));
            let mut best: Option<(usize, f32)> = None;
            for x in lo[0]..=hi[0] {
                for y in lo[1]..=hi[1] {
                    for z in lo[2]..=hi[2] {
                        let mut slot = self.newest_in([x, y, z]);
                        while slot != NO_SLOT {
                            let offset = self.firsts[slot] - position;
                            let distance = offset.length_squared();
                            let closer =
                                best.is_none_or(|(held, d)| distance.total_cmp(&d).then(slot.cmp(&held)).is_lt());
                            if offset.abs().max_element() <= WELD_TOLERANCE && closer {
                                best = Some((slot, distance));
                            }
                            slot = self.next[slot];
                        }
                    }
                }
            }
            let slot = best.map_or_else(
                || {
                    let slot = self.firsts.len();
                    if (slot + 1) * 2 > self.table.len() {
                        self.grow(slot + 1);
                    }
                    self.firsts.push(position);
                    self.cells.push(position.to_array().map(cell_of));
                    self.next.push(NO_SLOT);
                    self.file(slot);
                    slot
                },
                |(slot, _)| slot,
            );
            slots.push(slot);
        }
        self.firsts.len()
    }
}

/// The slot a [`Welder::table`] entry stands for: its low half less one.
#[allow(clippy::cast_possible_truncation)]
fn slot_of(entry: u64) -> usize {
    entry as u32 as usize - 1
}

/// A cheap, well-spread hash of a grid cell, the same on every platform (the table's layout never shows in the output,
/// but a deterministic hash keeps runs reproducible).
#[allow(clippy::cast_sign_loss)]
fn hash_cell([x, y, z]: [i64; 3]) -> u64 {
    let h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
        ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F)
        ^ (z as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
    (h ^ (h >> 29)).wrapping_mul(0xBF58_476D_1CE4_E5B9)
}

/// One past the highest joint any vertex gives weight to. Joints with zero weight are ignored: exporters often leave
/// index 0 in unused slots, and the shader skips them too.
fn joints_needed(mesh: &MeshData) -> u32 {
    mesh.joints
        .iter()
        .zip(&mesh.weights)
        .flat_map(|(joints, weights)| joints.iter().zip(weights))
        .filter(|&(_, &weight)| weight != 0.0)
        .map(|(&joint, _)| u32::from(joint) + 1)
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weld_slots(positions: &[Vec3]) -> Vec<usize> {
        let mut slots = Vec::new();
        Welder::with_capacity(positions.len()).weld(positions.iter().copied(), &mut slots);
        slots
    }

    fn smooth_normals(mesh: &MeshData) -> Vec<Vec3> {
        let slots = weld_slots(&mesh.positions);
        smooth_by_slots(&slots, slots.iter().max().map_or(0, |last| last + 1), &mesh.normals)
    }

    #[test]
    fn the_vertex_is_seventy_two_bytes_in_two_streams_with_no_padding() {
        let [shape, surface] = vertex_layouts().map(|l| l.map_or(0, |l| l.array_stride));
        assert_eq!((shape, surface), (24, 48));
        assert_eq!(size_of::<Shape>() as u64, shape);
        assert_eq!(size_of::<Surface>() as u64, surface);
    }

    #[test]
    fn the_smooth_normal_layout_is_three_floats_at_location_six() {
        let layout = smooth_normal_layout();
        assert_eq!(layout.array_stride, 12);
        assert_eq!(layout.attributes.len(), 1);
        assert_eq!((layout.attributes[0].shader_location, layout.attributes[0].offset), (6, 0));
    }

    #[test]
    fn smooth_normals_weld_a_hard_edged_corner_into_one_direction() {
        let cube = omuzaki_render_mock::shapes::cuboid(Vec3::splat(0.5));
        let smooth = smooth_normals(&cube);
        assert_eq!(smooth.len(), cube.positions.len());
        for (position, normal) in cube.positions.iter().zip(&smooth) {
            let corner = position.signum() / 3.0_f32.sqrt();
            assert!((*normal - corner).length() < 1e-6, "corner {position}: {normal}");
        }
    }

    #[test]
    fn smooth_normals_keep_a_smooth_meshs_own_normals_and_never_weld_apart_positions() {
        let mut quad = omuzaki_render_mock::shapes::quad(Vec2::ONE);
        quad.normals = vec![Vec3::Z, Vec3::X, Vec3::Y, Vec3::NEG_Z];
        assert_eq!(smooth_normals(&quad), quad.normals, "four distinct positions: nothing to average");
    }

    #[test]
    fn smooth_normals_weld_negative_zero_with_zero_and_survive_cancelling_normals() {
        let mut sheets = MeshData {
            positions: vec![Vec3::ZERO, Vec3::new(-0.0, -0.0, -0.0), Vec3::X],
            normals: vec![Vec3::Z, Vec3::NEG_Z, Vec3::Y],
            ..MeshData::default()
        };
        assert_eq!(smooth_normals(&sheets), sheets.normals, "back to back sheets cancel, so each keeps its own");
        sheets.normals[1] = Vec3::Z;
        assert_eq!(smooth_normals(&sheets)[..2], [Vec3::Z, Vec3::Z], "-0.0 welds with 0.0");
    }

    /// Moves every coordinate by 1e-7 m in a pattern that differs between the copies of a corner.
    fn jittered(mut mesh: MeshData) -> MeshData {
        for (i, position) in mesh.positions.iter_mut().enumerate() {
            let sign = |bit: usize| if (i >> bit) & 1 == 0 { 1e-7 } else { -1e-7 };
            *position += Vec3::new(sign(0), sign(1), sign(2));
        }
        mesh
    }

    #[test]
    fn jittered_copies_of_a_corner_still_weld() {
        let cube = omuzaki_render_mock::shapes::cuboid(Vec3::splat(0.5));
        let welded = smooth_normals(&cube);
        let jitter = jittered(cube);
        assert!(jitter.positions.windows(2).any(|w| w[0] != w[1]), "the copies of a corner must now differ");
        for (jittered, exact) in smooth_normals(&jitter).iter().zip(&welded) {
            assert!((*jittered - *exact).length() < 1e-6, "{jittered} instead of {exact}");
        }
    }

    #[test]
    fn welding_has_a_tolerance_in_each_axis_and_works_across_cell_borders() {
        // The border between two cells is at a multiple of the cell width. Two points 0.8e-6 apart straddle it and weld; one
        // 1.5e-5 along does not, and neither does one 1.5e-6 away on a single axis.
        let border = 4.0 * WELD_CELL;
        let mesh = MeshData {
            positions: vec![
                Vec3::new(border - 0.4e-6, 0.0, 0.0),
                Vec3::new(border + 0.4e-6, 0.0, 0.0),
                Vec3::new(border + 1.5e-5, 0.0, 0.0),
                Vec3::new(border, 1.5e-6, 0.0),
            ],
            normals: vec![Vec3::X, Vec3::Y, Vec3::Z, Vec3::NEG_Z],
            ..MeshData::default()
        };
        let slots = weld_slots(&mesh.positions);
        assert_eq!(slots, [0, 0, 1, 2]);
        let smooth = smooth_normals(&mesh);
        assert!((smooth[0] - Vec3::new(1.0, 1.0, 0.0).normalize()).length() < 1e-6);
        assert_eq!(smooth[2..], mesh.normals[2..], "far vertices keep their own normals");
    }

    /// The definition of [`Welder::weld`], by comparing every vertex with every earlier slot.
    fn weld_by_brute_force(positions: &[Vec3]) -> Vec<usize> {
        let mut firsts: Vec<Vec3> = Vec::new();
        let mut slots = Vec::new();
        for &position in positions {
            let mut best: Option<(usize, f32)> = None;
            for (slot, first) in firsts.iter().enumerate() {
                let offset = *first - position;
                let distance = offset.length_squared();
                if offset.abs().max_element() <= WELD_TOLERANCE && best.is_none_or(|(_, d)| distance < d) {
                    best = Some((slot, distance));
                }
            }
            slots.push(best.map_or_else(
                || {
                    firsts.push(position);
                    firsts.len() - 1
                },
                |(slot, _)| slot,
            ));
        }
        slots
    }

    #[test]
    fn welding_agrees_with_comparing_every_pair_including_a_welder_that_must_grow() {
        // Points on a lattice a few tolerances apart, each nudged by up to a tolerance either way, so that many are
        // within the tolerance of others, some only just, and many sit near cell borders. A tiny linear congruential
        // generator keeps the points the same on every run.
        let mut state = 0x2545_F491_u32;
        let mut unit = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            f32::from((state >> 16) as u16) / 65_535.0
        };
        let positions: Vec<Vec3> = (0..1500)
            .map(|_| {
                let lattice = |u: f32| (u * 6.0).floor() * 1.5e-6 + 8.0 * WELD_CELL;
                let nudge = |u: f32| (u - 0.5) * 2.0 * WELD_TOLERANCE;
                Vec3::new(
                    lattice(unit()) + nudge(unit()),
                    lattice(unit()) + nudge(unit()),
                    lattice(unit()) + nudge(unit()),
                )
            })
            .collect();
        let expected = weld_by_brute_force(&positions);
        assert!(expected.iter().max().is_some_and(|&last| last > 20 && last < positions.len() - 20), "a mixed case");
        let mut slots = Vec::new();
        for capacity in [positions.len(), 3] {
            let count = Welder::with_capacity(capacity).weld(positions.iter().copied(), &mut slots);
            assert_eq!(slots, expected, "capacity {capacity}");
            assert_eq!(count, expected.iter().max().map_or(0, |last| last + 1));
        }
    }

    #[test]
    fn a_chain_of_close_points_does_not_collapse_into_one_vertex() {
        // Each point is within the tolerance of the next, but the third is 1.6e-6 from the first.
        let positions: Vec<Vec3> = [0.0, 0.8e-6, 1.6e-6].map(|x| Vec3::new(x, 0.0, 0.0)).to_vec();
        assert_eq!(weld_slots(&positions), [0, 0, 1]);
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)]
    fn welding_again_with_the_same_welder_starts_from_scratch_and_keeps_its_buffers() {
        let cube = omuzaki_render_mock::shapes::cuboid(Vec3::splat(0.5));
        let mut welder = Welder::with_capacity(cube.positions.len());
        let mut slots = Vec::with_capacity(cube.positions.len());
        let (cap_slots, cap_firsts) = (slots.capacity(), welder.firsts.capacity());
        assert_eq!(welder.weld(cube.positions.iter().copied(), &mut slots), 8);
        let apart: Vec<Vec3> = (0..cube.positions.len() as u8).map(|i| Vec3::splat(f32::from(i))).collect();
        assert_eq!(welder.weld(apart.iter().copied(), &mut slots), apart.len());
        assert_eq!(welder.weld(cube.positions.iter().copied(), &mut slots), 8);
        assert_eq!(slots, weld_slots(&cube.positions));
        assert_eq!((slots.capacity(), welder.firsts.capacity()), (cap_slots, cap_firsts), "no growth");
    }

    #[test]
    fn cell_numbers_saturate_for_coordinates_beyond_them_instead_of_overflowing() {
        assert_eq!(cell_of(-1e20), i64::MIN, "the -1e20 of the review");
        assert_eq!(cell_of(f32::MIN), i64::MIN);
        assert_eq!(cell_of(f32::MAX), i64::MAX);
        // At the boundary: -2^63 cells scales back to exactly `i64::MIN` (no fraction to step down for), and the next
        // float below it is past the boundary and saturates.
        let boundary = -(2.0_f32.powi(63)) * WELD_CELL;
        assert_eq!(cell_of(boundary), i64::MIN);
        assert_eq!(cell_of(boundary.next_down()), i64::MIN);
        assert_eq!(cell_of(-0.5 * WELD_CELL), -1, "an ordinary negative fraction still steps down");
        assert_eq!(cell_of(-WELD_CELL), -1, "and a whole number does not");
    }

    #[test]
    fn welding_vertices_at_huge_negative_and_positive_coordinates_does_not_panic() {
        let positions = [Vec3::splat(-1e20), Vec3::splat(-1e20), Vec3::splat(f32::MIN), Vec3::splat(f32::MAX)];
        assert_eq!(weld_slots(&positions), [0, 0, 1, 2]);
    }

    #[test]
    fn anchors_are_the_first_vertex_of_each_slot() {
        assert_eq!(anchors_of(&[0, 1, 0, 2, 1, 2], 3), [0, 1, 3]);
        assert_eq!(anchors_of(&[], 0), Vec::<usize>::new());
    }

    #[test]
    fn joints_needed_ignores_zero_weights() {
        let mut quad = omuzaki_render_mock::shapes::quad(Vec2::ONE);
        quad.joints = vec![[0, 5, 0, 0], [0, 0, 0, 0], [3, 0, 0, 0], [0, 0, 0, 0]];
        quad.weights = vec![[1.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0], [0.5, 0.5, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]];
        assert_eq!(joints_needed(&quad), 4, "joint 5 has weight 0, joint 3 is the highest used");
        assert_eq!(joints_needed(&MeshData::default()), 0, "a rigid mesh needs none");
    }
}
