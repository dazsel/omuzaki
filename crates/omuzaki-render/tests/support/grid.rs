//! A rippling sheet for the dynamic-mesh and morph tests and benches: a grid of vertices in the XY plane whose height
//! is a travelling wave, so every vertex moves every frame and the normals tilt with the surface.

// Helpers shared by tests and examples; each binary uses a different part.
#![allow(dead_code, clippy::cast_precision_loss)]

use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_render_api::MeshData;

/// How tall the wave is, metres.
const AMPLITUDE: f32 = 0.12;
/// Waves across the sheet.
const WAVES: f32 = 3.0;

/// A `cols` x `rows` sheet, 2 m wide and 1 m tall, flat (the wave at time 0 is applied by [`wave_into`]).
pub fn grid(cols: u32, rows: u32) -> MeshData {
    let mut mesh = MeshData::default();
    for y in 0..rows {
        for x in 0..cols {
            let (u, v) = (x as f32 / (cols - 1) as f32, y as f32 / (rows - 1) as f32);
            mesh.positions.push(Vec3::new(u * 2.0 - 1.0, v - 0.5, 0.0));
            mesh.normals.push(Vec3::Z);
            mesh.uvs.push(Vec2::new(u, 1.0 - v));
        }
    }
    for y in 0..rows - 1 {
        for x in 0..cols - 1 {
            let i = y * cols + x;
            mesh.indices.extend([i, i + 1, i + cols + 1, i, i + cols + 1, i + cols]);
        }
    }
    mesh
}

/// Writes the sheet at time `t` into `positions` and `normals` (the vertices of [`grid`], flat), without allocating.
pub fn wave_into(flat: &[Vec3], t: f32, positions: &mut [Vec3], normals: &mut [Vec3]) {
    for ((p, n), flat) in positions.iter_mut().zip(normals).zip(flat) {
        let phase = flat.x * WAVES * std::f32::consts::PI + t;
        let slope = AMPLITUDE * WAVES * std::f32::consts::PI * phase.cos();
        *p = Vec3::new(flat.x, flat.y, AMPLITUDE * phase.sin());
        *n = Vec3::new(-slope, 0.0, 1.0).normalize();
    }
}

/// The sheet at time `t` as a fresh mesh, for comparing against one that was updated to the same shape.
pub fn waved(cols: u32, rows: u32, t: f32) -> MeshData {
    let mut mesh = grid(cols, rows);
    let flat = mesh.positions.clone();
    wave_into(&flat, t, &mut mesh.positions, &mut mesh.normals);
    mesh
}
