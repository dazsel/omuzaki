//! The winit application: creates the window, applies the platform features, draws at a fixed rate and reports.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::gpu::{self, Frame, Renderer};
use crate::platform::{self, ClickThrough};
use crate::scene::{Fit, Mask, Scene};
use crate::scene_pass::{self, ReferenceError};
use crate::stats::{Series, Summary};

/// What the run does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Open the window and draw until closed or the time is up.
    Run,
    /// Open the window, apply the platform features, read the OS state back and exit. No GPU is touched, except that
    /// per-pixel click-through draws the picture once on a device of its own, because its mask comes from the pixels.
    Probe,
    /// List the alpha modes each wgpu backend offers for the window and exit.
    Caps,
}

/// Everything a run is configured with.
#[derive(Clone, Debug)]
pub struct Config {
    /// What to do.
    pub mode: Mode,
    /// GPU options.
    pub gpu: gpu::Options,
    /// Platform options.
    pub platform: platform::Options,
    /// Window size in pixels.
    pub size: (u32, u32),
    /// Target frames per second.
    pub fps: u32,
    /// Stop after this long.
    pub seconds: Option<f64>,
    /// Starting opacity, 0..=1.
    pub opacity: f32,
}

/// Why the spike failed to run.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
    /// The event loop could not start.
    #[error("event loop: {0}")]
    EventLoop(#[from] winit::error::EventLoopError),
    /// The window could not be created.
    #[error("creating the window: {0}")]
    Window(#[from] winit::error::OsError),
    /// The GPU side failed.
    #[error(transparent)]
    Gpu(#[from] gpu::GpuError),
    /// The reference picture (the click-through mask) could not be made.
    #[error(transparent)]
    Reference(#[from] ReferenceError),
}

/// What a finished run measured.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outcome {
    /// Frames presented.
    pub presented: u32,
    /// Wall-clock seconds from the first frame to the last.
    pub seconds: f64,
    /// CPU seconds the process used between the first frame and the last report, where the platform can say.
    pub cpu_seconds: Option<f64>,
    /// Wall-clock seconds that `cpu_seconds` covers.
    pub cpu_wall_seconds: f64,
    /// CPU time inside `render` (acquire, encode, submit, present).
    pub render: Option<Summary>,
    /// GPU time per frame from timestamp queries, where the adapter has them.
    pub gpu: Option<Summary>,
    /// Frames whose GPU timestamps could not be read back.
    pub gpu_failures: u32,
}

impl std::fmt::Display for Outcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let fps = if self.seconds > 0.0 { f64::from(self.presented) / self.seconds } else { 0.0 };
        write!(f, "total: {} frames in {:.1}s = {fps:.1} fps", self.presented, self.seconds)?;
        if let Some(percent) = self.cpu_percent() {
            write!(f, "; process CPU {percent:.1}% of one core")?;
        }
        if let Some(s) = self.render {
            write!(f, "; render() CPU ms mean {:.2}, p95 {:.2}, max {:.2}", s.mean_ms, s.p95_ms, s.max_ms)?;
        }
        match self.gpu {
            Some(s) => write!(
                f,
                "; GPU ms mean {:.2}, p95 {:.2}, max {:.2} ({} frames timed, {} failed)",
                s.mean_ms, s.p95_ms, s.max_ms, s.frames, self.gpu_failures
            ),
            None => write!(f, "; GPU time not measured"),
        }
    }
}

impl Outcome {
    /// Process CPU use as a percentage of one core.
    pub fn cpu_percent(&self) -> Option<f64> {
        self.cpu_seconds.filter(|_| self.cpu_wall_seconds > 0.0).map(|cpu| 100.0 * cpu / self.cpu_wall_seconds)
    }
}

/// Runs the spike to completion on its own event loop.
///
/// # Errors
/// See [`RunError`].
pub fn run(config: Config) -> Result<Outcome, RunError> {
    let mut builder = EventLoop::builder();
    platform::configure_event_loop(&mut builder, &config.platform);
    let event_loop = builder.build()?;
    let mut app = App::new(config);
    event_loop.run_app(&mut app)?;
    app.finish()
}

/// The process's CPU time at one moment, for the CPU percentage. Read once a report interval, not once a frame: the read
/// is a system call (a file read on Linux) and would sit in the frame loop.
#[derive(Clone, Copy, Debug)]
struct CpuMark {
    at: Instant,
    seconds: f64,
}

impl CpuMark {
    fn now() -> Option<Self> {
        platform::process_cpu_seconds().map(|seconds| Self { at: Instant::now(), seconds })
    }
}

/// The mascot window as a winit application. Standalone it is the whole event loop; in [`crate::host`] it shares the
/// loop with eframe.
pub struct App {
    config: Config,
    scene: Option<Scene>,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    platform: Option<platform::State>,
    render_ms: Series,
    gpu_ms: Series,
    opacity: f32,
    /// The mask is the degraded one (every pixel takes input), so the display opacity must not thin it out.
    mask_open: bool,
    started: Instant,
    next_frame: Instant,
    last_report: Instant,
    presented: u32,
    total_presented: u32,
    first_frame: Option<Instant>,
    last_frame: Option<Instant>,
    cpu_first: Option<CpuMark>,
    cpu_last: Option<CpuMark>,
    failure: Option<RunError>,
}

/// The frame rate the loop runs at: the configured one, with a zero (which the command line refuses) read as 1, so the
/// schedule and the sample buffers are sized from the same number.
fn effective_fps(config: &Config) -> u32 {
    config.fps.max(1)
}

/// Samples the frame series hold: a run with a deadline records every frame; one without keeps the most recent ones (the
/// series are rings, so recording allocates nothing however long the run lasts).
fn expected_frames(config: &Config) -> usize {
    config.seconds.map_or(4096, |s| (s.max(0.0) * f64::from(effective_fps(config))).ceil() as usize + 64)
}

impl App {
    /// An application that has not opened its window yet.
    pub fn new(config: Config) -> Self {
        let now = Instant::now();
        let frames = expected_frames(&config);
        Self {
            opacity: config.opacity.clamp(0.0, 1.0),
            mask_open: false,
            config,
            scene: None,
            window: None,
            renderer: None,
            platform: None,
            render_ms: Series::with_capacity(frames),
            gpu_ms: Series::with_capacity(frames),
            started: now,
            next_frame: now,
            last_report: now,
            presented: 0,
            total_presented: 0,
            first_frame: None,
            last_frame: None,
            cpu_first: None,
            cpu_last: None,
            failure: None,
        }
    }

    /// Whether `id` is this application's window (the shared host routes events by it).
    pub fn owns(&self, id: WindowId) -> bool {
        self.window.as_ref().is_some_and(|w| w.id() == id)
    }

    /// The result of the run, or the error that stopped it.
    ///
    /// # Errors
    /// The failure recorded while the loop ran.
    pub fn finish(self) -> Result<Outcome, RunError> {
        if let Some(e) = self.failure {
            return Err(e);
        }
        let seconds = match (self.first_frame, self.last_frame) {
            (Some(first), Some(last)) => (last - first).as_secs_f64(),
            _ => 0.0,
        };
        // The end of the run is a mark of its own: the last report's mark can be a second or two before it.
        let last = CpuMark::now().or(self.cpu_last);
        let (cpu_seconds, cpu_wall_seconds) = match (self.cpu_first, last) {
            (Some(a), Some(b)) => (Some(b.seconds - a.seconds), (b.at - a.at).as_secs_f64()),
            _ => (None, 0.0),
        };
        Ok(Outcome {
            presented: self.total_presented,
            seconds,
            cpu_seconds,
            cpu_wall_seconds,
            render: self.render_ms.overall(),
            gpu: self.gpu_ms.overall(),
            gpu_failures: self.renderer.as_ref().map_or(0, Renderer::gpu_failures),
        })
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: RunError) {
        self.failure = Some(error);
        event_loop.exit();
    }

    /// The picture drawn once at the window's current size, for the click-through mask.
    fn reference(&self, size: PhysicalSize<u32>) -> Result<Scene, ReferenceError> {
        scene_pass::render_reference(
            self.config.gpu.backends,
            self.config.gpu.fallback_adapter,
            (size.width, size.height),
        )
    }

    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), RunError> {
        let attributes = platform::window_attributes(self.config.size, &self.config.platform);
        let window = Arc::new(event_loop.create_window(attributes)?);
        self.window = Some(window.clone());
        if self.config.mode == Mode::Caps {
            for line in gpu::probe_backends(&window, &self.config.gpu) {
                println!("{line}");
            }
            event_loop.exit();
            return Ok(());
        }
        // Only drawing and per-pixel click-through need the picture; any other probe reports the OS state without a GPU.
        let scene = if self.config.mode == Mode::Probe && self.config.platform.click_through != ClickThrough::Pixel {
            Scene { width: 1, height: 1, pixels: vec![0; 4] }
        } else {
            self.reference(window.inner_size())?
        };
        let mask = mask(&window, &scene, mask_opacity(self.mask_open, self.opacity));
        let (mut state, features) = platform::State::new(&window, self.config.platform, &mask);
        println!(
            "window: {}x{} physical px, scale factor {}",
            mask.fit.window.0,
            mask.fit.window.1,
            window.scale_factor()
        );
        for feature in &features {
            println!("{feature}");
        }
        if self.config.mode == Mode::Probe {
            // Windows and macOS toggle per-pixel click-through from a cursor poll; do one so the readback shows it applied.
            state.tick(&window, &mask);
            for line in state.describe(&window) {
                println!("{line}");
            }
            event_loop.exit();
        } else {
            let renderer = Renderer::new(&window, &self.config.gpu)?;
            print_surface(&renderer);
            self.renderer = Some(renderer);
            // The run's clock starts now: opening the GPU and drawing the reference picture are not part of it.
            let now = Instant::now();
            (self.started, self.next_frame, self.last_report) = (now, now, now);
        }
        self.platform = Some(state);
        self.scene = Some(scene);
        Ok(())
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_mut() else { return };
        let begin = Instant::now();
        match renderer.render(self.opacity) {
            Ok(Frame::Presented) => {
                self.render_ms.record(begin.elapsed());
                renderer.drain_gpu_times(&mut self.gpu_ms);
                self.presented += 1;
                self.total_presented += 1;
                let now = Instant::now();
                if self.first_frame.is_none() {
                    self.first_frame = Some(now);
                    self.cpu_first = CpuMark::now();
                }
                self.last_frame = Some(now);
            }
            Ok(Frame::Skipped) => {}
            Err(e) => self.fail(event_loop, e.into()),
        }
    }

    fn report_if_due(&mut self) {
        let elapsed = self.last_report.elapsed();
        if elapsed < Duration::from_secs(2) {
            return;
        }
        self.cpu_last = CpuMark::now();
        if let Some(s) = self.render_ms.take_window() {
            let gpu = self.gpu_ms.take_window().map_or_else(String::new, |g| {
                format!("; GPU ms mean {:.2}, p95 {:.2}, max {:.2}", g.mean_ms, g.p95_ms, g.max_ms)
            });
            println!(
                "frames: {} presented in {:.1}s = {:.1} fps; render() CPU ms mean {:.2}, p95 {:.2}, max {:.2}{gpu}",
                s.frames,
                elapsed.as_secs_f64(),
                f64::from(self.presented) / elapsed.as_secs_f64(),
                s.mean_ms,
                s.p95_ms,
                s.max_ms
            );
        }
        self.presented = 0;
        self.last_report = Instant::now();
    }

    /// Draws a frame when one is due, polls the cursor for per-pixel click-through, and asks for the next wake-up.
    /// `merge` keeps an earlier wake-up that another handler on the same loop (eframe) already asked for.
    pub fn tick(&mut self, event_loop: &ActiveEventLoop, merge: bool) {
        let (Some(window), Some(scene)) = (self.window.clone(), self.scene.as_ref()) else { return };
        if self.config.mode != Mode::Run {
            return;
        }
        if self.config.seconds.is_some_and(|s| self.started.elapsed().as_secs_f64() >= s) {
            event_loop.exit();
            return;
        }
        if let Some(state) = self.platform.as_mut() {
            state.tick(&window, &mask(&window, scene, mask_opacity(self.mask_open, self.opacity)));
            if let Some(e) = state.take_poll_error() {
                println!("[FAILED]      click_through_pixel toggle: {e}");
            }
        }
        let now = Instant::now();
        let interval = Duration::from_secs_f64(1.0 / f64::from(effective_fps(&self.config)));
        if now >= self.next_frame {
            self.draw(event_loop);
            self.report_if_due();
            // Advance from the schedule, not from now, so a slow frame does not lower the rate for good.
            self.next_frame = (self.next_frame + interval).max(now);
        }
        // Wake for whichever comes first: the next frame or the next ~60 Hz cursor poll.
        let wake = self.next_frame.min(now + Duration::from_millis(16));
        let flow = match (merge, event_loop.control_flow()) {
            (true, ControlFlow::Poll) => ControlFlow::Poll,
            (true, ControlFlow::WaitUntil(other)) => ControlFlow::WaitUntil(other.min(wake)),
            _ => ControlFlow::WaitUntil(wake),
        };
        event_loop.set_control_flow(flow);
    }

    /// Opens the window the first time the loop resumes.
    pub fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.start(event_loop) {
            self.fail(event_loop, e);
        }
    }

    /// Rebuilds the click-through mask after the window's size or the opacity changed, and tells the platform layer.
    fn refresh_mask(&mut self) {
        let (Some(window), Some(scene)) = (self.window.clone(), self.scene.as_ref()) else { return };
        if let Some(feature) = self
            .platform
            .as_mut()
            .and_then(|p| p.refresh(&window, &mask(&window, scene, mask_opacity(self.mask_open, self.opacity))))
        {
            println!("{feature}");
        }
    }

    /// The window changed size (a display scale change does this): the picture is drawn in window pixels, so it is redone
    /// at the new size, and so is the click-through mask built from it. The mask only matters for per-pixel click-through.
    /// A zero size (a minimised window) changes nothing: the renderer keeps its last size and so does the mask.
    fn resized(&mut self, size: PhysicalSize<u32>) {
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.resize(size.width, size.height);
        }
        let current = self.scene.as_ref().map(|s| (s.width, s.height));
        let size = (size.width, size.height);
        if self.config.platform.click_through != ClickThrough::Pixel
            || size.0 == 0
            || size.1 == 0
            || current == Some(size)
        {
            return;
        }
        let (scene, degraded) = scene_for_size(size, |size| {
            scene_pass::render_reference(self.config.gpu.backends, self.config.gpu.fallback_adapter, size)
        });
        self.mask_open = degraded.is_some();
        if let Some(reason) = degraded {
            println!(
                "[FAILED]      mask at {}x{}: {reason}; per-pixel click-through is off until the size changes",
                size.0, size.1
            );
        }
        self.scene = Some(scene);
        self.refresh_mask();
    }

    /// Handles an event for this application's window.
    pub fn window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let Some(window) = self.window.clone() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => self.resized(size),
            WindowEvent::MouseInput { state: ElementState::Pressed, button: MouseButton::Left, .. } => {
                // Drag the mascot by any opaque pixel; click-through lets the rest of the window through before this.
                if let Err(e) = window.drag_window() {
                    println!("[FAILED]      drag: {e}");
                }
            }
            WindowEvent::KeyboardInput {
                event: KeyEvent { logical_key, state: ElementState::Pressed, .. }, ..
            } => match logical_key {
                Key::Named(NamedKey::Escape) => event_loop.exit(),
                Key::Named(NamedKey::ArrowUp) => {
                    self.opacity = (self.opacity + 0.1).min(1.0);
                    self.refresh_mask();
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.opacity = (self.opacity - 0.1).max(0.0);
                    self.refresh_mask();
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// The picture for the click-through mask at `size`, drawn by `render`, and the reason when it could not be. A size below
/// the picture's minimum, or a failed draw, gives a fully opaque scene instead: the old picture cannot be scaled to match
/// what the shader now draws, so a mask from it would accept and reject the wrong pixels. Every pixel then takes input,
/// which is wrong in the safe direction, and the caller reports why.
fn scene_for_size(
    size: (u32, u32),
    render: impl FnOnce((u32, u32)) -> Result<Scene, ReferenceError>,
) -> (Scene, Option<String>) {
    if size.0 < scene_pass::MIN_SIZE.0 || size.1 < scene_pass::MIN_SIZE.1 {
        let reason = format!("the picture needs at least {}x{}", scene_pass::MIN_SIZE.0, scene_pass::MIN_SIZE.1);
        return (Scene::opaque(size.0, size.1), Some(reason));
    }
    match render(size) {
        Ok(scene) => (scene, None),
        Err(e) => (Scene::opaque(size.0, size.1), Some(e.to_string())),
    }
}

/// The opacity the hit-test mask is built at: the display opacity, except that a degraded (fully opaque) mask must stay
/// fully opaque whatever the window is shown at, or an opacity of 0 would empty it and make the whole window click-through.
fn mask_opacity(open: bool, display: f32) -> f32 {
    if open { 1.0 } else { display }
}

/// The hit-test mask for `scene` in `window` at `opacity`.
fn mask<'a>(window: &Window, scene: &'a Scene, opacity: f32) -> Mask<'a> {
    let size = window.inner_size();
    Mask { fit: Fit::new((size.width, size.height), (scene.width, scene.height)), scene, opacity }
}

fn print_surface(renderer: &Renderer) {
    let r = renderer.report();
    println!("adapter: {} ({:?}, {:?}, driver {})", r.adapter, r.backend, r.device_type, r.driver);
    println!(
        "alpha modes offered: {:?}; using {:?} ({:?}); shader writes {:?}",
        r.offered, r.picked.mode, r.picked.transparency, r.output
    );
    println!(
        "surface format {:?}, present mode {:?}, GPU timestamps {}",
        r.format,
        r.present_mode,
        if r.gpu_timestamps { "on" } else { "off" }
    );
}

impl<T: 'static> ApplicationHandler<T> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        App::resumed(self, event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        App::window_event(self, event_loop, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.tick(event_loop, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{HitTestBackend, Level};

    fn config(fps: u32, seconds: Option<f64>) -> Config {
        Config {
            mode: Mode::Run,
            gpu: gpu::Options {
                backends: wgpu::Backends::empty(),
                alpha: None,
                output: None,
                fallback_adapter: false,
                dx12_visual: false,
                animate: false,
                timestamps: false,
            },
            platform: platform::Options {
                level: Level::Normal,
                hide_from_switcher: false,
                click_through: ClickThrough::Off,
                hit_test: HitTestBackend::Winit,
            },
            size: (1280, 720),
            fps,
            seconds,
            opacity: 1.0,
        }
    }

    #[test]
    fn a_finite_run_is_sized_for_every_frame_at_the_rate_the_loop_runs() {
        assert_eq!(expected_frames(&config(30, Some(60.0))), 1800 + 64);
        // The loop reads a zero rate as 1, so a 100 s run holds 100 frames, not the 64 spare slots alone.
        assert_eq!(expected_frames(&config(0, Some(100.0))), 100 + 64);
    }

    #[test]
    fn a_resize_gets_the_drawn_picture_when_it_can_be_drawn() {
        let drawn = Scene { width: 1280, height: 720, pixels: vec![7; 1280 * 720 * 4] };
        let (scene, degraded) = scene_for_size((1280, 720), |_| Ok(drawn.clone()));
        assert_eq!((scene, degraded), (drawn, None));
    }

    #[test]
    fn a_resize_below_the_minimum_or_a_failed_draw_opens_the_whole_window() {
        let (scene, degraded) = scene_for_size((800, 200), |_| panic!("a size below the minimum is never drawn"));
        assert_eq!(scene, Scene::opaque(800, 200));
        assert!(degraded.is_some_and(|reason| reason.contains("1024x256")));

        let failed = |_| Err(ReferenceError::Readback("lost".to_owned()));
        let (scene, degraded) = scene_for_size((1280, 720), failed);
        assert_eq!(scene, Scene::opaque(1280, 720));
        assert!(degraded.is_some_and(|reason| reason.contains("lost")));
    }

    #[test]
    fn a_degraded_mask_catches_every_pixel_at_any_display_opacity() {
        let scene = Scene::opaque(40, 30);
        let fit = Fit::new((40, 30), (40, 30));
        for display in [0.0, 0.02, 1.0] {
            let mask = Mask { fit, scene: &scene, opacity: mask_opacity(true, display) };
            assert_eq!(mask.input_rects().len(), 1, "one full-window rectangle at opacity {display}");
            assert!(mask.hit(5.5, 5.5));
        }
        assert_eq!(mask_opacity(false, 0.3), 0.3, "an ordinary mask follows the display opacity");
        let thin = Mask { fit, scene: &scene, opacity: mask_opacity(false, 0.0) };
        assert!(thin.input_rects().is_empty(), "without the override a degraded mask would vanish at opacity 0");
    }

    #[test]
    fn a_run_without_a_deadline_gets_a_fixed_ring() {
        assert_eq!(expected_frames(&config(30, None)), 4096);
    }
}
