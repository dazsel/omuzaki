//! The Windows probe: `GetCursorPos` for the pointer (physical pixels for a per-monitor DPI aware process, which the
//! studio is) and `GetAsyncKeyState` for the chord.
#![allow(
    unsafe_code,
    reason = "two Win32 queries: one returns a key's state, one writes a POINT into a local it is handed"
)]

use egui::Pos2;
use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_CONTROL, VK_MENU};
use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;

use super::DesktopProbe;

/// The M key's virtual-key code.
const VK_M: i32 = 0x4D;

/// Polls the pointer and the keys.
#[derive(Debug)]
pub struct WinProbe;

fn down(key: i32) -> bool {
    // SAFETY: GetAsyncKeyState takes a key code and returns the key's state; it reads no memory of ours.
    let state = unsafe { GetAsyncKeyState(key) };
    // The most significant bit is set while the key is down.
    state < 0
}

impl DesktopProbe for WinProbe {
    fn cursor(&self) -> Option<Pos2> {
        let mut at = POINT { x: 0, y: 0 };
        // SAFETY: `at` is a valid POINT for the call to write.
        let ok = unsafe { GetCursorPos(&raw mut at) };
        #[allow(clippy::cast_precision_loss)] // desktop coordinates are far below 2^24
        (ok != 0).then(|| Pos2::new(at.x as f32, at.y as f32))
    }

    fn way_back_held(&self) -> bool {
        down(i32::from(VK_CONTROL)) && down(i32::from(VK_MENU)) && down(VK_M)
    }
}
