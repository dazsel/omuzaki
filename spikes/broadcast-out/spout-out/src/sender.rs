//! The sending half: render the test scene with wgpu and hand every frame to Spout.

use std::time::Duration;

use crate::analysis;
use crate::clock::Clock;
use crate::frame_tag::FrameTag;
use crate::gpu::{self, GpuError};
use crate::interop::{BridgeError, BridgedSender, SpoutBridge};
use crate::report::{Path, SenderReport, SyncMode, fnv1a};
use crate::scene::{self, SceneError, SceneParams, SceneRenderer, TRUTH_BACKGROUND};
use crate::spout::{SpoutError, SpoutSender};
use crate::stats::Summary;

/// Frames at the start of a run that do not count towards timing: Spout creates its shared texture on the first send.
const WARMUP_FRAMES: usize = 5;
/// How long the sender stays registered after its last frame, so a receiver can finish reading it.
const LINGER: Duration = Duration::from_millis(500);

/// What to send and how.
#[derive(Clone, Debug)]
pub struct SendConfig {
    /// Requested Spout sender name.
    pub name: String,
    /// Frame size.
    pub size: (u32, u32),
    /// Frames to send.
    pub frames: u32,
    /// Target frame rate; 0 sends as fast as the GPU and Spout allow.
    pub fps: f64,
    /// Hand-off path.
    pub path: Path,
    /// Synchronisation mode (GPU path).
    pub sync: SyncMode,
    /// Read back every n-th frame and record its hash (0 = never).
    pub hash_every: u32,
}

/// Why a send run failed.
#[derive(Debug, thiserror::Error)]
pub enum SendError {
    /// No GPU.
    #[error(transparent)]
    Gpu(#[from] GpuError),
    /// The scene could not be made or read back.
    #[error(transparent)]
    Scene(#[from] SceneError),
    /// The D3D12 to D3D11 bridge failed.
    #[error(transparent)]
    Bridge(#[from] BridgeError),
    /// Spout failed.
    #[error(transparent)]
    Spout(#[from] SpoutError),
}

/// Renders the scene once as premultiplied and once over a known background, and compares how each receiver blend mode
/// reproduces the background version. Also checks the patches' stored bytes.
fn composite_analysis(scene: &SceneRenderer) -> Result<(Vec<analysis::Case>, bool), SendError> {
    let tag = FrameTag { frame: 0, ticks: 0 };
    scene.render(&SceneParams { tag, t: 0.5, truth: false });
    let premultiplied = scene.read_back()?;
    scene.render(&SceneParams { tag, t: 0.5, truth: true });
    let truth = scene.read_back()?;
    let cases =
        analysis::composite_cases(&premultiplied, &truth, scene.size().0, scene.picture_rows(), TRUTH_BACKGROUND);
    let patches_ok = scene::check_patches(&premultiplied, scene.size().0, scene.size().1).is_ok();
    Ok((cases, patches_ok))
}

/// Where frames go: the bridged GPU texture, or CPU pixels through a standalone sender.
enum Output<'a> {
    Gpu(BridgedSender<'a>),
    Cpu(SpoutSender),
}

impl Output<'_> {
    fn sender(&mut self) -> &mut SpoutSender {
        match self {
            Self::Gpu(bridged) => bridged.sender(),
            Self::Cpu(sender) => sender,
        }
    }
}

/// Sends `config.frames` frames and returns what was measured.
///
/// # Errors
/// Fails when the GPU, the bridge or Spout cannot be set up, or a send fails.
pub fn run(config: &SendConfig) -> Result<SenderReport, SendError> {
    let gpu = gpu::open(wgpu::Backends::DX12)?;
    let scene = SceneRenderer::new(&gpu.device, &gpu.queue, config.size)?;
    let (composite, patches_ok) = composite_analysis(&scene)?;

    // The sender borrows the bridge, so the compiler already guarantees the D3D11 device outlives Spout's texture.
    let bridge = match config.path {
        Path::Gpu => Some(SpoutBridge::new(&gpu.device, scene.texture())?),
        Path::Cpu => None,
    };
    let mut output = match &bridge {
        Some(bridge) => Output::Gpu(bridge.create_sender(&config.name)?),
        None => Output::Cpu(SpoutSender::standalone(&config.name)?),
    };

    let clock = Clock::new();
    let period = (config.fps > 0.0).then(|| clock.ticks_in_ms(1000.0 / config.fps));
    let (mut submit_ms, mut handoff_ms, mut period_ms) = (Vec::new(), Vec::new(), Vec::new());
    let mut hashes = Vec::new();
    let mut previous_start = None;
    let mut deadline = clock.now();

    for frame in 0..config.frames {
        if let Some(period) = period {
            clock.sleep_until(deadline);
            deadline += period;
        }
        let start = clock.now();
        #[allow(clippy::cast_precision_loss)] // an animation phase: losing low bits of a huge frame number is harmless
        let t = frame as f32 * 0.02;
        scene.render(&SceneParams { tag: FrameTag { frame, ticks: start }, t, truth: false });
        let submitted = clock.now();

        match &mut output {
            Output::Gpu(sender) => {
                sender.send()?;
                if config.sync == SyncMode::Fence
                    && let Some(bridge) = &bridge
                {
                    bridge.wait_gpu()?;
                }
            }
            Output::Cpu(sender) => {
                sender.send_image(&scene.read_back()?, config.size.0, config.size.1)?;
                if config.sync == SyncMode::Fence {
                    sender.wait_gpu()?;
                }
            }
        }
        output.sender().signal_sync();
        let done = clock.now();

        if frame as usize >= WARMUP_FRAMES {
            submit_ms.push(clock.ms_between(start, submitted));
            handoff_ms.push(clock.ms_between(submitted, done));
            if let Some(previous) = previous_start {
                period_ms.push(clock.ms_between(previous, start));
            }
        }
        previous_start = Some(start);
        if config.hash_every > 0 && frame % config.hash_every == 0 {
            hashes.push((frame, fnv1a(&scene.read_back()?)));
        }
    }
    std::thread::sleep(LINGER);

    Ok(SenderReport {
        name: output.sender().name().unwrap_or_else(|| config.name.clone()),
        adapter: gpu.adapter,
        backend: format!("{:?}", gpu.backend),
        size: config.size,
        path: config.path,
        sync: config.sync,
        fps: config.fps,
        sent: config.frames,
        submit_ms: Summary::of(&submit_ms),
        handoff_ms: Summary::of(&handoff_ms),
        period_ms: Summary::of(&period_ms),
        hashes,
        composite,
        patches_ok,
    })
}
