//! The CPU fallback of the GPU face rasteriser (CB-24, #1433 A6): a `VelloRasterizer` registered on a `FaceSystem` declines
//! a target that is not the renderer's wgpu tuple (the mock renderer's `MockNative`) and a device Vello can't run on, so the
//! layer is written by the CPU raster, byte for byte as without a GPU rasteriser. The tests that need a device print why
//! and pass without an adapter.

// Test code: a panic is a clear failure.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::any::Any;

use omuzaki_anim_api::ChannelBuf;
use omuzaki_char_api::face_texture_desc;
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::SkiaRasterizer;
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{FaceSystem, GpuRasterizer, RealPainter};
use omuzaki_face_api::{DEFAULT_PX_PER_M, DrawList, FaceLayerId, FaceView};
use omuzaki_face_mock::{mock_profile, mock_projection};
use omuzaki_render_api::{PaintTarget, Painter, PixelRect, RenderError, TextureDesc};
use omuzaki_render_mock::MockNative;
use omuzaki_test_face::probes::{overlapping_fills, spec};

/// A paint target like the mock renderer's: `native()` is a `MockNative`, and `write_rgba` keeps the pixels.
struct MockTarget {
    desc: TextureDesc,
    written: Option<Vec<u8>>,
    native: MockNative,
}

impl PaintTarget for MockTarget {
    fn desc(&self) -> &TextureDesc {
        &self.desc
    }

    fn write_rgba(&mut self, rect: PixelRect, rgba: &[u8]) -> Result<(), RenderError> {
        assert_eq!(rect, PixelRect::full(self.desc.size));
        self.written = Some(rgba.to_vec());
        Ok(())
    }

    fn native(&mut self) -> &mut dyn Any {
        &mut self.native
    }
}

/// The Base layer of a face system painted into a mock target, with a `VelloRasterizer` registered or not.
fn base_layer_pixels(with_vello: bool) -> Vec<u8> {
    let mut system =
        FaceSystem::real(RealPainter::new(mock_projection(), DEFAULT_PX_PER_M), Box::new(SkiaRasterizer::new()));
    if with_vello {
        system.set_gpu_rasterizer(Some(Box::new(VelloRasterizer::new())));
    }
    system.update(&ChannelBuf::NEUTRAL, &mock_profile(), Seconds(0.0), FaceView::default()).unwrap();
    let layer = system.layers().iter().find(|l| l.id == FaceLayerId::Base).copied().unwrap();
    let mut target = MockTarget { desc: face_texture_desc(&layer), written: None, native: MockNative };
    system.layer_painter(FaceLayerId::Base).unwrap().paint(&mut target).unwrap();
    target.written.expect("the CPU raster was written")
}

#[test]
fn the_mock_renderers_target_gets_the_cpu_raster_byte_for_byte() {
    let without = base_layer_pixels(false);
    assert!(
        without.as_chunks::<4>().0.iter().any(|p| p[3] > 0),
        "the face drew nothing, so the comparison proves nothing"
    );
    assert_eq!(base_layer_pixels(true), without);
}

#[test]
fn a_native_handle_that_is_not_the_tuple_is_declined() {
    let mut list = DrawList::new();
    overlapping_fills(&mut list);
    let mut rasterizer = VelloRasterizer::new();
    assert!(!rasterizer.rasterize_native(&spec(), &list, &mut MockNative).unwrap());
    assert!(!rasterizer.rasterize_native(&spec(), &list, &mut 7_u32).unwrap());
}

#[test]
fn a_device_vello_cannot_run_on_is_declined_every_time_and_nothing_is_drawn() {
    let Some(adapter) = common::adapter() else { return };
    // The downlevel limits allow 4 storage buffers per stage; Vello's pipelines need more, which wgpu reports as a
    // validation error rather than as an `Err` from `Renderer::new`.
    let descriptor = wgpu::DeviceDescriptor {
        required_limits: wgpu::Limits::downlevel_defaults(),
        ..wgpu::DeviceDescriptor::default()
    };
    let Ok((device, queue)) = pollster::block_on(adapter.request_device(&descriptor)) else {
        eprintln!("skipped: the adapter has no downlevel device");
        return;
    };
    let texture = common::face_texture(&device, spec().size_px());
    let mut native = (device.clone(), queue.clone(), texture.clone());
    let mut list = DrawList::new();
    overlapping_fills(&mut list);
    let mut rasterizer = VelloRasterizer::new();
    for attempt in 0..3 {
        assert!(!rasterizer.rasterize_native(&spec(), &list, &mut native).unwrap(), "attempt {attempt} drew");
    }
    assert!(common::read_back(&(device, queue), &texture).iter().all(|&b| b == 0), "a declined paint wrote pixels");
}
