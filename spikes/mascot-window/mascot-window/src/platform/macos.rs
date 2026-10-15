//! macOS: winit for the window, the Dock icon and the app switcher through the activation policy, and `AppKit` for the
//! global cursor. Not compiled or run in the session that wrote it (no macOS toolchain there): the first build on a
//! Mac is part of the verification.
#![allow(unsafe_code)]
// The `State` methods have the same shape on every OS, so on macOS some take `&self` without using it.
#![allow(clippy::unused_self)]

use objc2::MainThreadMarker;
use objc2_app_kit::{NSEvent, NSScreen};
use winit::event_loop::EventLoopBuilder;
use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS as _, WindowAttributesExtMacOS as _};
use winit::window::{Window, WindowAttributes};

use super::{Feature, Options};
use crate::scene::{Fit, Scene};

pub(super) fn attributes(attributes: WindowAttributes, _options: Options) -> WindowAttributes {
    // The system shadow would draw a dark halo around the transparent window's rectangle.
    attributes.with_has_shadow(false)
}

pub(super) fn configure_event_loop<T: 'static>(builder: &mut EventLoopBuilder<T>, options: Options) {
    if options.hide_from_switcher {
        // Accessory: no Dock icon, not in Cmd-Tab.
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
}

pub(super) fn is_wayland(_window: &Window) -> bool {
    false
}

pub(super) struct State;

impl State {
    pub(super) fn new(_window: &Window, options: Options, _fit: &Fit, _scene: &Scene) -> (Self, Vec<Feature>) {
        let mut features = Vec::new();
        if options.hide_from_switcher {
            features.push(Feature::applied(
                "hide_from_switcher",
                "ActivationPolicy::Accessory (check the Dock and Cmd-Tab)",
            ));
        }
        (Self, features)
    }

    /// macOS needs the global cursor to drive per-pixel click-through.
    pub(super) fn polls_cursor(&self) -> bool {
        true
    }

    /// The cursor in physical pixels, origin at the top-left of the primary screen, like winit's window positions.
    pub(super) fn cursor_position(&self, window: &Window) -> Option<(f64, f64)> {
        let mtm = MainThreadMarker::new()?;
        let location = NSEvent::mouseLocation();
        // AppKit's origin is the bottom-left of the primary screen; flip with its height.
        let primary_height = NSScreen::screens(mtm).firstObject()?.frame().size.height;
        let scale = window.scale_factor();
        Some((location.x * scale, (primary_height - location.y) * scale))
    }

    pub(super) fn set_click_through(
        &self,
        window: &Window,
        _options: Options,
        through: bool,
    ) -> Result<String, String> {
        window
            .set_cursor_hittest(!through)
            .map(|()| format!("ignoresMouseEvents = {through} (via winit set_cursor_hittest)"))
            .map_err(|e| e.to_string())
    }

    pub(super) fn resized(&mut self, _window: &Window, _fit: &Fit, _scene: &Scene) -> Option<Feature> {
        None
    }

    pub(super) fn describe(&self, _window: &Window) -> Vec<String> {
        Vec::new()
    }
}

/// CPU time this process has used so far (user plus system, all threads).
pub(super) fn process_cpu_seconds() -> Option<f64> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage fills the whole struct when it returns 0, and `usage` is only read after that check.
    let usage = unsafe {
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return None;
        }
        usage.assume_init()
    };
    let seconds = |t: libc::timeval| t.tv_sec as f64 + f64::from(t.tv_usec) / 1.0e6;
    Some(seconds(usage.ru_utime) + seconds(usage.ru_stime))
}
