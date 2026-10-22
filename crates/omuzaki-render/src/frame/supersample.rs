//! Supersampling beyond the hardware's sample count (`QualityMode::Offline`).
//!
//! The offline preset asks for 16 samples per pixel; a GPU draws with at most 4 or 8. The rest comes from drawing the
//! geometry passes (`Opaque`, `Outlines`, `Transparent`) again, `rounds` times, each with the projection shifted by a
//! fixed sub-pixel offset, and averaging the rounds' resolved colour into one more half-float texture that the post
//! passes and `Resolve` then read. `PaintTextures` and `Shadows` run once: neither depends on where the pixels are.
//!
//! The offsets come from a fixed table ([`jitter`]), indexed by nothing but the round, so a frame stays a function of
//! the resources, the scene and the quality: `FrameInfo::index` never moves a sample, and a clip cut anywhere renders
//! the same pictures. The table spreads each hardware sample over a cell of `rounds` positions so the GPU's standard
//! pattern (2x, 4x and 8x are rook patterns on a 16-grid) becomes a rook pattern of `hardware × rounds` samples: every
//! sample in its own column and row, which is what gives a thin line its full set of coverage levels.

use omuzaki_core::glam::Vec2;
use omuzaki_render_api::{Pass, Quality, QualityMode};

use crate::gpu::SCENE_FORMAT;
use crate::shared::Shared;

/// The most rounds a frame draws: the most samples a quality asks for (16) on a GPU with no multisampling.
pub(crate) const MAX_ROUNDS: u32 = 16;

/// How many times the geometry passes run for `quality` on a GPU drawing `hardware` samples: enough rounds to reach the
/// samples asked for in `Offline` mode, and one otherwise (live frames take what the hardware gives).
pub(crate) fn rounds(quality: &Quality, hardware: u32) -> u32 {
    let wanted = u32::from(quality.samples);
    if quality.mode != QualityMode::Offline || wanted <= hardware {
        return 1;
    }
    wanted.div_ceil(hardware.max(1)).min(MAX_ROUNDS)
}

/// Whether `pass` runs once per round. The rest run once per frame.
pub(crate) fn repeats(pass: Pass) -> bool {
    matches!(pass, Pass::Opaque | Pass::Outlines | Pass::Transparent)
}

/// The row each round's sample takes in its cell, by the number of rounds: rook patterns that are also queen patterns
/// from 4 up (no two samples share a column, a row or a diagonal), so the samples spread over the cell rather than along
/// a line.
fn rows(rounds: u32) -> &'static [u32] {
    match rounds {
        2 => &[0, 1],
        4 => &[1, 3, 0, 2],
        8 => &[0, 4, 7, 5, 2, 6, 1, 3],
        16 => &[0, 2, 4, 1, 12, 8, 13, 11, 14, 5, 15, 6, 3, 10, 7, 9],
        _ => &[0],
    }
}

/// The offset of `round` (of `rounds`) in pixels, x to the right and y down, for a GPU drawing `hardware` samples.
///
/// The hardware's samples sit `1 / hardware` of a pixel apart in each axis; each round moves all of them by a step of
/// `1 / (hardware × rounds)`, centred on zero, along x in round order and along y in the order of [`rows`]. One round
/// is no offset at all, so a frame that does not supersample is drawn exactly as before.
pub(crate) fn jitter(round: u32, rounds: u32, hardware: u32) -> Vec2 {
    if rounds <= 1 {
        return Vec2::ZERO;
    }
    let row = rows(rounds).get(round as usize).copied().unwrap_or(0);
    // Counts of at most 16: exact in f32.
    #[allow(clippy::cast_precision_loss)]
    let step = |k: u32| (k as f32 - (rounds - 1) as f32 / 2.0) / (hardware.max(1) * rounds) as f32;
    Vec2::new(step(round), step(row))
}

/// The pipeline that adds one round's resolved colour, weighted, into the average (`shader/accumulate.wgsl`). The
/// weight is the blend constant, so one pipeline serves every round count.
pub(crate) struct Accumulate {
    pipeline: wgpu::RenderPipeline,
}

impl Accumulate {
    pub(crate) fn new(shared: &Shared) -> Self {
        let device = &shared.gpu.device;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("accumulate"),
            source: wgpu::ShaderSource::Wgsl(include_str!("../shader/accumulate.wgsl").into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("accumulate"),
            bind_group_layouts: &[Some(&shared.layouts.colour_input)],
            immediate_size: 0,
        });
        // Destination plus the round times the blend constant (1 / rounds): the average, built up a round at a time.
        let add = wgpu::BlendComponent {
            src_factor: wgpu::BlendFactor::Constant,
            dst_factor: wgpu::BlendFactor::One,
            operation: wgpu::BlendOperation::Add,
        };
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("accumulate"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: SCENE_FORMAT,
                    blend: Some(wgpu::BlendState { color: add, alpha: add }),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline }
    }

    /// Adds `round` (resolved, bound as `input`) into `sum`, weighted by `1 / rounds`; the first round clears it.
    pub(crate) fn record(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        input: &wgpu::BindGroup,
        sum: &wgpu::TextureView,
        round: u32,
        rounds: u32,
    ) {
        let load = if round == 0 { wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT) } else { wgpu::LoadOp::Load };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("accumulate"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: sum,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        let weight = 1.0 / f64::from(rounds.max(1));
        pass.set_pipeline(&self.pipeline);
        pass.set_blend_constant(wgpu::Color { r: weight, g: weight, b: weight, a: weight });
        pass.set_bind_group(0, input, &[]);
        pass.draw(0..3, 0..1);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    /// The standard sample positions (Vulkan, D3D and Metal agree), in sixteenths of a pixel from the centre.
    fn standard(hardware: u32) -> &'static [(i32, i32)] {
        match hardware {
            1 => &[(0, 0)],
            2 => &[(4, 4), (-4, -4)],
            4 => &[(-2, -6), (6, -2), (-6, 2), (2, 6)],
            8 => &[(1, -3), (-1, 3), (5, 1), (-3, -5), (-5, 5), (-7, -1), (3, 7), (7, -7)],
            _ => unreachable!(),
        }
    }

    #[test]
    fn rounds_reach_the_samples_asked_for_offline_only() {
        let offline = Quality::offline();
        assert_eq!(offline.samples, 16);
        assert_eq!([1, 2, 4, 8].map(|h| rounds(&offline, h)), [16, 8, 4, 2]);
        assert_eq!(rounds(&Quality { samples: 4, ..offline }, 4), 1, "the hardware has enough");
        assert_eq!(rounds(&Quality { samples: 8, ..offline }, 4), 2);
        assert_eq!(rounds(&Quality { samples: 16, ..Quality::high() }, 4), 1, "live frames never repeat");
    }

    #[test]
    fn one_round_is_no_offset() {
        assert_eq!(jitter(0, 1, 4), Vec2::ZERO);
    }

    #[test]
    fn every_hardware_count_becomes_sixteen_samples_each_in_its_own_column_and_row() {
        let offline = Quality::offline();
        for hardware in [1, 2, 4, 8] {
            let n = rounds(&offline, hardware);
            let mut columns = HashSet::new();
            let mut rows = HashSet::new();
            for round in 0..n {
                let offset = jitter(round, n, hardware) * 16.0;
                for &(x, y) in standard(hardware) {
                    #[allow(clippy::cast_precision_loss)]
                    let (sx, sy) = (x as f32 + offset.x, y as f32 + offset.y);
                    assert!(sx.abs() < 8.0 && sy.abs() < 8.0, "{hardware}x round {round}: ({sx}, {sy}) left the pixel");
                    // Halves of a sixteenth: twice the position is a whole number.
                    #[allow(clippy::cast_possible_truncation)]
                    let (cx, cy) = ((sx * 2.0).round() as i32, (sy * 2.0).round() as i32);
                    columns.insert(cx);
                    rows.insert(cy);
                }
            }
            assert_eq!((columns.len(), rows.len()), (16, 16), "{hardware}x: a rook pattern of 16");
        }
    }

    #[test]
    fn the_offsets_are_centred_and_fixed() {
        for rounds in [2, 4, 8, 16] {
            let sum: Vec2 = (0..rounds).map(|r| jitter(r, rounds, 16 / rounds)).sum();
            assert!(sum.length() < 1e-6, "{rounds} rounds: the mean offset is zero, so nothing shifts on average");
            assert_eq!(jitter(1, rounds, 16 / rounds), jitter(1, rounds, 16 / rounds));
        }
    }

    #[test]
    fn the_shader_reads_the_input_it_is_bound_to() {
        let source = include_str!("../shader/accumulate.wgsl");
        assert!(source.contains("@group(0) @binding(0) var colour: texture_2d<f32>;"));
    }
}
