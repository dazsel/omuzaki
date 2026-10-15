//! The worker half of the CPU lane: draws the test picture with wgpu (lavapipe on a VM without a GPU), copies it into a
//! shared-memory slot and posts `(slot, frame index)` on stdout. It runs until it is killed or the host goes away.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use spout_out::clock::Clock;
use spout_out::frame_tag::FrameTag;
use spout_out::gpu;
use spout_out::scene::{SceneParams, SceneRenderer};

use crate::Error;
use crate::ack::SlotGate;
use crate::protocol::Msg;
use crate::shm::{SLOTS, SlotFile};

/// What the worker is told on its command line.
pub struct WorkerArgs {
    /// The slot ring file.
    pub shm: PathBuf,
    /// Frame size.
    pub size: (u32, u32),
    /// Frame-rate cap; the worker never renders faster than this.
    pub fps: f64,
}

fn post(msg: Msg) -> Result<(), Error> {
    let mut out = std::io::stdout().lock();
    out.write_all(&msg.encode()).and_then(|()| out.flush()).map_err(Error::io("posting to the host"))
}

fn ns(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_nanos()).unwrap_or(u64::MAX)
}

/// Renders until killed. Returns only with an error (a closed pipe means the host is gone).
///
/// # Errors
/// Fails when the GPU, the slot ring or the pipe fails.
pub fn run(args: &WorkerArgs) -> Result<(), Error> {
    let started = Instant::now();
    let gpu = gpu::open(wgpu::Backends::VULKAN)?;
    let gpu_ns = ns(started);
    let scene_started = Instant::now();
    let scene = SceneRenderer::new(&gpu.device, &gpu.queue, args.size)?;
    let frame_bytes = u64::from(args.size.0) * u64::from(args.size.1) * 4;
    let ring = SlotFile::open(&args.shm, frame_bytes)?;
    let mut gate = SlotGate::<{ SLOTS as usize }>::new();
    let mut acks = std::io::stdin().lock();
    post(Msg::Ready { gpu_ns, scene_ns: ns(scene_started) })?;

    let clock = Clock::new();
    let period = Duration::from_secs_f64(1.0 / args.fps);
    let mut deadline = Instant::now();
    for index in 0u64.. {
        // A frame that took longer than the period does not make the next one start early.
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
        deadline = Instant::now().max(deadline) + period;

        let draw_started = Instant::now();
        let frame = u32::try_from(index).unwrap_or(u32::MAX);
        #[allow(clippy::cast_precision_loss)] // an animation phase: losing low bits of a huge frame number is harmless
        let t = index as f32 * 0.02;
        scene.render(&SceneParams { tag: FrameTag { frame, ticks: clock.now() }, t, truth: false });
        let pixels = scene.read_back()?;
        let render_ns = ns(draw_started);

        let slot = u32::try_from(index % u64::from(SLOTS)).unwrap_or(0);
        // Do not overwrite a slot the host may still be reading: wait until it has handed the slot back.
        gate.take(slot as usize, &mut acks)?;
        let write_started = Instant::now();
        ring.write(slot, &pixels)?;
        gate.give(slot as usize);
        post(Msg::Frame { slot, index, render_ns, write_ns: ns(write_started) })?;
    }
    Ok(())
}
