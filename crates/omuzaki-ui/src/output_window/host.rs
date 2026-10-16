//! [`OutputHost`]: the output window as the `output.window.*` ops drive it, the Live UI's implementation of
//! `omuzaki_stream_api::window::WindowHost` (#298).
//!
//! The ops run on whatever thread executes them, and the OS window belongs to the UI thread, so none of the methods
//! waits for the render loop. `open` and `close` queue a request (the newest one wins) and return at once, as the
//! contract allows; the UI thread carries it out at the start of its next frame ([`take_requests`](OutputHost::take_requests))
//! and publishes what the window is after drawing it ([`publish`](OutputHost::publish)), so `state` reports `Open`
//! from that frame on. The lock is held only to copy a request or a snapshot, never across a frame.

use std::sync::{Arc, Mutex, PoisonError};

use egui::Color32;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::UVec2;
use omuzaki_stream_api::window::{
    Backdrop, OutputSurface, WindowCaps, WindowError, WindowHost, WindowPlan, WindowSettings, WindowState,
};

use super::plan::{DEFAULT_FPS, DEFAULT_KEY, OutputBackdrop, OutputPlan};
use super::window::OutputWindow;
use crate::aux_window::{PhysicalSize, Transparency};

enum Request {
    Open(OutputPlan),
    Close,
}

struct Shared {
    pending: Option<Request>,
    state: WindowState,
    surface: OutputSurface,
}

/// The output window's host. Clones share one window.
#[derive(Clone)]
pub struct OutputHost {
    caps: WindowCaps,
    shared: Arc<Mutex<Shared>>,
}

impl OutputHost {
    /// A host for an output of `output` pixels, on a platform where a transparent surface is `transparency`. It can
    /// keep the window on top everywhere (an egui window level). Until the first frame publishes the display's scale,
    /// [`surface`](WindowHost::surface) says 1.
    pub fn new(transparency: Transparency, output: PhysicalSize) -> Self {
        let caps = WindowCaps::new(transparency == Transparency::Verified, true);
        let shared = Shared {
            pending: None,
            state: WindowState::Closed,
            surface: OutputSurface::new(UVec2::new(output.width, output.height), 1.0),
        };
        Self { caps, shared: Arc::new(Mutex::new(shared)) }
    }

    /// UI thread, start of a frame: carries out the newest request on `window`.
    pub fn take_requests(&self, window: &mut OutputWindow) {
        let request = self.lock().pending.take();
        match request {
            Some(Request::Open(plan)) => {
                window.set_plan(plan);
                window.set_open(true);
            }
            Some(Request::Close) => window.set_open(false),
            None => {}
        }
    }

    /// UI thread, after drawing: publishes what `window` is (its measured size once it has drawn, its planned size
    /// before) and the main window's display scale.
    pub fn publish(&self, window: &OutputWindow, scale_factor: f64) {
        let status = window.status();
        let state = if status.open {
            let plan = window.plan();
            let physical = status.measured.unwrap_or(plan.size);
            WindowState::open(UVec2::new(physical.width, physical.height), backdrop_of(plan.backdrop))
                .with_minimised(status.minimised)
        } else {
            WindowState::Closed
        };
        let mut shared = self.lock();
        shared.state = state;
        shared.surface = OutputSurface::new(shared.surface.size, scale_factor);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        // Every write leaves the record whole, so a panic elsewhere can't leave it half-written.
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl WindowHost for OutputHost {
    fn capabilities(&self) -> WindowCaps {
        self.caps
    }

    fn surface(&self) -> OutputSurface {
        self.lock().surface
    }

    fn open(&self, plan: &WindowPlan, settings: &WindowSettings) -> Result<(), WindowError> {
        let size = PhysicalSize::new(plan.physical.x, plan.physical.y);
        if size.width == 0 || size.height == 0 {
            return Err(WindowError::Host(format!("the size {}x{} has a zero side", size.width, size.height)));
        }
        self.lock().pending = Some(Request::Open(output_plan(size, settings)));
        Ok(())
    }

    fn close(&self) {
        self.lock().pending = Some(Request::Close);
    }

    fn state(&self) -> WindowState {
        self.lock().state
    }
}

impl std::fmt::Debug for OutputHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputHost").field("caps", &self.caps).field("state", &self.state()).finish_non_exhaustive()
    }
}

/// The window the ops asked for: `size` pixels, with the settings' title, backdrop, cursor and stacking.
fn output_plan(size: PhysicalSize, settings: &WindowSettings) -> OutputPlan {
    let backdrop = match settings.backdrop {
        Backdrop::Key(key) => OutputBackdrop::Key(colour32(key)),
        // The ops pass `Transparent` only to a host that verified it; the key stays the fallback all the same.
        Backdrop::Transparent => OutputBackdrop::Transparent { fallback: DEFAULT_KEY },
    };
    OutputPlan {
        size,
        backdrop,
        title: settings.title.clone(),
        hide_cursor: settings.hide_cursor,
        always_on_top: settings.always_on_top,
        fps: DEFAULT_FPS,
    }
}

/// The contract's backdrop for what the window shows.
fn backdrop_of(backdrop: OutputBackdrop) -> Backdrop {
    match backdrop {
        OutputBackdrop::Key(key) => Backdrop::Key(Colour::rgba(key.r(), key.g(), key.b(), key.a())),
        OutputBackdrop::Transparent { .. } => Backdrop::Transparent,
    }
}

/// An sRGB colour as egui holds it. A key is opaque; a translucent one is premultiplied as egui expects.
fn colour32(colour: Colour) -> Color32 {
    Color32::from_rgba_unmultiplied(colour.r, colour.g, colour.b, colour.a)
}

#[cfg(test)]
mod tests {
    use omuzaki_stream_api::window::DEFAULT_TITLE;

    use super::*;

    fn host() -> (OutputHost, OutputWindow) {
        let host = OutputHost::new(Transparency::Fallback, PhysicalSize::FULL_HD);
        (host, OutputWindow::new(OutputPlan::default(), Transparency::Fallback))
    }

    fn plan(width: u32, height: u32) -> WindowPlan {
        WindowPlan::new(UVec2::new(width, height), (f64::from(width), f64::from(height)), 1.0)
    }

    #[test]
    fn open_from_another_thread_waits_for_the_ui_frame_then_shows_as_open() {
        let (host, mut window) = host();
        let ops = host.clone();
        let mut settings = WindowSettings::default();
        settings.always_on_top = true;
        std::thread::spawn(move || ops.open(&plan(1280, 720), &settings))
            .join()
            .expect("the op thread")
            .expect("queued");
        assert_eq!(host.state(), WindowState::Closed, "nothing changes until the UI thread runs");
        host.take_requests(&mut window);
        host.publish(&window, 1.25);
        let key = Backdrop::Key(omuzaki_stream_api::window::DEFAULT_KEY);
        assert_eq!(host.state(), WindowState::open(UVec2::new(1280, 720), key));
        assert!(window.status().open);
        assert_eq!(window.plan().title, DEFAULT_TITLE, "the contract's title");
        assert!(window.plan().always_on_top && window.plan().hide_cursor, "the settings reach the window");
        assert_eq!(host.surface(), OutputSurface::new(UVec2::new(1920, 1080), 1.25));
    }

    #[test]
    fn the_newest_request_wins_and_closing_closes() {
        let (host, mut window) = host();
        let settings = WindowSettings::default();
        host.open(&plan(1920, 1080), &settings).expect("queued");
        host.close();
        host.take_requests(&mut window);
        host.publish(&window, 1.0);
        assert_eq!(host.state(), WindowState::Closed, "the close came last");
        host.open(&plan(1920, 1080), &settings).expect("queued");
        host.take_requests(&mut window);
        host.close();
        host.close();
        host.take_requests(&mut window);
        host.publish(&window, 1.0);
        assert_eq!(host.state(), WindowState::Closed);
        assert!(!window.status().open);
    }

    #[test]
    fn a_zero_size_is_refused_and_transparency_follows_the_verified_list() {
        let (host, _) = host();
        let refused = host.open(&plan(0, 1080), &WindowSettings::default());
        assert!(matches!(refused, Err(WindowError::Host(_))), "{refused:?}");
        assert_eq!(host.capabilities(), WindowCaps::new(false, true));
        let verified = OutputHost::new(Transparency::Verified, PhysicalSize::FULL_HD);
        assert!(verified.capabilities().transparent);
    }

    #[test]
    fn a_transparent_window_reports_transparent_and_a_key_its_colour() {
        let host = OutputHost::new(Transparency::Verified, PhysicalSize::FULL_HD);
        let mut window = OutputWindow::new(OutputPlan::default(), Transparency::Verified);
        let mut settings = WindowSettings::default();
        settings.backdrop = Backdrop::Transparent;
        host.open(&plan(1920, 1080), &settings).expect("queued");
        host.take_requests(&mut window);
        host.publish(&window, 1.0);
        assert_eq!(host.state(), WindowState::open(UVec2::new(1920, 1080), Backdrop::Transparent));
        let magenta = Colour::rgb(255, 0, 255);
        settings.backdrop = Backdrop::Key(magenta);
        host.open(&plan(1920, 1080), &settings).expect("queued");
        host.take_requests(&mut window);
        host.publish(&window, 1.0);
        assert_eq!(host.state(), WindowState::open(UVec2::new(1920, 1080), Backdrop::Key(magenta)));
    }
}
