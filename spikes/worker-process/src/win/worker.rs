//! The Windows worker: draws the scene, copies it into a shared texture, signals the shared fence, posts the slot.

#![allow(unsafe_code)]

use std::io::Write;
use std::time::Instant;

use spout_out::clock::Clock;
use spout_out::frame_tag::FrameTag;
use spout_out::gpu;
use spout_out::scene::{FORMAT, SceneParams, SceneRenderer};

use super::shared::{create_shared_fence, create_shared_texture, raw_device_and_queue};
use super::{SLOTS, object_name};
use crate::Error;
use crate::ack::SlotGate;
use crate::options;
use crate::protocol::Msg;

/// What the worker is told on its command line.
pub struct WorkerArgs {
    /// The host's process id, which names the shared objects.
    pub host: u32,
    /// This worker's number.
    pub worker: u32,
    /// Frame size.
    pub size: (u32, u32),
    /// Frame-rate cap.
    pub fps: f64,
}

fn post(msg: Msg) -> Result<(), Error> {
    let mut out = std::io::stdout().lock();
    out.write_all(&msg.encode()).and_then(|()| out.flush()).map_err(Error::io("posting to the host"))
}

fn ns(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Wraps a D3D12 resource this process created as a wgpu texture that can be a copy target.
fn wrap(
    device: &wgpu::Device,
    resource: windows::Win32::Graphics::Direct3D12::ID3D12Resource,
    size: (u32, u32),
) -> wgpu::Texture {
    let extent = wgpu::Extent3d { width: size.0, height: size.1, depth_or_array_layers: 1 };
    // SAFETY: the resource is a 2D BGRA8 texture of `extent`, created on this device, and nothing else in this process
    // uses it; its contents are fully overwritten before they are read.
    let hal = unsafe {
        wgpu::hal::dx12::Device::texture_from_raw(resource, FORMAT, wgpu::TextureDimension::D2, extent, 1, 1)
    };
    let desc = wgpu::TextureDescriptor {
        label: Some("shared slot"),
        size: extent,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    };
    // SAFETY: `hal` was made from this device's resource and matches `desc`.
    unsafe { device.create_texture_from_hal::<wgpu::hal::api::Dx12>(hal, &desc, wgpu::TextureUses::UNINITIALIZED) }
}

fn wait_for_gpu(device: &wgpu::Device) -> Result<(), Error> {
    device
        .poll(wgpu::PollType::wait_indefinitely())
        .map(|_| ())
        .map_err(|e| Error::Config(format!("waiting for the GPU: {e}")))
}

/// Renders until killed.
///
/// # Errors
/// Fails when the GPU, a shared object or the pipe fails.
pub fn run(args: &WorkerArgs) -> Result<(), Error> {
    let started = Instant::now();
    let gpu = gpu::open(wgpu::Backends::DX12)?;
    let gpu_ns = ns(started);
    let scene_started = Instant::now();
    let scene = SceneRenderer::new(&gpu.device, &gpu.queue, args.size)?;
    let scene_ns = ns(scene_started);
    let (device12, queue12) = raw_device_and_queue(&gpu.device)?;
    // The handles stay open until the process dies; closing them would unpublish the names.
    let mut handles = Vec::new();
    let mut slots = Vec::new();
    for slot in 0..SLOTS {
        let (resource, handle) =
            create_shared_texture(&device12, args.size, &object_name(args.host, args.worker, Some(slot)))?;
        handles.push(handle);
        slots.push(wrap(&gpu.device, resource, args.size));
    }
    let (fence, fence_handle) = create_shared_fence(&device12, &object_name(args.host, args.worker, None))?;
    handles.push(fence_handle);
    post(Msg::Ready { gpu_ns, scene_ns })?;

    let mut gate = SlotGate::<{ SLOTS as usize }>::new();
    let mut acks = std::io::stdin().lock();
    let clock = Clock::new();
    let period = options::period_for(args.fps)?;
    let mut deadline = Instant::now();
    for index in 0u64.. {
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        deadline = Instant::now().max(deadline) + period;

        let draw_started = Instant::now();
        let frame = u32::try_from(index).unwrap_or(u32::MAX);
        #[allow(clippy::cast_precision_loss)] // an animation phase: losing low bits of a huge frame number is harmless
        let t = index as f32 * 0.02;
        scene.render(&SceneParams { tag: FrameTag { frame, ticks: clock.now() }, t, truth: false });
        // Finish the draw first, so its time does not include the hand-off.
        wait_for_gpu(&gpu.device)?;
        let render_ns = ns(draw_started);

        let slot = u32::try_from(index % u64::from(SLOTS)).unwrap_or(0);
        // Do not overwrite a slot the host's copy may still be reading: wait until the host has handed it back.
        gate.take(slot as usize, &mut acks)?;
        let copy_started = Instant::now();
        let mut encoder = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("to slot") });
        let size = wgpu::Extent3d { width: args.size.0, height: args.size.1, depth_or_array_layers: 1 };
        encoder.copy_texture_to_texture(scene.texture().as_image_copy(), slots[slot as usize].as_image_copy(), size);
        gpu.queue.submit([encoder.finish()]);
        // Queued behind the copy on the same D3D12 queue, so the host's wait releases only once the slot is whole. Fence
        // value `index + 1` because a fence starts at 0.
        // SAFETY: the queue and fence are valid COM objects owned by this function.
        unsafe { queue12.Signal(&fence, index + 1) }.map_err(|source| Error::Win32 { what: "queue Signal", source })?;
        // Wait for the GPU so the posted frame is real; the 30 fps cap leaves room.
        wait_for_gpu(&gpu.device)?;
        let write_ns = ns(copy_started);
        gate.give(slot as usize);
        post(Msg::Frame { slot, index, render_ns, write_ns })?;
    }
    drop(handles);
    Ok(())
}
