//! The ports (design §2, §3, §5): what the frame thread drains, what the binding dispatcher polls, the handles a
//! surface gets, and the hub an App builds once.
//!
//! ```text
//!  InputHub (built once by the App)
//!   ├─ control()         ─▶ Services: dyn InputControl  (the input.capture.* and input.binding.* ops)
//!   ├─ indicator_host()  ─▶ the surface's indicator      (never in Services)
//!   ├─ chord_store()     ─▶ the binding editor           (never in Services)
//!   ├─ protect_list()    ─▶ the person's settings page   (never in Services)
//!   └─ take_feeds()      ─▶ MotionFeed + IntentFeed to the frame thread, BindingFeed to the dispatcher (once)
//! ```
//!
//! **Hot path:** [`MotionFeed::drain`], [`MotionFeed::pad`], [`MotionFeed::pads`] and [`IntentFeed::drain`] run on the
//! frame thread and never block, lock or allocate. Frames never drop: due events that do not fit stay queued, in
//! order, for the next drain.

use std::sync::Arc;

use omuzaki_core::time::Seconds;

use crate::binding::{BindingAction, BindingHit, BindingId, BindingInfo, ChordLabel, IntentEvent};
use crate::capture::{CaptureReport, Detail, IndicatorHandle, InputError};
use crate::event::{DeviceClass, DeviceMask, MotionEvent, PadId, PadSet, PadState, SourceMask, SourceTag};

/// How many events a [`MotionFrame`] holds.
pub const MOTION_FRAME_MAX: usize = 128;

/// How many events an [`IntentFrame`] holds.
pub const INTENT_FRAME_MAX: usize = 64;

/// One drain's motion events, in drained order, and the sources whose held state ended before them. A fixed array.
#[derive(Clone, Debug)]
pub struct MotionFrame {
    events: [MotionEvent; MOTION_FRAME_MAX],
    len: usize,
    reset: SourceMask,
}

/// The value unused slots hold; never handed out.
const BLANK: MotionEvent = MotionEvent::source_gone(SourceTag(0), Seconds(0.0));

impl Default for MotionFrame {
    fn default() -> Self {
        Self::new()
    }
}

impl MotionFrame {
    /// An empty frame.
    pub const fn new() -> Self {
        Self { events: [BLANK; MOTION_FRAME_MAX], len: 0, reset: SourceMask::NONE }
    }

    /// Empties it (the feed does this at the start of every drain).
    pub fn clear(&mut self) {
        self.len = 0;
        self.reset = SourceMask::NONE;
    }

    /// Appends `event`; gives it back when the frame is full, for the feed to keep queued.
    ///
    /// # Errors
    /// The frame holds [`MOTION_FRAME_MAX`] events.
    pub fn push(&mut self, event: MotionEvent) -> Result<(), MotionEvent> {
        match self.events.get_mut(self.len) {
            Some(slot) => {
                *slot = event;
                self.len += 1;
                Ok(())
            }
            None => Err(event),
        }
    }

    /// Whether it holds [`MOTION_FRAME_MAX`] events.
    pub const fn is_full(&self) -> bool {
        self.len == MOTION_FRAME_MAX
    }

    /// The events, in drained order.
    pub fn events(&self) -> &[MotionEvent] {
        self.events.get(..self.len).unwrap_or_default()
    }

    /// The sources whose held state (zones, buttons, pad zones and levels) ended before this frame's events: a pause,
    /// a stop, a backend leaving, a `detail` change (design §4, §5a).
    pub const fn reset(&self) -> SourceMask {
        self.reset
    }

    /// Adds `sources` to [`reset`](Self::reset).
    pub fn add_reset(&mut self, sources: SourceMask) {
        self.reset = self.reset.or(sources);
    }
}

/// One drain's locomotion intents, in drained order, and the sources whose held intents ended before them.
#[derive(Clone, Debug)]
pub struct IntentFrame {
    events: [Option<IntentEvent>; INTENT_FRAME_MAX],
    len: usize,
    reset: SourceMask,
}

impl Default for IntentFrame {
    fn default() -> Self {
        Self::new()
    }
}

impl IntentFrame {
    /// An empty frame.
    pub const fn new() -> Self {
        Self { events: [None; INTENT_FRAME_MAX], len: 0, reset: SourceMask::NONE }
    }

    /// Empties it.
    pub fn clear(&mut self) {
        self.len = 0;
        self.reset = SourceMask::NONE;
    }

    /// Appends `event`; gives it back when the frame is full.
    ///
    /// # Errors
    /// The frame holds [`INTENT_FRAME_MAX`] events.
    pub fn push(&mut self, event: IntentEvent) -> Result<(), IntentEvent> {
        match self.events.get_mut(self.len) {
            Some(slot) => {
                *slot = Some(event);
                self.len += 1;
                Ok(())
            }
            None => Err(event),
        }
    }

    /// Whether it holds [`INTENT_FRAME_MAX`] events.
    pub const fn is_full(&self) -> bool {
        self.len == INTENT_FRAME_MAX
    }

    /// The events, in drained order.
    pub fn events(&self) -> impl ExactSizeIterator<Item = IntentEvent> + '_ {
        self.events.get(..self.len).unwrap_or_default().iter().map(|e| e.unwrap_or(BLANK_INTENT))
    }

    /// The sources whose held intents ended before this frame's events.
    pub const fn reset(&self) -> SourceMask {
        self.reset
    }

    /// Adds `sources` to [`reset`](Self::reset).
    pub fn add_reset(&mut self, sources: SourceMask) {
        self.reset = self.reset.or(sources);
    }
}

/// Never handed out: every slot below `len` holds an event.
const BLANK_INTENT: IntentEvent = IntentEvent {
    id: BindingId(0),
    intent: crate::binding::Intent::Run,
    edge: crate::binding::Edge::Release,
    at: Seconds(0.0),
};

/// The frame thread's port for motion. Never blocks, never allocates.
pub trait MotionFeed: Send {
    /// Replaces `out` with the events at or before `up_to`: each backend's queue in its order, the queues in turn
    /// (design §2). Events past [`MOTION_FRAME_MAX`] stay queued, in order, for the next drain.
    fn drain(&mut self, up_to: Seconds, out: &mut MotionFrame);

    /// A pad's newest levels; `None` for a pad that is not connected or whose backend is paused, so a deflected stick
    /// cannot outlast a pause.
    fn pad(&self, pad: PadId) -> Option<PadState>;

    /// Replaces `out` with the connected pads.
    fn pads(&self, out: &mut PadSet);
}

/// The frame thread's port for locomotion intents. Never blocks, never allocates.
pub trait IntentFeed: Send {
    /// Replaces `out` with the intents at or before `up_to`, in drained order; the rest stay queued.
    fn drain(&mut self, up_to: Seconds, out: &mut IntentFrame);
}

/// The binding dispatcher's port (the ops side): resolved hits, ready to run.
pub trait BindingFeed: Send {
    /// The next hit, if any. A release carries the action its press started (design §5); the borrow is of the held
    /// record's own copy, never of a table that can be recycled.
    fn poll(&mut self) -> Option<BindingHit<'_>>;
}

/// The `Services` entry the `input.capture.*` and `input.binding.*` ops call. Nothing here takes or returns a chord, a
/// key or an indicator.
pub trait InputControl: Send + Sync {
    /// Starts or resumes capture of `classes`. The op that calls it is person-only.
    ///
    /// # Errors
    /// [`InputError::NoIndicator`] unless an installed indicator is shown.
    fn start(&self, classes: DeviceMask) -> Result<(), InputError>;

    /// Pauses capture, keeping the classes; only a person's start resumes it. Idempotent; open to everyone.
    fn pause(&self);

    /// Stops capture. Idempotent; open to everyone.
    fn stop(&self);

    /// The state, the backends and what works here, with the full pause reason (the op redacts it).
    fn report(&self) -> CaptureReport;

    /// Sets the protected-program list (the kill switch). The op that calls it needs the `process` scope.
    fn protect(&self, programs: &[String]);

    /// The bindings, without their chords.
    fn bindings(&self) -> Vec<BindingInfo>;

    /// Adds a binding with no chord yet (the person learns it in the binding editor).
    ///
    /// # Errors
    /// [`InputError::TooManyBindings`], or [`InputError::Store`].
    fn add_binding(&self, name: &str, device: DeviceClass, action: BindingAction) -> Result<BindingId, InputError>;

    /// Sets what a binding does; from its next press on.
    ///
    /// # Errors
    /// [`InputError::UnknownBinding`], or [`InputError::Store`].
    fn set_action(&self, id: BindingId, action: BindingAction) -> Result<(), InputError>;

    /// Removes a binding; an action it holds ends.
    ///
    /// # Errors
    /// [`InputError::UnknownBinding`], or [`InputError::Store`].
    fn remove(&self, id: BindingId) -> Result<(), InputError>;

    /// The presets' names.
    fn presets(&self) -> Vec<String>;

    /// The actions a preset would bind, so the op can check the caller holds every scope they need.
    ///
    /// # Errors
    /// [`InputError::UnknownPreset`].
    fn preset_actions(&self, preset: &str) -> Result<Vec<BindingAction>, InputError>;

    /// Applies a preset: its bindings, with their chords, which stay inside the hub.
    ///
    /// # Errors
    /// [`InputError::UnknownPreset`], [`InputError::TooManyBindings`], or [`InputError::Store`].
    fn apply_preset(&self, preset: &str) -> Result<Vec<BindingId>, InputError>;
}

/// Where a surface installs its indicator. Handed to the surface directly, never put in `Services`.
pub trait IndicatorHost: Send + Sync {
    /// Installs an indicator; the surface keeps the handle, writes heartbeats to it and reads the view from it.
    fn install(&self) -> IndicatorHandle;
}

/// The chords, for the binding editor alone; never in `Services`. Nothing here carries a chord, only its label.
pub trait ChordStore: Send + Sync {
    /// Reads the next chord pressed and stores it for `id`, returning its label.
    ///
    /// # Errors
    /// [`InputError::NotCapturing`] unless capture is on with a shown indicator; [`InputError::UnknownBinding`].
    fn learn(&self, id: BindingId) -> Result<ChordLabel, InputError>;

    /// The label of `id`'s chord, if it has one.
    fn label(&self, id: BindingId) -> Option<ChordLabel>;

    /// Forgets `id`'s chord.
    fn clear(&self, id: BindingId);

    /// Replaces `out` with the bindings sharing `id`'s chord. Cold.
    fn conflicts(&self, id: BindingId, out: &mut Vec<BindingId>);
}

/// The person's kill-switch list; never in `Services`.
pub trait ProtectList: Send + Sync {
    /// Sets the protected programs.
    fn set(&self, programs: &[String]);

    /// The protected programs.
    fn get(&self) -> Vec<String>;

    /// The protected program running now, for the person's own indicator.
    fn matching(&self) -> Option<String>;
}

/// The three feeds, taken once.
pub type Feeds = (Box<dyn MotionFeed>, Box<dyn IntentFeed>, Box<dyn BindingFeed>);

/// What an App builds once and hands out from: the `Services` entry, the three surface handles and the feeds.
pub trait InputHub: Send + Sync {
    /// The `Services` entry.
    fn control(&self) -> Arc<dyn InputControl>;

    /// Where the surface installs its indicator.
    fn indicator_host(&self) -> Arc<dyn IndicatorHost>;

    /// The chords, for the binding editor.
    fn chord_store(&self) -> Arc<dyn ChordStore>;

    /// The person's protected-program list.
    fn protect_list(&self) -> Arc<dyn ProtectList>;

    /// The feeds; `None` after the first call.
    fn take_feeds(&self) -> Option<Feeds>;

    /// The `input.motion.detail` setting changed.
    fn set_detail(&self, detail: Detail);
}
