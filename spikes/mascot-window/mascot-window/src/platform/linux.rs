//! Linux: X11 through our own x11rb connection (input shape, EWMH states) and Wayland through winit alone.

use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use winit::event_loop::EventLoopBuilder;
use winit::window::{Window, WindowAttributes};
use x11rb::connection::Connection as _;
use x11rb::protocol::shape::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

use super::{ClickThrough, Feature, Level, Options};
use crate::scene::Mask;

pub(super) fn attributes(attributes: WindowAttributes, _options: Options) -> WindowAttributes {
    attributes
}

pub(super) fn configure_event_loop<T: 'static>(_builder: &mut EventLoopBuilder<T>, _options: Options) {}

/// Whether the window lives on a Wayland compositor.
pub(super) fn is_wayland(window: &Window) -> bool {
    matches!(window.window_handle().map(|h| h.as_raw()), Ok(RawWindowHandle::Wayland(_)))
}

fn x11_window(window: &Window) -> Option<u32> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(h) => u32::try_from(h.window).ok(),
        RawWindowHandle::Xcb(h) => Some(h.window.get()),
        _ => None,
    }
}

/// A second connection to the X server, used only to talk about our own window.
struct X11 {
    conn: RustConnection,
    root: u32,
    window: u32,
}

#[derive(Debug, thiserror::Error)]
enum X11Error {
    #[error("connecting to the X server: {0}")]
    Connect(#[from] x11rb::errors::ConnectError),
    #[error("X request: {0}")]
    Connection(#[from] x11rb::errors::ConnectionError),
    #[error("X reply: {0}")]
    Reply(#[from] x11rb::errors::ReplyError),
    #[error("a window larger than 65535 pixels cannot have an X input shape")]
    TooLarge,
}

impl X11 {
    fn open(window: u32) -> Result<Self, X11Error> {
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen].root;
        Ok(Self { conn, root, window })
    }

    fn atom(&self, name: &str) -> Result<u32, X11Error> {
        Ok(self.conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
    }

    /// Asks the window manager (EWMH `_NET_WM_STATE` client message) to add up to two states.
    fn add_states(&self, first: &str, second: Option<&str>) -> Result<(), X11Error> {
        let state = self.atom("_NET_WM_STATE")?;
        let second = second.map_or(Ok(0), |s| self.atom(s))?;
        let message = xproto::ClientMessageEvent::new(32, self.window, state, [1, self.atom(first)?, second, 1, 0]);
        let mask = xproto::EventMask::SUBSTRUCTURE_NOTIFY | xproto::EventMask::SUBSTRUCTURE_REDIRECT;
        // `check` waits for the server's verdict: a bare request only fails later, as an unread error event.
        self.conn.send_event(false, self.root, mask, message)?.check()?;
        Ok(())
    }

    /// Replaces the window's input shape: pointer events outside `rects` fall through to the window below.
    fn set_input_shape(&self, rects: &[crate::scene::Rect]) -> Result<(), X11Error> {
        let rectangles = rects
            .iter()
            .map(|r| {
                Ok(xproto::Rectangle {
                    x: i16::try_from(r.x).map_err(|_| X11Error::TooLarge)?,
                    y: i16::try_from(r.y).map_err(|_| X11Error::TooLarge)?,
                    width: u16::try_from(r.w).map_err(|_| X11Error::TooLarge)?,
                    height: u16::try_from(r.h).map_err(|_| X11Error::TooLarge)?,
                })
            })
            .collect::<Result<Vec<_>, X11Error>>()?;
        self.conn
            .shape_rectangles(
                shape::SO::SET,
                shape::SK::INPUT,
                xproto::ClipOrdering::UNSORTED,
                self.window,
                0,
                0,
                &rectangles,
            )?
            .check()?;
        Ok(())
    }

    fn describe(&self) -> Result<Vec<String>, X11Error> {
        let mut lines = Vec::new();
        let geometry = self.conn.get_geometry(self.window)?.reply()?;
        lines.push(format!("x11: window depth {} (32 means an ARGB visual)", geometry.depth));
        let shape = self.conn.shape_get_rectangles(self.window, shape::SK::INPUT)?.reply()?;
        lines.push(format!("x11: input shape has {} rectangles", shape.rectangles.len()));
        let state = self.atom("_NET_WM_STATE")?;
        let reply = self.conn.get_property(false, self.window, state, xproto::AtomEnum::ATOM, 0, 64)?.reply()?;
        let names = reply
            .value32()
            .into_iter()
            .flatten()
            .map(|atom| {
                let name = self.conn.get_atom_name(atom)?.reply()?.name;
                Ok(String::from_utf8_lossy(&name).into_owned())
            })
            .collect::<Result<Vec<_>, X11Error>>()?;
        lines.push(format!(
            "x11: _NET_WM_STATE property {names:?} (a window manager adds states when it honours the request)"
        ));
        Ok(lines)
    }
}

// The `State` methods have the same shape on every OS, so on X11 some take `&self` without using it.
pub(super) struct State {
    x11: Option<X11>,
}

impl State {
    pub(super) fn new(window: &Window, options: Options, mask: &Mask<'_>) -> (Self, Vec<Feature>) {
        let mut features = Vec::new();
        let Some(id) = x11_window(window) else {
            features
                .push(Feature::unsupported("hide_from_switcher", "Wayland: the compositor decides; no client request"));
            if options.click_through == ClickThrough::Pixel {
                features.push(Feature::unsupported(
                    "click_through_pixel",
                    "winit can only set an empty Wayland input region; a mask needs wl_compositor.create_region on the raw surface",
                ));
            }
            return (Self { x11: None }, features);
        };
        let x11 = match X11::open(id) {
            Ok(x) => x,
            Err(e) => {
                features.push(Feature::failed("x11_connection", e));
                return (Self { x11: None }, features);
            }
        };
        if options.hide_from_switcher {
            features.push(match x11.add_states("_NET_WM_STATE_SKIP_TASKBAR", Some("_NET_WM_STATE_SKIP_PAGER")) {
                Ok(()) => Feature::applied("hide_from_switcher", "_NET_WM_STATE_SKIP_TASKBAR and SKIP_PAGER requested"),
                Err(e) => Feature::failed("hide_from_switcher", e),
            });
        }
        let state = match options.level {
            Level::Top => Some("_NET_WM_STATE_ABOVE"),
            Level::Bottom => Some("_NET_WM_STATE_BELOW"),
            Level::Normal => None,
        };
        if let Some(name) = state
            && let Err(e) = x11.add_states(name, None)
        {
            features.push(Feature::failed("window_level_ewmh", e));
        }
        let mut state = Self { x11: Some(x11) };
        if options.click_through == ClickThrough::Pixel {
            features.push(state.apply_shape(mask));
        }
        (state, features)
    }

    fn apply_shape(&mut self, mask: &Mask<'_>) -> Feature {
        let Some(x11) = &self.x11 else { return Feature::unsupported("click_through_pixel", "no X11 connection") };
        let rects = mask.input_rects();
        match x11.set_input_shape(&rects) {
            Ok(()) => {
                Feature::applied("click_through_pixel", format!("XShape input shape of {} rectangles", rects.len()))
            }
            Err(e) => Feature::failed("click_through_pixel", e),
        }
    }

    /// X11 needs no cursor polling: the server applies the input shape.
    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(super) fn polls_cursor(&self) -> bool {
        false
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(super) fn cursor_position(&self, _window: &Window) -> Option<(f64, f64)> {
        None
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(super) fn set_click_through(
        &self,
        window: &Window,
        _options: Options,
        through: bool,
    ) -> Result<String, String> {
        window
            .set_cursor_hittest(!through)
            .map(|()| {
                format!(
                    "set_cursor_hittest({}) (X11: empty or full input shape; Wayland: empty or default input region)",
                    !through
                )
            })
            .map_err(|e| e.to_string())
    }

    #[allow(clippy::unused_self, clippy::unnecessary_wraps)]
    pub(super) fn refresh(&mut self, _window: &Window, mask: &Mask<'_>) -> Option<Feature> {
        Some(self.apply_shape(mask))
    }

    pub(super) fn describe(&self, _window: &Window) -> Vec<String> {
        match &self.x11 {
            Some(x11) => x11.describe().unwrap_or_else(|e| vec![format!("x11: reading back failed: {e}")]),
            None => vec!["wayland: nothing to read back from the client side".to_owned()],
        }
    }
}

/// CPU time this process has used so far (user plus system, all threads).
pub(super) fn process_cpu_seconds() -> Option<f64> {
    // /proc/self/stat fields 14 and 15 (utime, stime) in clock ticks; the command name in field 2 may hold spaces, so
    // count from the closing parenthesis.
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = stat.rsplit_once(')')?.1;
    let mut fields = rest.split_whitespace().skip(11);
    let user: f64 = fields.next()?.parse().ok()?;
    let system: f64 = fields.next()?.parse().ok()?;
    // The kernel's USER_HZ is 100 on every Linux ABI that matters here.
    Some((user + system) / 100.0)
}
