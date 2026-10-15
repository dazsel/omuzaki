//! The winit application: creates the window, applies the platform features, draws at a fixed rate and reports.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use crate::gpu::{self, Frame, Renderer};
use crate::platform;
use crate::scene::{Fit, Scene};
use crate::scene_pass::{self, ReferenceError};
use crate::stats::{Series, Summary};

/// What the run does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Open the window and draw until closed or the time is up.
    Run,
    /// Open the window, apply the platform features, read the OS state back and exit. No GPU is touched.
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
    /// CPU seconds the process used over that time, where the platform can say.
    pub cpu_seconds: Option<f64>,
    /// CPU time inside `render` (acquire, encode, submit, present).
    pub render: Option<Summary>,
    /// GPU time per frame from timestamp queries, where the adapter has them.
    pub gpu: Option<Summary>,
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
                "; GPU ms mean {:.2}, p95 {:.2}, max {:.2} ({} frames timed)",
                s.mean_ms, s.p95_ms, s.max_ms, s.frames
            ),
            None => write!(f, "; GPU time not measured"),
        }
    }
}

impl Outcome {
    /// Process CPU use as a percentage of one core.
    pub fn cpu_percent(&self) -> Option<f64> {
        self.cpu_seconds.filter(|_| self.seconds > 0.0).map(|cpu| 100.0 * cpu / self.seconds)
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
    started: Instant,
    next_frame: Instant,
    last_report: Instant,
    presented: u32,
    total_presented: u32,
    first_frame: Option<(Instant, Option<f64>)>,
    last_frame: Option<(Instant, Option<f64>)>,
    failure: Option<RunError>,
}

impl App {
    /// An application that has not opened its window yet.
    pub fn new(config: Config) -> Self {
        let now = Instant::now();
        Self {
            opacity: config.opacity,
            config,
            scene: None,
            window: None,
            renderer: None,
            platform: None,
            render_ms: Series::default(),
            gpu_ms: Series::default(),
            started: now,
            next_frame: now,
            last_report: now,
            presented: 0,
            total_presented: 0,
            first_frame: None,
            last_frame: None,
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
        let (seconds, cpu_seconds) = match (self.first_frame, self.last_frame) {
            (Some((t0, c0)), Some((t1, c1))) => ((t1 - t0).as_secs_f64(), c0.zip(c1).map(|(a, b)| b - a)),
            _ => (0.0, None),
        };
        Ok(Outcome {
            presented: self.total_presented,
            seconds,
            cpu_seconds,
            render: self.render_ms.overall(),
            gpu: self.gpu_ms.overall(),
        })
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: RunError) {
        self.failure = Some(error);
        event_loop.exit();
    }

    fn fit(window: &Window, scene: &Scene) -> Fit {
        let size = window.inner_size();
        Fit::new((size.width, size.height), (scene.width, scene.height))
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
        // The click-through mask comes from drawing the picture once on a device of its own, at the window's size.
        let size = window.inner_size();
        let scene = scene_pass::render_reference(
            self.config.gpu.backends,
            self.config.gpu.fallback_adapter,
            (size.width, size.height),
        )?;
        let fit = Self::fit(&window, &scene);
        let (state, features) = platform::State::new(&window, self.config.platform, &fit, &scene);
        println!("window: {}x{} physical px, scale factor {}", fit.window.0, fit.window.1, window.scale_factor());
        for feature in &features {
            println!("{feature}");
        }
        if self.config.mode == Mode::Probe {
            for line in state.describe(&window) {
                println!("{line}");
            }
            event_loop.exit();
        } else {
            let renderer = Renderer::new(&window, &self.config.gpu)?;
            print_surface(&renderer);
            self.renderer = Some(renderer);
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
                self.gpu_ms.extend_ms(renderer.take_gpu_times());
                self.presented += 1;
                self.total_presented += 1;
                let now = (Instant::now(), timing_cpu());
                self.first_frame.get_or_insert(now);
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
        let size = window.inner_size();
        let fit = Fit::new((size.width, size.height), (scene.width, scene.height));
        if let Some(state) = self.platform.as_mut() {
            state.tick(&window, &fit, scene);
            if let Some(e) = state.take_poll_error() {
                println!("[FAILED]      click_through_pixel toggle: {e}");
            }
        }
        let now = Instant::now();
        let interval = Duration::from_secs_f64(1.0 / f64::from(self.config.fps.max(1)));
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

    /// Handles an event for this application's window.
    pub fn window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        let (Some(window), Some(scene)) = (self.window.clone(), self.scene.as_ref()) else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size.width, size.height);
                }
                let fit = Fit::new((size.width, size.height), (scene.width, scene.height));
                if let Some(feature) = self.platform.as_mut().and_then(|p| p.resized(&window, &fit, scene)) {
                    println!("{feature}");
                }
            }
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
                Key::Named(NamedKey::ArrowUp) => self.opacity = (self.opacity + 0.1).min(1.0),
                Key::Named(NamedKey::ArrowDown) => self.opacity = (self.opacity - 0.1).max(0.0),
                _ => {}
            },
            _ => {}
        }
    }
}

fn timing_cpu() -> Option<f64> {
    platform::process_cpu_seconds()
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
