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
use crate::stats::FrameStats;

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
}

/// Runs the spike to completion.
///
/// # Errors
/// See [`RunError`].
pub fn run(config: Config) -> Result<(), RunError> {
    let mut builder = EventLoop::builder();
    platform::configure_event_loop(&mut builder, &config.platform);
    let event_loop = builder.build()?;
    let mut app = App::new(config);
    event_loop.run_app(&mut app)?;
    app.failure.map_or(Ok(()), Err)
}

struct App {
    config: Config,
    scene: Scene,
    window: Option<Arc<Window>>,
    renderer: Option<Renderer>,
    platform: Option<platform::State>,
    stats: FrameStats,
    opacity: f32,
    started: Instant,
    next_frame: Instant,
    last_report: Instant,
    presented: u32,
    failure: Option<RunError>,
}

impl App {
    fn new(config: Config) -> Self {
        let now = Instant::now();
        Self {
            opacity: config.opacity,
            config,
            scene: Scene::generate(),
            window: None,
            renderer: None,
            platform: None,
            stats: FrameStats::default(),
            started: now,
            next_frame: now,
            last_report: now,
            presented: 0,
            failure: None,
        }
    }

    fn fail(&mut self, event_loop: &ActiveEventLoop, error: RunError) {
        self.failure = Some(error);
        event_loop.exit();
    }

    fn fit(&self, window: &Window) -> Fit {
        let size = window.inner_size();
        Fit::new((size.width, size.height), (self.scene.width, self.scene.height))
    }

    fn start(&mut self, event_loop: &ActiveEventLoop) -> Result<(), RunError> {
        let attributes = platform::window_attributes(self.config.size, &self.config.platform);
        let window = Arc::new(event_loop.create_window(attributes)?);
        let fit = self.fit(&window);
        let (state, features) = platform::State::new(&window, self.config.platform, &fit, &self.scene);
        println!("window: {}x{} physical px, scale factor {}", fit.window.0, fit.window.1, window.scale_factor());
        for feature in &features {
            println!("{feature}");
        }
        match self.config.mode {
            Mode::Run => {
                let renderer = Renderer::new(&window, &self.config.gpu, &self.scene)?;
                print_surface(&renderer);
                self.renderer = Some(renderer);
            }
            Mode::Probe => {
                for line in state.describe(&window) {
                    println!("{line}");
                }
                event_loop.exit();
            }
            Mode::Caps => {
                for line in gpu::probe_backends(&window, &self.config.gpu) {
                    println!("{line}");
                }
                event_loop.exit();
            }
        }
        self.platform = Some(state);
        self.window = Some(window);
        Ok(())
    }

    fn draw(&mut self, event_loop: &ActiveEventLoop) {
        let Some(renderer) = self.renderer.as_mut() else { return };
        let begin = Instant::now();
        match renderer.render(self.opacity) {
            Ok(Frame::Presented) => {
                self.stats.record(begin.elapsed());
                self.presented += 1;
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
        if let Some(s) = self.stats.take_summary() {
            println!(
                "frames: {} presented in {:.1}s = {:.1} fps; render() CPU ms mean {:.2}, p95 {:.2}, max {:.2}",
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
}

fn print_surface(renderer: &Renderer) {
    let r = renderer.report();
    println!("adapter: {} ({:?}, {:?})", r.adapter, r.backend, r.device_type);
    println!(
        "alpha modes offered: {:?}; using {:?} ({:?}); shader writes {:?}",
        r.offered, r.picked.mode, r.picked.transparency, r.output
    );
    println!("surface format {:?}, present mode {:?}", r.format, r.present_mode);
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        if let Err(e) = self.start(event_loop) {
            self.fail(event_loop, e);
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(window) = self.window.clone() else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(renderer) = self.renderer.as_mut() {
                    renderer.resize(size.width, size.height);
                }
                let fit = self.fit(&window);
                if let Some(feature) = self.platform.as_mut().and_then(|p| p.resized(&window, &fit, &self.scene)) {
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

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(window) = self.window.clone() else { return };
        if self.config.mode != Mode::Run {
            return;
        }
        if self.config.seconds.is_some_and(|s| self.started.elapsed().as_secs_f64() >= s) {
            event_loop.exit();
            return;
        }
        let fit = self.fit(&window);
        if let Some(state) = self.platform.as_mut() {
            state.tick(&window, &fit, &self.scene);
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
        let poll = now + Duration::from_millis(16);
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame.min(poll)));
    }
}
