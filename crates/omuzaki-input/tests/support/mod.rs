//! A hub with two test backends on a fake clock, its feeds and an injector per backend.
#![allow(clippy::unwrap_used, dead_code)]

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_input::{FakeProcesses, Hub, HubConfig, Injector, TestBackend};
use omuzaki_input_api::binding::{BindingAction, BindingId, Edge, IntentEvent};
use omuzaki_input_api::capture::{CaptureState, IndicatorHandle};
use omuzaki_input_api::event::{DeviceClass, DeviceMask, MotionEvent, SourceMask, SourceTag};
use omuzaki_input_api::zones::KeyZone;
use omuzaki_input_api::feed::{BindingFeed, InputControl, InputHub, IntentFeed, IntentFrame, MotionFeed, MotionFrame};

/// Keyboard usages the tests press.
pub const W: u16 = 0x1A;
pub const A: u16 = 0x04;
pub const J: u16 = 0x0D;
pub const F9: u16 = 0x42;
pub const F10: u16 = 0x43;
pub const LEFT_CTRL: u16 = 0xE0;
pub const CTRL: u8 = 1;

/// The keyboard backend's devices.
pub const KEYBOARD: u64 = 1;
pub const KEYBOARD_2: u64 = 2;
pub const MOUSE: u64 = 3;
pub const PEN: u64 = 4;
/// The gamepad backend's devices.
pub const PAD: u64 = 10;
pub const PAD_2: u64 = 11;

/// The keyboard, mouse and pen backend.
pub const DESK: TestBackend = TestBackend {
    name: "a.desk",
    classes: DeviceMask::NONE.with(DeviceClass::Keyboard).with(DeviceClass::Mouse).with(DeviceClass::Pen),
};
/// The gamepad backend.
pub const PADS: TestBackend = TestBackend { name: "b.pads", classes: DeviceMask::NONE.with(DeviceClass::Gamepad) };

pub struct Rig {
    pub clock: Arc<FakeClock>,
    pub hub: Hub,
    pub control: Arc<dyn InputControl>,
    pub indicator: IndicatorHandle,
    pub processes: FakeProcesses,
    pub desk: Injector,
    pub pads: Injector,
    pub motion: Box<dyn MotionFeed>,
    pub intents: Box<dyn IntentFeed>,
    pub bindings: Box<dyn BindingFeed>,
    /// The `SourceUp`s of the devices `started` connected.
    pub ups: Vec<MotionEvent>,
    motion_frame: MotionFrame,
    intent_frame: IntentFrame,
}

impl Rig {
    /// A hub with the indicator installed and drawn, capture off.
    pub fn new() -> Self {
        Self::in_dir(None)
    }

    pub fn in_dir(dir: Option<std::path::PathBuf>) -> Self {
        let clock = Arc::new(FakeClock::new(Seconds(10.0)));
        let processes = FakeProcesses::default();
        let config = HubConfig::new(clock.clone(), dir)
            .with_test_backends(&[DESK, PADS])
            .manual()
            .with_processes(processes.clone());
        let hub = Hub::new(config).unwrap();
        let indicator = hub.indicator_host().install();
        indicator.beat();
        let control = hub.control();
        let (motion, intents, bindings) = hub.take_feeds().unwrap();
        let desk = hub.injector(0);
        let pads = hub.injector(1);
        Self {
            clock,
            hub,
            control,
            indicator,
            processes,
            desk,
            pads,
            motion,
            intents,
            bindings,
            ups: Vec::new(),
            motion_frame: MotionFrame::new(),
            intent_frame: IntentFrame::new(),
        }
    }

    /// Capture on for every class, with the usual devices connected.
    pub fn started() -> Self {
        let mut rig = Self::new();
        rig.start();
        rig.desk.device_up(KEYBOARD, DeviceClass::Keyboard);
        rig.desk.device_up(MOUSE, DeviceClass::Mouse);
        rig.desk.device_up(PEN, DeviceClass::Pen);
        rig.pads.device_up(PAD, DeviceClass::Gamepad);
        rig.ups = rig.motion().0;
        rig.drain_all();
        rig
    }

    pub fn start(&self) {
        self.indicator.beat();
        self.control.start(DeviceMask::ALL).unwrap();
    }

    pub fn state(&self) -> CaptureState {
        self.control.report().state
    }

    pub fn advance(&self, seconds: f64) {
        self.clock.advance(Seconds(seconds));
    }

    /// One motion drain: its events and its reset.
    pub fn motion(&mut self) -> (Vec<MotionEvent>, SourceMask) {
        self.motion.drain(Seconds(f64::MAX), &mut self.motion_frame);
        (self.motion_frame.events().to_vec(), self.motion_frame.reset())
    }

    /// One intent drain.
    pub fn intents(&mut self) -> Vec<IntentEvent> {
        self.intents.drain(Seconds(f64::MAX), &mut self.intent_frame);
        self.intent_frame.events().collect()
    }

    /// Every binding hit there is now.
    pub fn hits(&mut self) -> Vec<(BindingId, BindingAction, Edge)> {
        let mut out = Vec::new();
        while let Some(hit) = self.bindings.poll() {
            out.push((hit.id, hit.action.clone(), hit.edge));
        }
        out
    }

    /// Empties every feed.
    pub fn drain_all(&mut self) {
        let _ = self.motion();
        let _ = self.intents();
        let _ = self.hits();
    }

    /// An op binding on a key with modifiers; its action is trigger `slot`.
    pub fn trigger_on(&self, slot: u8, usage: u16, mods: u8) -> BindingId {
        let id = self.control.add_binding("test", DeviceClass::Keyboard, BindingAction::Trigger(slot)).unwrap();
        self.hub.bind_key(id, usage, mods).unwrap();
        id
    }

    /// The locomotion preset's ids, in preset order (W, A, S, D, Space, Shift, C).
    pub fn locomotion(&self) -> Vec<BindingId> {
        self.control.apply_preset("locomotion").unwrap()
    }
}

impl Rig {
    /// The source `started` connected for `class`.
    pub fn source(&self, class: DeviceClass) -> SourceTag {
        self.ups
            .iter()
            .find_map(|e| match *e {
                MotionEvent::SourceUp { src, class: c, .. } if c == class => Some(src),
                _ => None,
            })
            .unwrap()
    }
}

/// The key events in `events`, as (source, zone, down).
pub fn keys(events: &[MotionEvent]) -> Vec<(SourceTag, KeyZone, bool)> {
    events
        .iter()
        .filter_map(|e| match *e {
            MotionEvent::Key { src, zone, down, .. } => Some((src, zone, down)),
            _ => None,
        })
        .collect()
}

/// The edges of `id` in `events`.
pub fn edges(events: &[IntentEvent], id: BindingId) -> Vec<Edge> {
    events.iter().filter(|e| e.id == id).map(|e| e.edge).collect()
}
