//! The X11 probe: `QueryPointer` on the root window for the pointer, `QueryKeymap` for the chord. One connection of
//! its own, opened when a desktop character first needs it; `None` without an X server (a Wayland session without its X
//! server, a headless machine).

use egui::Pos2;
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{ConnectionExt as _, Keycode, Window};
use x11rb::rust_connection::RustConnection;

use super::DesktopProbe;

/// `XK_Control_L`, `XK_Control_R`, `XK_Alt_L`, `XK_Alt_R`, `XK_m` and `XK_M`.
const CONTROL: [u32; 2] = [0xffe3, 0xffe4];
const ALT: [u32; 2] = [0xffe9, 0xffea];
const M: [u32; 2] = [0x006d, 0x004d];

/// The connection, its root window and the keycodes of the chord's keys on this keyboard.
#[derive(Debug)]
pub struct X11Probe {
    conn: RustConnection,
    root: Window,
    control: Vec<Keycode>,
    alt: Vec<Keycode>,
    m: Vec<Keycode>,
}

impl X11Probe {
    /// Connects to the display `DISPLAY` names; `None` (logged at debug) when there is none.
    pub fn connect() -> Option<Self> {
        let (conn, screen) = x11rb::connect(None)
            .inspect_err(|error| tracing::debug!(%error, "no X display: per-pixel click-through is off"))
            .ok()?;
        let root = conn.setup().roots.get(screen)?.root;
        let (min, max) = (conn.setup().min_keycode, conn.setup().max_keycode);
        let map = conn.get_keyboard_mapping(min, max.saturating_sub(min).saturating_add(1)).ok()?.reply().ok()?;
        let per = usize::from(map.keysyms_per_keycode.max(1));
        let codes = |wanted: &[u32]| -> Vec<Keycode> {
            map.keysyms
                .chunks(per)
                .enumerate()
                .filter(|(_, syms)| syms.iter().any(|sym| wanted.contains(sym)))
                .filter_map(|(i, _)| u8::try_from(i).ok().and_then(|i| min.checked_add(i)))
                .collect()
        };
        let (control, alt, m) = (codes(&CONTROL), codes(&ALT), codes(&M));
        Some(Self { conn, root, control, alt, m })
    }
}

impl DesktopProbe for X11Probe {
    fn cursor(&self) -> Option<Pos2> {
        let reply = self.conn.query_pointer(self.root).ok()?.reply().ok()?;
        Some(Pos2::new(f32::from(reply.root_x), f32::from(reply.root_y)))
    }

    fn way_back_held(&self) -> bool {
        let Some(keys) = self.conn.query_keymap().ok().and_then(|cookie| cookie.reply().ok()) else { return false };
        let down =
            |code: &Keycode| keys.keys.get(usize::from(*code / 8)).is_some_and(|byte| byte & (1 << (code % 8)) != 0);
        self.control.iter().any(down) && self.alt.iter().any(down) && self.m.iter().any(down)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The probe reads the pointer where the server put it, and no chord is held on a fresh server.
    /// `xvfb-run cargo test -p omuzaki-ui --lib hit_test::x11 -- --ignored`.
    #[test]
    #[ignore = "needs an X server (xvfb-run)"]
    fn the_probe_reads_the_pointer_and_the_keys_from_the_server() {
        let probe = X11Probe::connect().expect("an X server");
        assert!(!probe.control.is_empty() && !probe.alt.is_empty() && !probe.m.is_empty(), "{probe:?}");
        probe.conn.warp_pointer(x11rb::NONE, probe.root, 0, 0, 0, 0, 120, 45).expect("warp").check().expect("warped");
        assert_eq!(probe.cursor(), Some(Pos2::new(120.0, 45.0)));
        assert!(!probe.way_back_held());
    }
}
