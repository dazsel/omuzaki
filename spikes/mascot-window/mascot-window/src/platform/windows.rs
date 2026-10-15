//! Windows: winit for the window, the `windows` crate for the switcher, click-through and the global cursor.

#![allow(unsafe_code)]
// The extended-style flags are small bit masks, far below `isize::MAX`. The `State` methods have the same shape on every
// OS, so on Windows some take `&self` without using it.
#![allow(clippy::cast_possible_wrap, clippy::unused_self)]

use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};
use windows::Win32::Foundation::{COLORREF, GetLastError, HWND, POINT, SetLastError, WIN32_ERROR};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW,
    WS_EX_APPWINDOW, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TRANSPARENT,
};
use winit::event_loop::EventLoopBuilder;
use winit::platform::windows::WindowAttributesExtWindows as _;
use winit::window::{Window, WindowAttributes};

use super::{Feature, HitTestBackend, Options};
use crate::scene::Mask;

pub(super) fn attributes(attributes: WindowAttributes, options: Options) -> WindowAttributes {
    // WS_EX_NOREDIRECTIONBITMAP: the window has no GDI surface, only the DirectComposition visual wgpu attaches.
    attributes.with_no_redirection_bitmap(true).with_skip_taskbar(options.hide_from_switcher)
}

pub(super) fn configure_event_loop<T: 'static>(_builder: &mut EventLoopBuilder<T>, _options: Options) {}

pub(super) fn is_wayland(_window: &Window) -> bool {
    false
}

fn hwnd(window: &Window) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
        _ => None,
    }
}

/// Reads the extended style, applies `edit`, writes it back, and reports a refusal by Windows.
fn edit_ex_style(hwnd: HWND, edit: impl FnOnce(isize) -> isize) -> Result<(), String> {
    // SAFETY: `hwnd` is the live window winit created on this thread; Get/SetWindowLongPtrW only read and write its
    // style word.
    unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        // SetWindowLongPtrW returns the previous value, and 0 is a valid one, so a failure is told apart by clearing the
        // thread's last error before the call and reading it after.
        SetLastError(WIN32_ERROR(0));
        let previous = SetWindowLongPtrW(hwnd, GWL_EXSTYLE, edit(style));
        let error = GetLastError();
        if previous == 0 && error.0 != 0 {
            return Err(format!("SetWindowLongPtrW failed with error {}", error.0));
        }
    }
    Ok(())
}

/// `WS_EX_TOOLWINDOW` removes the window from Alt-Tab (winit's `skip_taskbar` only hides the taskbar button).
fn hide_from_switcher(hwnd: HWND) -> Result<(), String> {
    edit_ex_style(hwnd, |s| (s | WS_EX_TOOLWINDOW.0 as isize) & !(WS_EX_APPWINDOW.0 as isize))
}

pub(super) struct State;

impl State {
    pub(super) fn new(window: &Window, options: Options, _mask: &Mask<'_>) -> (Self, Vec<Feature>) {
        let mut features = Vec::new();
        if options.hide_from_switcher {
            features.push(match hwnd(window).map(hide_from_switcher) {
                Some(Ok(())) => Feature::applied(
                    "hide_from_switcher",
                    "with_skip_taskbar(true) and WS_EX_TOOLWINDOW (check Alt-Tab and the taskbar)",
                ),
                Some(Err(e)) => Feature::failed("hide_from_switcher", e),
                None => Feature::failed("hide_from_switcher", "no Win32 window handle"),
            });
        }
        (Self, features)
    }

    /// Windows needs the global cursor to drive per-pixel click-through.
    pub(super) fn polls_cursor(&self) -> bool {
        true
    }

    pub(super) fn cursor_position(&self, _window: &Window) -> Option<(f64, f64)> {
        let mut point = POINT::default();
        // SAFETY: `point` is a valid out pointer for the duration of the call.
        unsafe { GetCursorPos(&raw mut point) }.ok()?;
        Some((f64::from(point.x), f64::from(point.y)))
    }

    pub(super) fn set_click_through(&self, window: &Window, options: Options, through: bool) -> Result<String, String> {
        match options.hit_test {
            HitTestBackend::Winit => {
                window.set_cursor_hittest(!through).map_err(|e| e.to_string())?;
                // winit rebuilds the extended style from its own flags, which drops the WS_EX_TOOLWINDOW added above.
                if options.hide_from_switcher {
                    hide_from_switcher(hwnd(window).ok_or("no Win32 window handle")?)?;
                }
                Ok(format!("winit set_cursor_hittest({})", !through))
            }
            HitTestBackend::Native => {
                let hwnd = hwnd(window).ok_or("no Win32 window handle")?;
                edit_ex_style(hwnd, |s| {
                    let layered = s | WS_EX_LAYERED.0 as isize;
                    if through {
                        layered | WS_EX_TRANSPARENT.0 as isize
                    } else {
                        layered & !(WS_EX_TRANSPARENT.0 as isize)
                    }
                })?;
                // A window given WS_EX_LAYERED draws nothing until its layered attributes are set once.
                // SAFETY: `hwnd` is our live window.
                unsafe { SetLayeredWindowAttributes(hwnd, COLORREF(0), 255, LWA_ALPHA) }.map_err(|e| e.to_string())?;
                Ok(format!("WS_EX_LAYERED | WS_EX_TRANSPARENT {}", if through { "set" } else { "cleared" }))
            }
        }
    }

    pub(super) fn refresh(&mut self, _window: &Window, _mask: &Mask<'_>) -> Option<Feature> {
        None
    }

    pub(super) fn describe(&self, window: &Window) -> Vec<String> {
        let Some(hwnd) = hwnd(window) else { return vec!["win32: no window handle".to_owned()] };
        // SAFETY: reading the style word of our own live window.
        let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
        let has = |flag: u32| if style & flag != 0 { "set" } else { "clear" };
        vec![format!(
            "win32: WS_EX_LAYERED {}, WS_EX_TRANSPARENT {}, WS_EX_TOOLWINDOW {}, WS_EX_APPWINDOW {}",
            has(WS_EX_LAYERED.0),
            has(WS_EX_TRANSPARENT.0),
            has(WS_EX_TOOLWINDOW.0),
            has(WS_EX_APPWINDOW.0)
        )]
    }
}

/// CPU time this process has used so far (user plus system, all threads).
pub(super) fn process_cpu_seconds() -> Option<f64> {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let (mut created, mut exited, mut kernel, mut user) =
        (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: the pseudo handle from GetCurrentProcess is always valid and the four out-pointers are live locals.
    unsafe {
        GetProcessTimes(GetCurrentProcess(), &raw mut created, &raw mut exited, &raw mut kernel, &raw mut user).ok()?;
    }
    let ticks = |t: FILETIME| (u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime)) as f64;
    // FILETIME counts 100 ns intervals.
    Some((ticks(kernel) + ticks(user)) / 1.0e7)
}
