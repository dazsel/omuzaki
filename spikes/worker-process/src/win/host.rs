//! The Windows host: imports the worker's slots, keeps the last frame in a texture of its own, sends that texture to Spout
//! every tick, and runs the shared kill loop.

#![allow(unsafe_code)]

use std::collections::HashMap;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use spout_out::gpu;
use spout_out::interop::SpoutBridge;
use spout_out::scene::FORMAT;
use windows::Win32::Graphics::Direct3D12::{ID3D12CommandQueue, ID3D12Device, ID3D12Fence, ID3D12Resource};

use super::shared::{open_by_name, raw_device_and_queue};
use super::{SLOTS, object_name};
use crate::Error;
use crate::analysis::{self, Analysis, Present};
use crate::options::HostOptions;
use crate::protocol::Msg;
use crate::schedule::KillSchedule;
use crate::supervise::{self, OnFrame, Shared, SuperviseConfig};

/// Spout sender name; OBS lists it under this.
const SENDER_NAME: &str = "omuzaki-worker-process-spike";

/// One worker's imported objects.
struct Imported {
    slots: Vec<wgpu::Texture>,
    fence: ID3D12Fence,
}

/// The host's GPU side: its device, the texture that holds the last frame, and what it has imported.
struct HostGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    device12: ID3D12Device,
    queue12: ID3D12CommandQueue,
    hold: wgpu::Texture,
    size: (u32, u32),
    host_pid: u32,
    imported: Mutex<HashMap<u32, Imported>>,
}

impl HostGpu {
    /// Opens worker `worker`'s slots and fence; called on its first frame, when they certainly exist.
    fn import(&self, worker: u32) -> Result<Imported, Error> {
        let extent = wgpu::Extent3d { width: self.size.0, height: self.size.1, depth_or_array_layers: 1 };
        let mut slots = Vec::new();
        for slot in 0..SLOTS {
            let resource: ID3D12Resource =
                open_by_name(&self.device12, &object_name(self.host_pid, worker, Some(slot)))?;
            // SAFETY: the resource is the worker's 2D BGRA8 texture of `extent`, opened on this device; it is only read.
            let hal = unsafe {
                wgpu::hal::dx12::Device::texture_from_raw(resource, FORMAT, wgpu::TextureDimension::D2, extent, 1, 1)
            };
            let desc = wgpu::TextureDescriptor {
                label: Some("imported slot"),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: FORMAT,
                usage: wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            };
            // SAFETY: made from this device's resource, matches `desc`. It is initialised (the worker drew it) and, with
            // ALLOW_SIMULTANEOUS_ACCESS, usable as a copy source without a state change.
            slots.push(unsafe {
                self.device.create_texture_from_hal::<wgpu::hal::api::Dx12>(hal, &desc, wgpu::TextureUses::COPY_SRC)
            });
        }
        let fence = open_by_name(&self.device12, &object_name(self.host_pid, worker, None))?;
        Ok(Imported { slots, fence })
    }

    /// Makes the host's queue wait (on the GPU) for the worker's frame, then copies it into the hold texture.
    fn take_frame(&self, worker: u32, slot: u32, index: u64) -> Result<(), Error> {
        let mut imported = self.imported.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if !imported.contains_key(&worker) {
            // The previous worker is dead; its textures are no longer needed, the hold texture has the picture.
            imported.clear();
            imported.insert(worker, self.import(worker)?);
        }
        let Some(from) = imported.get(&worker) else { return Err(Error::Config("import vanished".into())) };
        // SAFETY: the queue and fence are valid COM objects; the wait is queued on the GPU, not the CPU.
        unsafe { self.queue12.Wait(&from.fence, index + 1) }
            .map_err(|source| Error::Win32 { what: "queue Wait", source })?;
        let extent = wgpu::Extent3d { width: self.size.0, height: self.size.1, depth_or_array_layers: 1 };
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("hold") });
        let slot = from.slots.get(slot as usize).ok_or_else(|| Error::Config(format!("slot {slot} out of range")))?;
        encoder.copy_texture_to_texture(slot.as_image_copy(), self.hold.as_image_copy(), extent);
        // An empty render pass that loads and stores the hold texture, so wgpu leaves it in the render-target state that
        // `SpoutBridge` wraps it in; a copy alone would leave it in copy-destination.
        let view = self.hold.create_view(&wgpu::TextureViewDescriptor::default());
        drop(encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("hold to render target"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        }));
        self.queue.submit([encoder.finish()]);
        Ok(())
    }
}

/// The host's report: the timings, plus what Windows can say cheaply about handles.
#[derive(Debug, Serialize)]
pub struct Report {
    /// Adapter name and backend.
    pub adapter: String,
    /// Kill count, seed, size.
    pub kills: u32,
    /// Seed of the kill schedule.
    pub seed: u64,
    /// The timings.
    pub analysis: Analysis,
    /// Frames the Spout sender failed to send.
    pub send_errors: u64,
}

/// Runs the experiment on Windows: the same kill loop as Linux, with Spout as the output.
///
/// # Errors
/// Fails when the GPU, a shared object, Spout or a process cannot be used.
pub fn run(options: &HostOptions) -> Result<Report, Error> {
    let exe = std::env::current_exe().map_err(Error::io("finding this executable"))?;
    let host_pid = std::process::id();
    let gpu = gpu::open(wgpu::Backends::DX12)?;
    let (device12, queue12) = raw_device_and_queue(&gpu.device)?;
    let hold = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("hold"),
        size: wgpu::Extent3d { width: options.size.0, height: options.size.1, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let host_gpu = Arc::new(HostGpu {
        device: gpu.device.clone(),
        queue: gpu.queue.clone(),
        device12,
        queue12,
        hold,
        size: options.size,
        host_pid,
        imported: Mutex::new(HashMap::new()),
    });
    let bridge = SpoutBridge::new(&gpu.device, &host_gpu.hold).map_err(|e| Error::Config(e.to_string()))?;
    let mut sender = bridge.create_sender(SENDER_NAME).map_err(|e| Error::Config(e.to_string()))?;

    let shared = Arc::new(Shared::new());
    let frames_taken = Arc::new(AtomicU64::new(0));
    let last_arrival_ms = Arc::new(Mutex::new((0u64, 0.0f64)));
    let stop = Arc::new(AtomicBool::new(false));
    let on_frame: OnFrame = {
        let (host_gpu, frames_taken, shared, last) =
            (Arc::clone(&host_gpu), Arc::clone(&frames_taken), Arc::clone(&shared), Arc::clone(&last_arrival_ms));
        Arc::new(move |worker, msg| {
            let Msg::Frame { slot, index, .. } = *msg else { return false };
            if host_gpu.take_frame(worker, slot, index).is_err() {
                return false;
            }
            let count = frames_taken.fetch_add(1, Ordering::AcqRel) + 1;
            *last.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (count, shared.now_ms());
            true
        })
    };
    let spawn = |worker: u32| {
        Command::new(&exe)
            .arg("worker")
            .arg("--host")
            .arg(host_pid.to_string())
            .arg("--worker-number")
            .arg(worker.to_string())
            .arg("--size")
            .arg(format!("{}x{}", options.size.0, options.size.1))
            .arg("--fps")
            .arg(options.fps.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
    };
    let config = SuperviseConfig {
        kills: options.kills,
        baseline: Duration::from_secs(options.baseline_s),
        schedule: KillSchedule::new(options.seed, options.kill_window_ms.0, options.kill_window_ms.1),
    };

    let period = Duration::from_secs_f64(1.0 / options.fps);
    let (presents, send_errors) = thread::scope(|scope| {
        let supervisor = scope.spawn(|| {
            let result = supervise::run(&shared, config, spawn, &on_frame, || {});
            stop.store(true, Ordering::Release);
            result
        });
        let mut presents = Vec::new();
        let mut send_errors = 0u64;
        while frames_taken.load(Ordering::Acquire) == 0 && !stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(1));
        }
        let (mut seen, mut next) = (0u64, Instant::now());
        while !stop.load(Ordering::Acquire) {
            let (count, arrived_ms) = *last_arrival_ms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            // Sent whether or not a new frame arrived: the hold texture is the last frame.
            if sender.send().and_then(|()| bridge.wait_gpu().map_err(|_| spout_out::spout::SpoutError::Send)).is_err() {
                send_errors += 1;
            }
            sender.sender().signal_sync();
            let now_ms = shared.now_ms();
            presents.push(Present { t_ms: now_ms, index: count, fresh: count != seen, age_ms: now_ms - arrived_ms });
            seen = count;
            next += period;
            thread::sleep(next.saturating_duration_since(Instant::now()));
        }
        supervisor.join().map_err(|_| Error::Panicked).and_then(|result| result).map(|()| (presents, send_errors))
    })?;

    let analysis = analysis::analyse(&shared.take(), &presents, &[], period.as_secs_f64() * 1000.0);
    Ok(Report { adapter: gpu.adapter, kills: options.kills, seed: options.seed, analysis, send_errors })
}
