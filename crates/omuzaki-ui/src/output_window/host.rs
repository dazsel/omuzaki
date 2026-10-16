//! [`OutputHost`]: the output window as the `output.window.*` ops drive it, shaped to the `WindowHost` contract of
//! `omuzaki_stream_api::window` (#2146): [`capabilities`](OutputHost::capabilities),
//! [`surface`](OutputHost::surface), [`open`](OutputHost::open), [`close`](OutputHost::close) and
//! [`state`](OutputHost::state), all on `&self` and callable from any thread.
//!
//! None of them waits for the render loop. `open` and `close` queue a request (the newest one wins); the UI thread
//! carries it out at the start of its next frame ([`apply`](OutputHost::apply)) and publishes what the window is
//! after drawing it ([`publish`](OutputHost::publish)). The lock is held only to copy a request or a snapshot, never
//! across a frame. When #2146 merges, `WindowHost` is implemented by forwarding to these methods, with its
//! `WindowPlan` and `WindowSettings` translated into an [`OutputPlan`].

use std::sync::{Arc, Mutex, PoisonError};

use super::plan::{OutputBackdrop, OutputPlan};
use super::window::OutputWindow;
use crate::aux_window::{PhysicalSize, Transparency};

/// What the host can do (`WindowCaps`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct HostCaps {
    /// It can make a transparent surface: only where #133 verified one ([`Transparency::Verified`]).
    pub transparent: bool,
    /// It can keep the window above others.
    pub always_on_top: bool,
}

/// The output and the display the window opens on (`OutputSurface`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct HostSurface {
    /// The output size in pixels.
    pub size: PhysicalSize,
    /// The scale factor of the display the window would open on: the main window's, as of its last frame.
    pub scale_factor: f64,
}

/// Whether the window is open, and its size and backdrop (`WindowState`).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum HostState {
    /// No window.
    Closed,
    /// The window is open.
    Open {
        /// Its client area in physical pixels: as measured once it has drawn, as planned before.
        physical: PhysicalSize,
        /// What it shows behind the character.
        backdrop: OutputBackdrop,
    },
}

/// Why the host refused to open the window (`WindowError::Host`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostError(pub String);

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "the window could not be opened: {}", self.0)
    }
}

impl std::error::Error for HostError {}

enum Request {
    Open(OutputPlan),
    Close,
}

struct Shared {
    pending: Option<Request>,
    state: HostState,
    surface: HostSurface,
}

/// The output window's host. Clones share one window.
#[derive(Clone)]
pub struct OutputHost {
    caps: HostCaps,
    shared: Arc<Mutex<Shared>>,
}

impl OutputHost {
    /// A host for an output of `output` pixels, on a platform where a transparent surface is `transparency`. Until
    /// the first frame publishes the display's scale, [`surface`](Self::surface) says 1.
    pub fn new(transparency: Transparency, output: PhysicalSize) -> Self {
        let caps = HostCaps { transparent: transparency == Transparency::Verified, always_on_top: true };
        let shared = Shared {
            pending: None,
            state: HostState::Closed,
            surface: HostSurface { size: output, scale_factor: 1.0 },
        };
        Self { caps, shared: Arc::new(Mutex::new(shared)) }
    }

    /// What this host can do. Constant for its life.
    pub fn capabilities(&self) -> HostCaps {
        self.caps
    }

    /// The output size and the scale of the display the window would open on, now.
    pub fn surface(&self) -> HostSurface {
        self.lock().surface
    }

    /// Queues the window to open as `plan` says, replacing an open one. The UI thread opens it on its next frame.
    ///
    /// # Errors
    /// [`HostError`] for a plan with a zero side.
    pub fn open(&self, plan: OutputPlan) -> Result<(), HostError> {
        if plan.size.width == 0 || plan.size.height == 0 {
            return Err(HostError(format!("the size {}x{} has a zero side", plan.size.width, plan.size.height)));
        }
        self.lock().pending = Some(Request::Open(plan));
        Ok(())
    }

    /// Queues the window to close. Closing a closed window does nothing.
    pub fn close(&self) {
        self.lock().pending = Some(Request::Close);
    }

    /// Whether the window is open, and its size and backdrop, as of the UI thread's last frame.
    pub fn state(&self) -> HostState {
        self.lock().state
    }

    /// UI thread, start of a frame: carries out the newest request on `window`.
    pub fn apply(&self, window: &mut OutputWindow) {
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

    /// UI thread, after drawing: publishes what `window` is, and the main window's display scale.
    pub fn publish(&self, window: &OutputWindow, scale_factor: f64) {
        let status = window.status();
        let state = if status.open {
            let plan = window.plan();
            HostState::Open { physical: status.measured.unwrap_or(plan.size), backdrop: plan.backdrop }
        } else {
            HostState::Closed
        };
        let mut shared = self.lock();
        shared.state = state;
        shared.surface.scale_factor = scale_factor;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Shared> {
        // Every write leaves the record whole, so a panic elsewhere can't leave it half-written.
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl std::fmt::Debug for OutputHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OutputHost").field("caps", &self.caps).field("state", &self.state()).finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output_window::DEFAULT_KEY;

    fn host() -> (OutputHost, OutputWindow) {
        let host = OutputHost::new(Transparency::Fallback, PhysicalSize::FULL_HD);
        (host, OutputWindow::new(OutputPlan::default(), Transparency::Fallback))
    }

    #[test]
    fn open_from_another_thread_waits_for_the_ui_frame_then_shows_as_open() {
        let (host, mut window) = host();
        let ops = host.clone();
        let plan = OutputPlan { size: PhysicalSize::new(1280, 720), ..OutputPlan::default() };
        std::thread::spawn(move || ops.open(plan)).join().expect("the op thread").expect("queued");
        assert_eq!(host.state(), HostState::Closed, "nothing changes until the UI thread runs");
        host.apply(&mut window);
        host.publish(&window, 1.25);
        let open =
            HostState::Open { physical: PhysicalSize::new(1280, 720), backdrop: OutputBackdrop::Key(DEFAULT_KEY) };
        assert_eq!(host.state(), open);
        assert!(window.status().open);
        assert_eq!(host.surface(), HostSurface { size: PhysicalSize::FULL_HD, scale_factor: 1.25 });
    }

    #[test]
    fn the_newest_request_wins_and_closing_closes() {
        let (host, mut window) = host();
        host.open(OutputPlan::default()).expect("queued");
        host.close();
        host.apply(&mut window);
        host.publish(&window, 1.0);
        assert_eq!(host.state(), HostState::Closed, "the close came last");
        host.open(OutputPlan::default()).expect("queued");
        host.apply(&mut window);
        host.close();
        host.close();
        host.apply(&mut window);
        host.publish(&window, 1.0);
        assert_eq!(host.state(), HostState::Closed);
        assert!(!window.status().open);
    }

    #[test]
    fn a_zero_size_is_refused_and_transparency_follows_the_verified_list() {
        let (host, _) = host();
        let empty = OutputPlan { size: PhysicalSize::new(0, 1080), ..OutputPlan::default() };
        assert!(host.open(empty).is_err());
        assert!(!host.capabilities().transparent);
        let verified = OutputHost::new(Transparency::Verified, PhysicalSize::FULL_HD);
        assert!(verified.capabilities().transparent);
    }
}
