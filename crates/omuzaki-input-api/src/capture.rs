//! Capture state (design §4): off by default, started only by a person, never restored, and running only while an
//! indicator is shown. The global [`CaptureState`] is derived from each backend's [`BackendState`]; none of them names a
//! device or a key.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use omuzaki_core::time::{Clock, Seconds};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::binding::BindingId;
use crate::event::DeviceMask;

/// How much of a key the motion sees (the `input.motion.detail` setting, design §6.2).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Detail {
    /// Keys arrive as their zone.
    #[default]
    Zones,
    /// Keys arrive as their side only ([`KeyZone::LEFT_ONLY`](crate::zones::KeyZone::LEFT_ONLY) or
    /// [`RIGHT_ONLY`](crate::zones::KeyZone::RIGHT_ONLY)); the motion draws the finger.
    HandOnly,
}

/// Why the hub paused capture on its own. Shown only to actors that hold the `process` scope, and in full only on the
/// person's own indicator.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PauseWhy {
    /// The indicator stopped being drawn (its heartbeat went stale).
    IndicatorLost,
    /// A program on the person's protected list is running.
    ProtectedApp,
}

/// What a backend needs the person to grant.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionHow {
    /// macOS Input Monitoring, in System Settings.
    InputMonitoring,
    /// Membership of the Linux `input` group, for the evdev opt-in.
    InputGroup,
}

/// One running backend's state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum BackendState {
    /// Reading its devices.
    Running,
    /// Waiting for the person to grant a permission.
    #[non_exhaustive]
    NeedsPermission {
        /// Which.
        how: PermissionHow,
    },
    /// Stopped by a failure; `reason` is a fixed string, never device or key data.
    #[non_exhaustive]
    Failed {
        /// Why.
        reason: String,
    },
}

impl BackendState {
    /// [`BackendState::NeedsPermission`].
    pub const fn needs_permission(how: PermissionHow) -> Self {
        Self::NeedsPermission { how }
    }

    /// [`BackendState::Failed`].
    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed { reason: reason.into() }
    }
}

/// One backend in `input.capture.state`'s list: its registered name, the classes it serves, its state. No device names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackendReport {
    /// Its registered stable name (`windows.raw_input`, `gamepad.gilrs`, ...).
    pub name: String,
    /// The device classes it serves.
    pub classes: DeviceMask,
    /// Its state.
    pub state: BackendState,
}

/// The global capture state, derived from the backends' (design §4).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum CaptureState {
    /// Before start and after stop. A crash or restart comes back here.
    #[default]
    Off,
    /// Paused, keeping the classes chosen. `why` is set for the hub's own pauses, and shown only with `process`.
    #[non_exhaustive]
    Paused {
        /// Why the hub paused, if it did.
        why: Option<PauseWhy>,
    },
    /// At least one backend runs.
    #[non_exhaustive]
    On {
        /// The classes being read.
        classes: DeviceMask,
        /// The classes asked for that a backend should serve and does not.
        degraded: DeviceMask,
    },
    /// Capture was started and no backend runs; `reason` is a fixed string.
    #[non_exhaustive]
    Failed {
        /// Why.
        reason: String,
    },
}

impl CaptureState {
    /// [`CaptureState::Paused`].
    pub const fn paused(why: Option<PauseWhy>) -> Self {
        Self::Paused { why }
    }

    /// [`CaptureState::On`].
    pub const fn on(classes: DeviceMask, degraded: DeviceMask) -> Self {
        Self::On { classes, degraded }
    }

    /// [`CaptureState::Failed`].
    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed { reason: reason.into() }
    }

    /// Whether capture is on.
    pub const fn is_on(&self) -> bool {
        matches!(self, Self::On { .. })
    }

    /// The state as an actor without the `process` scope may see it: every pause reason hidden, so one cannot be told
    /// from another by elimination (design §4).
    #[must_use]
    pub fn redacted(self) -> Self {
        match self {
            Self::Paused { .. } => Self::Paused { why: None },
            other => other,
        }
    }
}

/// How well a part of capture works on this OS and build.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Support {
    /// It works.
    Available,
    /// It works once the person opts in (the evdev `input` group on Wayland).
    NeedsOptIn,
    /// It needs an OS permission first.
    NeedsPermission,
    /// It goes through the desktop portal (bindings on Wayland).
    Portal,
    /// Not on this OS or build.
    #[default]
    Unavailable,
}

/// What works per class on this OS (research §2), so the UI can say so.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CaptureCaps {
    /// Typing motion (keyboard zones).
    pub typing: Support,
    /// Relative mouse motion and buttons.
    pub pointer: Support,
    /// The pen's position and contact.
    pub pen: Support,
    /// The pen's pressure.
    pub pen_pressure: Support,
    /// Gamepads.
    pub gamepad: Support,
    /// Keyboard bindings.
    pub bindings: Support,
    /// MIDI controllers (bindings only).
    pub midi: Support,
    /// OSC messages that press bindings.
    pub osc: Support,
}

/// Everything `input.capture.state` reports: the state, the per-backend list and the capabilities.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CaptureReport {
    /// The global state, with its pause reason (redact it with [`CaptureState::redacted`] for actors without
    /// `process`).
    pub state: CaptureState,
    /// The running backends.
    pub backends: Vec<BackendReport>,
    /// What works here.
    pub caps: CaptureCaps,
    /// The detail keys are read at.
    pub detail: Detail,
}

/// What an indicator shows, written by the hub (design §4).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndicatorView {
    /// The state, with the full pause reason: the indicator is the person's own.
    pub state: CaptureState,
    /// The detail keys are read at.
    pub detail: Detail,
    /// The protected program that paused capture, if one did (from the person's own list).
    pub program: Option<String>,
    /// The running backends.
    pub backends: Vec<BackendReport>,
}

/// How recent a heartbeat must be for the indicator to count as shown.
pub const SHOWN_WITHIN: Seconds = Seconds(1.5);

/// An installed indicator: two values shared by the surface and the hub, and nothing that calls across (design §4).
///
/// The surface calls [`beat`](Self::beat) each time it has actually drawn the indicator in a visible window and reads
/// [`view`](Self::view) to draw it; the hub reads the heartbeat ([`is_shown`](Self::is_shown)) and writes the view
/// ([`publish`](Self::publish)). Neither ever waits for the other: the hub's write only tries the view's lock and
/// retries on its next tick.
#[derive(Clone)]
pub struct IndicatorHandle {
    shared: Arc<Shared>,
}

struct Shared {
    clock: Arc<dyn Clock>,
    /// The last heartbeat's time as `f64` bits; [`NEVER`] before the first.
    heartbeat: AtomicU64,
    view: Mutex<IndicatorView>,
    version: AtomicU64,
}

const NEVER: u64 = u64::MAX;

impl std::fmt::Debug for IndicatorHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IndicatorHandle").field("last_beat", &self.last_beat()).finish_non_exhaustive()
    }
}

impl IndicatorHandle {
    /// A handle whose heartbeats are timed on `clock` (the hub's), showing `Off` until the hub writes a view.
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let shared = Shared {
            clock,
            heartbeat: AtomicU64::new(NEVER),
            view: Mutex::new(IndicatorView::default()),
            version: AtomicU64::new(0),
        };
        Self { shared: Arc::new(shared) }
    }

    /// The surface: the indicator was just drawn in a visible window.
    pub fn beat(&self) {
        self.shared.heartbeat.store(self.shared.clock.now().0.to_bits(), Ordering::Release);
    }

    /// The last heartbeat, if there was one.
    pub fn last_beat(&self) -> Option<Seconds> {
        match self.shared.heartbeat.load(Ordering::Acquire) {
            NEVER => None,
            bits => Some(Seconds(f64::from_bits(bits))),
        }
    }

    /// The hub: whether the indicator was drawn within [`SHOWN_WITHIN`].
    pub fn is_shown(&self) -> bool {
        self.last_beat().is_some_and(|beat| self.shared.clock.now().0 - beat.0 < SHOWN_WITHIN.0)
    }

    /// The surface: what to show.
    pub fn view(&self) -> IndicatorView {
        self.shared.view.lock().map_or_else(|poisoned| poisoned.into_inner().clone(), |view| view.clone())
    }

    /// The view's version, bumped on every write, so a surface redraws only on a change.
    pub fn view_version(&self) -> u64 {
        self.shared.version.load(Ordering::Acquire)
    }

    /// The hub: shows `view`. Never waits: `false` when the surface holds the view right now, and the hub writes it
    /// again on its next tick.
    pub fn publish(&self, view: &IndicatorView) -> bool {
        let Ok(mut current) = self.shared.view.try_lock() else {
            return false;
        };
        if *current != *view {
            current.clone_from(view);
            self.shared.version.fetch_add(1, Ordering::AcqRel);
        }
        true
    }
}

/// What can go wrong with input capture. No variant carries a key, a chord or a device name.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum InputError {
    /// Capture can start only while an installed indicator is shown.
    #[error("input capture needs its indicator on screen: no indicator is installed or shown")]
    NoIndicator,
    /// Learning a chord reads OS input, so it needs capture on with a shown indicator.
    #[error("learning a chord needs input capture on")]
    NotCapturing,
    /// No binding has this id.
    #[error("no binding {0:?}")]
    UnknownBinding(BindingId),
    /// No preset has this name.
    #[error("no binding preset is named `{0}`")]
    UnknownPreset(String),
    /// The table holds [`MAX_BINDINGS`](crate::binding::MAX_BINDINGS) bindings already.
    #[error("the binding table is full")]
    TooManyBindings,
    /// The feeds were taken already: an App takes them once.
    #[error("the input feeds were taken already")]
    FeedsTaken,
    /// The binding store could not be read or written; a fixed reason.
    #[error("the binding store: {0}")]
    Store(String),
    /// Nothing on this OS or build can do it; a fixed reason.
    #[error("unavailable here: {0}")]
    Unavailable(String),
}
