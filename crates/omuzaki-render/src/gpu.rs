//! The GPU the renderer draws with: adapter selection, the device and queue, what the device can do, and the helpers
//! that turn wgpu's failures into [`RenderError`]s.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use omuzaki_render_api::RenderError;

use crate::caps::{self, CapabilityProfile};

/// The longest the renderer waits for the GPU in any one call. Far above what a frame takes, even a 4K frame with
/// supersampling on a software rasteriser, so it only ever ends a wait that would otherwise never end (a hung driver,
/// a lost wake-up): the call then returns an error instead of freezing the caller.
pub(crate) const WAIT_LIMIT: Duration = Duration::from_secs(120);

/// The colour format scene passes draw into: linear light, premultiplied alpha, and headroom above 1.0 for bright
/// lights and bloom. Changing it is a one-line edit here; every pass reads it from this constant.
pub(crate) const SCENE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// The depth format of scene passes.
pub(crate) const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// The format of a render target: sRGB-*encoded* bytes (the Resolve pass encodes by hand), so the same texture can be
/// shown by egui or handed to an output without a format conversion, and premultiplication applies to the stored
/// numbers as the contract defines it.
pub(crate) const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

/// Which adapter [`WgpuRenderer::headless`](crate::WgpuRenderer::headless) opens.
///
/// `WGPU_ADAPTER_NAME` (a case-insensitive part of the adapter's name) overrides either choice, and `WGPU_BACKEND`
/// restricts the backends, so a developer can point any test or bench at a specific GPU.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum AdapterPreference {
    /// The best GPU: what an application wants.
    #[default]
    Auto,
    /// A software rasteriser (lavapipe on Linux, WARP on Windows). Slow but deterministic and never in competition
    /// with other GPU work on the machine, which is what tests want.
    Software,
}

/// How to open a headless renderer.
///
/// Headless devices leave Vulkan's debug names (`VK_EXT_debug_utils`) off, because the Vulkan loader on the dev and CI
/// machines crashes when one thread names an object while another drops a device. `WGPU_DEBUG=1` turns them
/// back on, for a single-device session in `RenderDoc`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct HeadlessOptions {
    /// Which adapter to use.
    pub adapter: AdapterPreference,
}

/// A device and queue plus what they can do.
pub(crate) struct Gpu {
    pub(crate) device: wgpu::Device,
    pub(crate) queue: wgpu::Queue,
    /// For logs and `Capabilities::backend`.
    pub(crate) backend_name: String,
    /// The largest texture side the device accepts.
    pub(crate) max_texture_size: u32,
    /// The hardware MSAA sample counts [`SCENE_FORMAT`] supports, as a set of the values 1, 2, 4 and 8 or'd together
    /// (so 4 and 1 are `0b0101`). Not every adapter has 2 or 8; lavapipe has only 1 and 4.
    sample_counts: u32,
    /// Whether per-pass GPU timestamps work: both the pass-boundary and the between-passes kinds.
    pub(crate) timestamps: bool,
    /// Whether the adapter is lavapipe/llvmpipe, whose anisotropic filter is not trustworthy (see `max_anisotropy`).
    lavapipe: bool,
    /// Set by the device-lost callback. Only headless devices have one (the owner of a shared device sees the loss
    /// first); on a shared device a loss shows up as the errors every call then returns.
    lost: Arc<AtomicBool>,
}

/// The MSAA sample counts a device may use for [`SCENE_FORMAT`], as a set of the values 1, 2, 4 and 8 or'd together.
///
/// 1 is always there and 4 whenever the adapter offers it (the WebGPU guarantee). 2 and 8 are adapter-specific: wgpu
/// only lets a device use them when it was opened with `TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES`, and picking 8 on a
/// device without it makes `create_texture` fail. So they are kept only with that feature on the device and the
/// flags offering the count.
fn sample_count_mask(features: wgpu::Features, flags: wgpu::TextureFormatFeatureFlags) -> u32 {
    let adapter_specific = features.contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES);
    [1u32, 2, 4, 8]
        .into_iter()
        .filter(|&n| match n {
            1 => true,
            4 => flags.sample_count_supported(4),
            _ => adapter_specific && flags.sample_count_supported(n),
        })
        .fold(0, |set, n| set | n)
}

/// Whether an adapter is Mesa's lavapipe, which reports itself as a CPU adapter named or driven by "llvmpipe".
fn is_lavapipe(kind: wgpu::DeviceType, name: &str, driver: &str) -> bool {
    let says = |text: &str| {
        let text = text.to_lowercase();
        text.contains("llvmpipe") || text.contains("lavapipe")
    };
    kind == wgpu::DeviceType::Cpu && (says(name) || says(driver))
}

impl Gpu {
    /// Wraps a device the caller owns (the egui app's). The adapter supplies the name and the format capabilities.
    pub(crate) fn from_parts(adapter: &wgpu::Adapter, device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let info = adapter.get_info();
        let formats = adapter.get_texture_format_features(SCENE_FORMAT);
        let sample_counts = sample_count_mask(device.features(), formats.flags);
        let wanted = wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
        Self {
            max_texture_size: device.limits().max_texture_dimension_2d,
            timestamps: device.features().contains(wanted),
            lavapipe: is_lavapipe(info.device_type, &info.name, &info.driver),
            backend_name: format!("wgpu {:?} ({})", info.backend, info.name),
            sample_counts,
            device,
            queue,
            lost: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Opens a device of its own on an adapter chosen by `options`, with the profile `OMUZAKI_RENDER_PROFILE` names
    /// (`Native` if unset), so the test suite can be pointed at any profile without a code change.
    pub(crate) fn open_headless(options: HeadlessOptions) -> Result<Self, RenderError> {
        Self::open_headless_with(options, CapabilityProfile::from_env()?)
    }

    /// Opens a device of its own on an adapter chosen by `options`, asking for exactly the limits and features of
    /// `profile`. The adapter's own limits are never requested, so what works here works on any device that meets the
    /// profile.
    ///
    /// # Errors
    /// `Backend` listing every unmet requirement if the profile cannot run the renderer, or saying the adapter cannot
    /// provide the profile; `Backend` if no adapter or device is available.
    pub(crate) fn open_headless_with(
        options: HeadlessOptions,
        profile: CapabilityProfile,
    ) -> Result<Self, RenderError> {
        caps::require(&format!("the {profile} profile"), &profile.limits())?;
        let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
        // The GL backend is a fallback for old hardware; it is not part of the product and crashes at exit on some
        // drivers (WSL's D3D12-backed GL), so it is opt-in through WGPU_BACKEND only.
        descriptor.backends = wgpu::Backends::PRIMARY;
        descriptor.flags = headless_instance_flags(descriptor.flags);
        let instance = wgpu::Instance::new(descriptor.with_env());
        let adapter = pollster::block_on(choose_adapter(&instance, options.adapter))?;

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("omuzaki-render"),
            required_features: adapter.features() & profile.features(),
            required_limits: profile.limits(),
            ..Default::default()
        }))
        .map_err(|e| {
            RenderError::Backend(format!(
                "opening the GPU device with the {profile} profile (the adapter may not meet it): {e}"
            ))
        })?;

        let gpu = Self::from_parts(&adapter, device, queue);
        let lost = Arc::clone(&gpu.lost);
        gpu.device.set_device_lost_callback(move |_reason, _message| lost.store(true, Ordering::Release));
        Ok(gpu)
    }

    /// The most anisotropic taps a sampler may ask for. Only lavapipe gets 1: its anisotropic path breaks a magnified,
    /// face-on footprint into texel-sized steps along every soft edge. WARP, real GPUs and every other adapter
    /// keep what the texture asks for.
    pub(crate) fn max_anisotropy(&self) -> u16 {
        if self.lavapipe { 1 } else { 16 }
    }

    /// The most samples per pixel the hardware offers for the scene colour.
    pub(crate) fn max_samples(&self) -> u8 {
        // The set holds the counts themselves, so its highest set bit is the largest count.
        let highest = self.sample_counts.checked_ilog2().map_or(1, |bit| 1u32 << bit);
        u8::try_from(highest).unwrap_or(1)
    }

    /// The hardware sample count to draw with for a request of `requested` samples: the largest supported count that
    /// does not exceed it. A request above the hardware maximum (the offline preset asks for 16) gets the maximum, which
    /// is the fallback `Capabilities::max_samples` promises.
    pub(crate) fn samples_for(&self, requested: u8) -> u32 {
        [8u32, 4, 2, 1].into_iter().find(|&n| n <= u32::from(requested) && self.sample_counts & n != 0).unwrap_or(1)
    }

    /// Blocks until the GPU has finished everything submitted so far, running the callbacks that were waiting on it,
    /// but for at most `limit`. Only tests, exports and tools wait; the frame loop never does.
    ///
    /// # Errors
    /// `Backend` if the GPU is still busy when `limit` has passed.
    pub(crate) fn wait_idle(&self, limit: Duration) -> Result<(), RenderError> {
        // Writes queued by `write_buffer` (a dynamic mesh's update) only reach the GPU, and free their staging memory,
        // with the next submission; an empty one flushes them so the wait covers them. Flushing can fail (the
        // command encoder may not allocate), and wgpu panics on an error nobody captured, so it runs in a scope.
        let flushed = scope_errors(&self.device, "a queue flush", || {
            self.queue.submit(std::iter::empty());
            Ok(())
        });
        let polled = self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(limit) });
        let waited = wait_outcome(&polled, limit);
        // Polling is what delivers a device loss, and a flush on a lost device failing is the loss, not a new fault.
        match flushed {
            Err(_) if self.check_alive().is_err() => waited,
            flushed => flushed.and(waited),
        }
    }

    /// `DeviceLost` if the device has gone.
    pub(crate) fn check_alive(&self) -> Result<(), RenderError> {
        if self.lost.load(Ordering::Acquire) { Err(RenderError::DeviceLost) } else { Ok(()) }
    }

    /// Runs `work` and turns anything the GPU API reports while it runs into an error.
    ///
    /// wgpu panics on errors nobody captured, and a library must not; every call that touches the GPU goes through
    /// here. `what` names the thing being made, for the out-of-memory message.
    pub(crate) fn scoped<T>(
        &self,
        what: &'static str,
        work: impl FnOnce() -> Result<T, RenderError>,
    ) -> Result<T, RenderError> {
        self.check_alive()?;
        // An error after the device was lost is the loss, not a fault in the request.
        scope_errors(&self.device, what, work)
            .map_err(|e| if self.check_alive().is_err() { RenderError::DeviceLost } else { e })
    }
}

/// What a bounded wait's result means: only running out of time is an error here. Any other answer ends the wait, a
/// lost device included, which the next call reports as `DeviceLost`.
fn wait_outcome(polled: &Result<wgpu::PollStatus, wgpu::PollError>, limit: Duration) -> Result<(), RenderError> {
    if matches!(polled, Err(wgpu::PollError::Timeout)) {
        return Err(RenderError::Backend(format!("the GPU did not finish within {limit:?}; giving up on it")));
    }
    Ok(())
}

/// Runs `work` inside validation and out-of-memory error scopes of `device`.
///
/// Never waits: popping a scope is read with [`ready_now`]. wgpu's native backend resolves a scope the moment it is
/// popped, so nothing is lost; a backend that resolves them later (the browser's) is reported as an error rather than
/// blocked on, because blocking on a frame's critical path is exactly what a real-time renderer must not do.
pub(crate) fn scope_errors<T>(
    device: &wgpu::Device,
    what: &'static str,
    work: impl FnOnce() -> Result<T, RenderError>,
) -> Result<T, RenderError> {
    let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
    let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
    let value = work();
    let memory = ready_now(memory.pop());
    let validation = ready_now(validation.pop());
    match (memory, validation) {
        (Some(Some(_)), _) => Err(RenderError::OutOfMemory { what }),
        (Some(None), Some(Some(error))) => Err(RenderError::Backend(format!("{what}: {error}"))),
        (Some(None), Some(None)) => value,
        // A caller's own error still beats this one: it says what the caller got wrong.
        _ => value.and(Err(RenderError::Backend(format!(
            "{what}: this backend resolves error scopes asynchronously, which a synchronous call cannot wait for"
        )))),
    }
}

/// The output of `future` if it is ready on the first poll; never waits and never allocates.
fn ready_now<T>(future: impl Future<Output = T>) -> Option<T> {
    match pin!(future).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => Some(value),
        Poll::Pending => None,
    }
}

/// An adapter as far as choosing one is concerned.
struct Candidate {
    name: String,
    kind: wgpu::DeviceType,
}

/// The instance flags for a headless device: wgpu's defaults, minus `DEBUG` (Vulkan's `VK_EXT_debug_utils`, which names
/// objects and labels command buffers for tools like `RenderDoc`).
///
/// Why: Vulkan loader 1.3.204 (Ubuntu 22.04's `libvulkan1` 1.3.204.1-2, on the dev and CI machines) looks the device up
/// for every debug-utils call by walking a process-wide list of every instance's devices, with no lock, and
/// destroying a device clears its handle while it is still on that list. A thread naming an object while another
/// drops a device therefore dereferences null inside `libvulkan.so.1` (`terminator_SetDebugUtilsObjectNameEXT`): the
/// intermittent SIGSEGV hit by test binaries whose tests each open and drop their own device in parallel.
/// Loader 1.3.280 and later lock that walk. Without the extension wgpu never makes the calls, and nothing else on the
/// headless path reaches the list.
///
/// `WGPU_DEBUG=1` still wins, because [`Gpu::open_headless`] applies the environment after this.
fn headless_instance_flags(defaults: wgpu::InstanceFlags) -> wgpu::InstanceFlags {
    defaults - wgpu::InstanceFlags::DEBUG
}

/// The index of the adapter to use among `candidates`: the first whose name contains `named` (case-insensitively) if a
/// name is given, otherwise the first software one.
///
/// # Errors
/// A message naming what was wanted and what is available.
fn pick(candidates: &[Candidate], named: Option<&str>) -> Result<usize, String> {
    let found = candidates.iter().position(|c| match named {
        Some(name) => c.name.to_lowercase().contains(&name.to_lowercase()),
        None => c.kind == wgpu::DeviceType::Cpu,
    });
    found.ok_or_else(|| {
        let wanted =
            named.map_or_else(|| "a software adapter (install mesa-vulkan-drivers)".to_owned(), |n| format!("{n:?}"));
        let available: Vec<String> = candidates.iter().map(|c| format!("{:?} ({:?})", c.name, c.kind)).collect();
        format!("no adapter matches {wanted}; available: {}", available.join(", "))
    })
}

/// Picks an adapter: `WGPU_ADAPTER_NAME` if set, else by `preference`.
async fn choose_adapter(
    instance: &wgpu::Instance,
    preference: AdapterPreference,
) -> Result<wgpu::Adapter, RenderError> {
    let named = std::env::var("WGPU_ADAPTER_NAME").ok();
    if named.is_none() && preference == AdapterPreference::Auto {
        return instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance),
                ..Default::default()
            })
            .await
            .map_err(|e| RenderError::Backend(format!("no GPU adapter: {e}")));
    }
    // `all()` is a filter over the backends the instance started, not a request to start them; `PRIMARY` here would hide
    // the GL adapters that `WGPU_BACKEND=gl` opted in to.
    let mut adapters = instance.enumerate_adapters(wgpu::Backends::all()).await;
    let candidates: Vec<Candidate> = adapters
        .iter()
        .map(|a| {
            let info = a.get_info();
            Candidate { name: info.name, kind: info.device_type }
        })
        .collect();
    let index = pick(&candidates, named.as_deref()).map_err(RenderError::Backend)?;
    Ok(adapters.swap_remove(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> Vec<Candidate> {
        vec![
            Candidate { name: "NVIDIA GeForce RTX 4090".into(), kind: wgpu::DeviceType::DiscreteGpu },
            Candidate { name: "llvmpipe (LLVM 15.0.7, 256 bits)".into(), kind: wgpu::DeviceType::Cpu },
        ]
    }

    #[test]
    fn the_sample_count_mask_keeps_1_and_4_and_offers_8_only_with_the_adapter_specific_feature() {
        use wgpu::TextureFormatFeatureFlags as Flags;
        let all = Flags::MULTISAMPLE_X2 | Flags::MULTISAMPLE_X4 | Flags::MULTISAMPLE_X8;
        let with = wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        let without = wgpu::Features::empty();

        // Without the device feature 8 is never offered, even when the adapter's flags have it.
        let mask = sample_count_mask(without, all);
        assert_eq!(mask & 8, 0, "8 must not be used without the device feature");
        assert_eq!(mask, 0b0101, "1 and 4 only: {mask:#b}");

        // With the feature and the flags, 8 is in (and 2 with it).
        let mask = sample_count_mask(with, all);
        assert_eq!(mask, 0b1111, "{mask:#b}");
        assert_eq!(sample_count_mask(with, Flags::MULTISAMPLE_X4 | Flags::MULTISAMPLE_X8) & 8, 8);
        assert_eq!(sample_count_mask(with, Flags::MULTISAMPLE_X4) & 8, 0, "8 needs the flags too");
    }

    #[test]
    fn the_sample_count_mask_always_holds_1_even_with_no_flags() {
        use wgpu::TextureFormatFeatureFlags as Flags;
        for features in [wgpu::Features::empty(), wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES] {
            assert_eq!(sample_count_mask(features, Flags::empty()), 0b0001);
            assert_eq!(sample_count_mask(features, Flags::MULTISAMPLE_X8) & 1, 1);
        }
    }

    #[test]
    fn software_means_the_cpu_adapter_and_a_name_matches_any_part_case_insensitively() {
        assert_eq!(pick(&candidates(), None), Ok(1));
        assert_eq!(pick(&candidates(), Some("RTX")), Ok(0));
        assert_eq!(pick(&candidates(), Some("LLVMPIPE")), Ok(1));
    }

    #[test]
    fn only_lavapipe_is_treated_as_the_broken_anisotropic_adapter() {
        use wgpu::DeviceType::{Cpu, DiscreteGpu};
        assert!(is_lavapipe(Cpu, "llvmpipe (LLVM 15.0.7, 256 bits)", "llvmpipe"));
        assert!(is_lavapipe(Cpu, "Lavapipe", ""));
        assert!(!is_lavapipe(Cpu, "Microsoft Basic Render Driver", "D3D12 (WARP)"));
        assert!(!is_lavapipe(DiscreteGpu, "NVIDIA GeForce RTX 4090", "NVIDIA"));
    }

    #[test]
    fn headless_instances_drop_vulkan_debug_names_and_keep_every_other_flag() {
        let wgpu_defaults = wgpu::InstanceFlags::debugging() | wgpu::InstanceFlags::GPU_BASED_VALIDATION;
        let flags = headless_instance_flags(wgpu_defaults);
        assert!(!flags.contains(wgpu::InstanceFlags::DEBUG));
        assert_eq!(flags, wgpu_defaults - wgpu::InstanceFlags::DEBUG);
        assert_eq!(headless_instance_flags(wgpu::InstanceFlags::empty()), wgpu::InstanceFlags::empty());
    }

    #[test]
    fn a_missing_adapter_is_an_error_that_lists_what_exists() {
        let message = pick(&candidates(), Some("radeon")).unwrap_err();
        assert!(
            message.contains("\"radeon\"") && message.contains("RTX 4090") && message.contains("llvmpipe"),
            "{message}"
        );
        let message = pick(&candidates()[..1], None).unwrap_err();
        assert!(message.contains("software adapter"), "{message}");
    }

    #[test]
    fn a_wait_that_runs_out_of_time_is_an_error_and_any_other_end_is_not() {
        let limit = Duration::from_secs(3);
        let Err(RenderError::Backend(message)) = wait_outcome(&Err(wgpu::PollError::Timeout), limit) else {
            panic!("a timeout must be an error")
        };
        assert!(message.contains("did not finish within 3s"), "{message}");
        assert_eq!(wait_outcome(&Ok(wgpu::PollStatus::WaitSucceeded), limit), Ok(()));
        assert_eq!(wait_outcome(&Ok(wgpu::PollStatus::QueueEmpty), limit), Ok(()));
        assert_eq!(wait_outcome(&Err(wgpu::PollError::WrongSubmissionIndex(2, 1)), limit), Ok(()));
    }

    #[test]
    fn waiting_for_an_idle_gpu_returns_at_once_and_a_zero_limit_cannot_hang() {
        let gpu = Gpu::open_headless(HeadlessOptions { adapter: AdapterPreference::Software }).unwrap();
        gpu.queue.submit([]);
        assert_eq!(gpu.wait_idle(Duration::from_secs(30)), Ok(()));
        // Out of time before it starts: an error or an answer, but it returns.
        let _ = gpu.wait_idle(Duration::ZERO);
    }

    #[test]
    fn waiting_on_a_destroyed_device_flushes_inside_a_scope_and_never_panics() {
        let gpu = Gpu::open_headless(HeadlessOptions { adapter: AdapterPreference::Software }).unwrap();
        gpu.device.destroy();
        // Whatever the flush reports, it is a value (or the loss delivered by the poll), never wgpu's default panic.
        let _ = gpu.wait_idle(Duration::from_secs(30));
        let _ = gpu.wait_idle(Duration::from_secs(30));
    }

    #[test]
    fn ready_now_reads_a_finished_future_and_gives_up_on_one_that_is_not() {
        assert_eq!(ready_now(std::future::ready(7)), Some(7));
        assert_eq!(ready_now(std::future::pending::<u8>()), None);
    }

    #[test]
    fn error_scopes_turn_a_validation_failure_into_an_error_without_waiting() {
        let gpu = Gpu::open_headless(HeadlessOptions { adapter: AdapterPreference::Software }).unwrap();
        let fine = scope_errors(&gpu.device, "a buffer", || Ok(7));
        assert_eq!(fine, Ok(7));

        // A buffer with no usage is a validation error in every backend.
        let broken = scope_errors(&gpu.device, "a buffer", || {
            let _ = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: 16,
                usage: wgpu::BufferUsages::empty(),
                mapped_at_creation: false,
            });
            Ok(())
        });
        let Err(RenderError::Backend(message)) = broken else { panic!("expected a backend error, got {broken:?}") };
        assert!(message.starts_with("a buffer: "), "{message}");

        // The error is gone once reported: the next scope starts clean.
        assert_eq!(scope_errors(&gpu.device, "a buffer", || Ok(())), Ok(()));
    }

    /// Opens a device on the software adapter, as every test does.
    fn software() -> Gpu {
        Gpu::open_headless(HeadlessOptions { adapter: AdapterPreference::Software }).unwrap()
    }

    /// Regression test: with `VK_EXT_debug_utils` on, a thread naming a Vulkan object walked the Vulkan loader's process-wide device
    /// list with no lock while another thread was destroying a device, and crashed in the loader (SIGSEGV). One
    /// thread here names objects as fast as it can while others open and drop devices; with debug utils off the
    /// naming never reaches the loader. A regression is a crash of the test process, not an assertion.
    #[test]
    fn devices_can_come_and_go_while_another_thread_creates_labelled_objects() {
        const CHURN_THREADS: usize = 4;
        const DEVICES_PER_THREAD: usize = 6;
        let labeller = software();
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while !stop.load(Ordering::Acquire) {
                    for _ in 0..64 {
                        let _ = labeller.device.create_buffer(&wgpu::BufferDescriptor {
                            label: Some("labelled"),
                            size: 4,
                            usage: wgpu::BufferUsages::COPY_DST,
                            mapped_at_creation: false,
                        });
                    }
                }
            });
            let churn: Vec<_> = (0..CHURN_THREADS)
                .map(|_| {
                    scope.spawn(|| {
                        for _ in 0..DEVICES_PER_THREAD {
                            drop(software());
                        }
                    })
                })
                .collect();
            for thread in churn {
                thread.join().unwrap();
            }
            stop.store(true, Ordering::Release);
        });
    }

    #[test]
    fn the_scene_formats_are_ones_every_backend_can_render_to() {
        assert!(
            SCENE_FORMAT.is_srgb() == OUTPUT_FORMAT.is_srgb(),
            "both are linear-coded formats; sRGB is applied by hand"
        );
        assert_eq!(DEPTH_FORMAT, wgpu::TextureFormat::Depth32Float);
    }
}
