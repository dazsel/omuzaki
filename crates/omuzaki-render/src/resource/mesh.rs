//! Mesh upload: one interleaved vertex buffer with a fixed layout, and a `u32` index buffer.
//!
//! A single fixed layout (with neutral defaults for what a mesh lacks) lets every pass share one vertex input and one
//! set of pipelines. The spike proved this for the whole character, hair included.
//!
//! Beside the vertex buffer sits a second, smaller one holding each vertex's *smooth normal*: the normals of every
//! vertex at the same position, averaged. The outline pass pushes vertices along it, because the shading normals of a
//! hard-edged mesh differ at a corner and would tear the hull apart there.

use std::collections::HashMap;

use bytemuck::{Pod, Zeroable};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_render_api::{MeshData, RenderError};
use wgpu::util::DeviceExt;

use crate::gpu::Gpu;
use crate::program::AttributeSpec;
use crate::shadow::bounds::MeshBounds;

/// One vertex as the shaders read it (`Vertex` in `shader/common.wgsl`). 72 bytes, no padding.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    colour: [f32; 4],
    joints: [u16; 4],
    weights: [f32; 4],
}

const VERTEX_ATTRIBUTES: [wgpu::VertexAttribute; 6] = wgpu::vertex_attr_array![
    0 => Float32x3, 1 => Float32x3, 2 => Float32x2, 3 => Float32x4, 4 => Uint16x4, 5 => Float32x4
];

/// The vertex buffer layout every scene pipeline uses.
pub(crate) fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: size_of::<Vertex>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &VERTEX_ATTRIBUTES,
    }
}

const SMOOTH_NORMAL_ATTRIBUTES: [wgpu::VertexAttribute; 1] = wgpu::vertex_attr_array![6 => Float32x3];

/// The layout of the smooth-normal stream (vertex buffer slot 1, shader location 6). Only the outline pass binds it.
pub(crate) fn smooth_normal_layout() -> wgpu::VertexBufferLayout<'static> {
    wgpu::VertexBufferLayout {
        array_stride: size_of::<[f32; 3]>() as wgpu::BufferAddress,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &SMOOTH_NORMAL_ATTRIBUTES,
    }
}

/// Where the extra streams start: vertex buffer slot 1 and shader location 6, then upward (`Extras` in
/// `shader/toon.wgsl`).
const EXTRA_FIRST_SLOT: u32 = 1;
const EXTRA_FIRST_LOCATION: u32 = 6;

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

/// A mesh on the GPU.
pub(crate) struct GpuMesh {
    vertices: wgpu::Buffer,
    /// One unit vector per vertex, in the order of `vertices` ([`smooth_normals`]).
    smooth_normals: wgpu::Buffer,
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
}

impl GpuMesh {
    /// Uploads `mesh`, which the caller has validated.
    pub(crate) fn upload(gpu: &Gpu, mesh: &MeshData) -> Result<Self, RenderError> {
        let index_count = u32::try_from(mesh.indices.len())
            .map_err(|_| RenderError::LimitExceeded { what: "indices in a mesh", limit: u64::from(u32::MAX) })?;
        let skinned = mesh.is_skinned();
        let joints_needed = joints_needed(mesh);
        let vertices: Vec<Vertex> = (0..mesh.positions.len())
            .map(|i| Vertex {
                position: mesh.positions[i].to_array(),
                normal: mesh.normals[i].to_array(),
                uv: mesh.uvs.get(i).map_or([0.0; 2], Vec2::to_array),
                colour: mesh.colours.get(i).copied().unwrap_or([1.0; 4]),
                joints: mesh.joints.get(i).copied().unwrap_or([0; 4]),
                weights: mesh.weights.get(i).copied().unwrap_or([1.0, 0.0, 0.0, 0.0]),
            })
            .collect();
        let smooth: Vec<[f32; 3]> = smooth_normals(mesh).iter().map(Vec3::to_array).collect();
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
        Ok(Self {
            vertices: buffer(bytemuck::cast_slice(&vertices), wgpu::BufferUsages::VERTEX),
            smooth_normals: buffer(bytemuck::cast_slice(&smooth), wgpu::BufferUsages::VERTEX),
            indices: buffer(bytemuck::cast_slice(&mesh.indices), wgpu::BufferUsages::INDEX),
            extras,
            index_count,
            skinned,
            joints_needed,
            bounds: MeshBounds::of(mesh, joints_needed),
            triangles: mesh.triangle_count() as u64,
            bytes: (vertices.len() * (size_of::<Vertex>() + size_of::<[f32; 3]>())
                + mesh.indices.len() * size_of::<u32>()
                + extra_bytes) as u64,
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
    pub(crate) fn draw(&self, pass: &mut wgpu::RenderPass<'_>, instance: u32) {
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        self.draw_indexed(pass, instance);
    }

    /// Like [`draw`](Self::draw), with the program's attributes bound to vertex buffer slots 1 and up and `zeros` in
    /// the slots of attributes the mesh lacks (and of unused slots). Bind order matches [`extra_mask`](Self::extra_mask).
    pub(crate) fn draw_with_extras(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        instance: u32,
        attributes: &[AttributeSpec],
        zeros: &wgpu::Buffer,
    ) {
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        for slot in 0..crate::program::MAX_ATTRIBUTES {
            let buffer = attributes.get(slot).and_then(|a| self.extra(a.name, a.components)).unwrap_or(zeros);
            pass.set_vertex_buffer(EXTRA_FIRST_SLOT + u32::try_from(slot).unwrap_or(0), buffer.slice(..));
        }
        self.draw_indexed(pass, instance);
    }

    /// Like [`draw`](Self::draw), with the smooth-normal stream bound to vertex buffer slot 1.
    pub(crate) fn draw_with_smooth_normals(&self, pass: &mut wgpu::RenderPass<'_>, instance: u32) {
        pass.set_vertex_buffer(0, self.vertices.slice(..));
        pass.set_vertex_buffer(1, self.smooth_normals.slice(..));
        self.draw_indexed(pass, instance);
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

/// Each vertex's normal averaged with those of every vertex at the same point (within [`WELD_TOLERANCE`]), so a
/// hard-edged mesh (a cube has three normals per corner) keeps one direction per corner and an outline pushed along it
/// stays closed. A vertex whose neighbours cancel it out (two sheets back to back) keeps its own normal.
fn smooth_normals(mesh: &MeshData) -> Vec<Vec3> {
    let slots = weld_slots(&mesh.positions);
    let mut sums = vec![Vec3::ZERO; slots.iter().max().map_or(0, |last| last + 1)];
    for (&slot, &normal) in slots.iter().zip(&mesh.normals) {
        sums[slot] += normal;
    }
    slots.iter().zip(&mesh.normals).map(|(&slot, &own)| sums[slot].try_normalize().unwrap_or(own)).collect()
}

/// The grid cell holding coordinate `v`, for cells [`WELD_CELL`] wide. A float too large for a cell number saturates:
/// such vertices share a cell and the distance test alone decides.
#[allow(clippy::cast_possible_truncation)]
fn cell_of(v: f32) -> i64 {
    (v / WELD_CELL).floor() as i64
}

/// Cells twice the tolerance wide, so the vertices within the tolerance of a point lie in at most two cells per axis
/// (at most eight cells in all, usually one).
const WELD_CELL: f32 = 2.0 * WELD_TOLERANCE;

/// The slot of every vertex: vertices within [`WELD_TOLERANCE`] of one another share a slot, and slots are numbered in
/// order of first appearance. A vertex joins the slot of the first vertex found within the tolerance (the nearest, if
/// several), so a chain of points each within tolerance of the next does not collapse into one.
fn weld_slots(positions: &[Vec3]) -> Vec<usize> {
    let tolerance = Vec3::splat(WELD_TOLERANCE);
    // Where each slot's first vertex is, and which slots have their first vertex in each cell.
    let mut firsts: Vec<Vec3> = Vec::new();
    let mut grid: HashMap<[i64; 3], Vec<usize>> = HashMap::with_capacity(positions.len());
    positions
        .iter()
        .map(|&position| {
            let [lo, hi] = [position - tolerance, position + tolerance].map(|corner| corner.to_array().map(cell_of));
            let cells = (lo[0]..=hi[0])
                .flat_map(|x| (lo[1]..=hi[1]).flat_map(move |y| (lo[2]..=hi[2]).map(move |z| [x, y, z])));
            let nearest = cells
                .filter_map(|cell| grid.get(&cell))
                .flatten()
                .map(|&slot| (slot, firsts[slot] - position))
                .filter(|(_, offset)| offset.abs().max_element() <= WELD_TOLERANCE)
                .min_by(|(_, a), (_, b)| a.length_squared().total_cmp(&b.length_squared()))
                .map(|(slot, _)| slot);
            nearest.unwrap_or_else(|| {
                firsts.push(position);
                grid.entry(position.to_array().map(cell_of)).or_default().push(firsts.len() - 1);
                firsts.len() - 1
            })
        })
        .collect()
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

    #[test]
    fn the_vertex_is_seventy_two_bytes_with_no_padding() {
        assert_eq!(size_of::<Vertex>(), 72);
        assert_eq!(vertex_layout().array_stride, 72);
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
        // The border between two cells is at a multiple of 2e-6. Two points 0.8e-6 apart straddle it and weld; one
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

    #[test]
    fn a_chain_of_close_points_does_not_collapse_into_one_vertex() {
        // Each point is within the tolerance of the next, but the third is 1.6e-6 from the first.
        let positions: Vec<Vec3> = [0.0, 0.8e-6, 1.6e-6].map(|x| Vec3::new(x, 0.0, 0.0)).to_vec();
        assert_eq!(weld_slots(&positions), [0, 0, 1]);
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
