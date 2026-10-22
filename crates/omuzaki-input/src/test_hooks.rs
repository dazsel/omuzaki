//! What this crate's own tests and benches need to drive the hub without an OS (design §13 IN-3): test backends fed by
//! a raw injector that stands in for a capture thread, the control tick run by hand on a fake clock, a fake process
//! list, and direct chord binding. Behind the `test-hooks` feature, which no other crate enables (`tests/gate.rs`).
#![cfg(feature = "test-hooks")]

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_input_api::binding::BindingId;
use omuzaki_input_api::capture::{BackendState, CaptureCaps, InputError, Support};
use omuzaki_input_api::event::{DeviceClass, DeviceMask, PadZones};
use omuzaki_input_api::zones::KeyZone;

use crate::hub::{Hub, HubConfig, InputBackend, spawn_capture};
use crate::kill_switch::Processes;
use crate::matcher::{Chord, Mods};
use crate::normalise::{HidUsage, RawEvent, RawSink, zone_map};
use crate::shared::lock;

/// A backend with no thread of its own: a test feeds it through an [`Injector`].
#[derive(Copy, Clone, Debug)]
pub struct TestBackend {
    /// Its name; backends are drained in name order.
    pub name: &'static str,
    /// The classes it serves.
    pub classes: DeviceMask,
}

fn all_available() -> CaptureCaps {
    CaptureCaps {
        typing: Support::Available,
        pointer: Support::Available,
        pen: Support::Available,
        pen_pressure: Support::Available,
        gamepad: Support::Available,
        bindings: Support::Available,
    }
}

impl HubConfig {
    /// Uses these backends instead of the registered ones; backend `i` of [`Hub::injector`] is the `i`-th by name.
    #[must_use]
    pub fn with_test_backends(mut self, backends: &[TestBackend]) -> Self {
        self.backends = Some(
            backends
                .iter()
                .map(|b| InputBackend { name: b.name, classes: b.classes, caps: all_available, run: None })
                .collect(),
        );
        self
    }

    /// No control thread: the test runs each pass with [`Hub::tick`].
    #[must_use]
    pub fn manual(mut self) -> Self {
        self.manual = true;
        self
    }

    /// The kill switch reads this list instead of the OS's.
    #[must_use]
    pub fn with_processes(mut self, processes: FakeProcesses) -> Self {
        self.processes = Arc::new(processes);
        self
    }
}

/// A process list a test sets.
#[derive(Clone, Debug, Default)]
pub struct FakeProcesses(Arc<Mutex<Vec<String>>>);

impl FakeProcesses {
    /// The programs running from now on.
    pub fn set(&self, names: &[&str]) {
        *lock(&self.0) = names.iter().map(|n| (*n).to_owned()).collect();
    }
}

impl Processes for FakeProcesses {
    fn names(&self, out: &mut Vec<String>) {
        out.clone_from(&lock(&self.0));
    }
}

/// The keys of a zone, by usage: the zone-equivalence test swaps keys inside one zone.
pub fn keys_in_zone(zone: KeyZone) -> Vec<u16> {
    zone_map::in_zone(zone)
}

impl Hub {
    /// One pass of the control thread (the indicator check, the kill switch, the view).
    pub fn tick(&self) {
        self.core.tick();
    }

    /// The capture-thread side of test backend `backend`. Use one injector per backend, from one thread: the rings
    /// check their single writer in debug builds.
    pub fn injector(&self, backend: usize) -> Injector {
        Injector { sink: RawSink::new(Arc::clone(&self.core.shared), backend) }
    }

    /// Runs `f` on a capture thread of `backend`, the way a real backend's loop runs: a panic is caught and reported
    /// as the backend's fixed failure reason, and its epoch closes.
    pub fn run_backend(&self, backend: usize, f: impl FnOnce(&mut Injector) + Send + 'static) -> Option<JoinHandle<()>> {
        let shared = Arc::clone(&self.core.shared);
        spawn_capture(shared, backend, "test", move |sink| f(&mut Injector { sink }))
    }

    /// Fails backend `backend` as a panic on its thread would.
    pub fn fail_backend(&self, backend: usize) {
        self.core.fail_backend(backend);
    }

    /// Binds `id` to a key with modifiers (bits: 1 Ctrl, 2 Shift, 4 Alt, 8 Super), as `learn` would.
    ///
    /// # Errors
    /// As the store's.
    pub fn bind_key(&self, id: BindingId, usage: u16, mods: u8) -> Result<(), InputError> {
        self.core.set_chord(id, Some(Chord::Key { usage: HidUsage(usage), mods: Mods(mods) }))
    }

    /// Binds `id` to a set of pad buttons.
    ///
    /// # Errors
    /// As the store's.
    pub fn bind_pad(&self, id: BindingId, buttons: u32) -> Result<(), InputError> {
        self.core.set_chord(id, Some(Chord::Pad { buttons }))
    }

    /// Entries dropped because their binding was gone (the stale counter of design §5).
    pub fn stale_entries(&self) -> u64 {
        self.core.shared.stale_entries.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Whether a `ChordStore::learn` is waiting for a chord.
    pub fn learning(&self) -> bool {
        self.core.shared.learn.wanted.load(std::sync::atomic::Ordering::Acquire) != 0
    }
}

/// A raw injector: what a backend's capture thread does with what it reads, with raw usages in and nothing else.
#[derive(Debug)]
pub struct Injector {
    sink: RawSink,
}

impl Injector {
    /// A device connected.
    pub fn device_up(&mut self, device: u64, class: DeviceClass) {
        self.sink.push(RawEvent::DeviceUp { device, class });
    }

    /// It left.
    pub fn device_gone(&mut self, device: u64) {
        self.sink.push(RawEvent::DeviceGone { device });
    }

    /// A key went down or up.
    pub fn key(&mut self, device: u64, usage: u16, down: bool) {
        self.sink.push(RawEvent::Key { device, usage: HidUsage(usage), down, repeat: false });
    }

    /// The OS auto-repeated a key.
    pub fn repeat(&mut self, device: u64, usage: u16) {
        self.sink.push(RawEvent::Key { device, usage: HidUsage(usage), down: true, repeat: true });
    }

    /// Reads a key without pushing it yet: `true` when something waits for [`push_read`](Self::push_read). The
    /// epoch-window tests pause between the two.
    pub fn read_key(&mut self, device: u64, usage: u16, down: bool) -> bool {
        self.sink.prepare(RawEvent::Key { device, usage: HidUsage(usage), down, repeat: false })
    }

    /// Pushes what [`read_key`](Self::read_key) read.
    pub fn push_read(&mut self) {
        self.sink.commit();
    }

    /// Mouse or pen buttons.
    pub fn buttons(&mut self, device: u64, class: DeviceClass, pressed: u8, released: u8) {
        self.sink.push(RawEvent::Buttons { device, class, pressed, released });
    }

    /// Relative mouse motion in counts, and wheel notches.
    pub fn motion(&mut self, device: u64, dx: i32, dy: i32, wheel: f32) {
        self.sink.push(RawEvent::Motion { device, dx, dy, wheel });
    }

    /// A pen sample.
    pub fn pen(&mut self, device: u64, x: f32, y: f32, pressure: f32, contact: bool) {
        self.sink.push(RawEvent::Pen { device, x, y, pressure, tilt: [0.0; 2], in_range: true, contact });
    }

    /// A pad's state.
    pub fn pad(&mut self, device: u64, buttons: u32, zones: PadZones, sticks: [[f32; 2]; 2], triggers: [f32; 2]) {
        self.sink.push(RawEvent::Pad { device, buttons, zones, sticks, triggers });
    }

    /// The capture loop's idle call: pushes finished pointer periods and what waited for ring space.
    pub fn idle(&mut self) {
        self.sink.idle();
    }

    /// Reports the backend's state.
    pub fn set_state(&self, state: BackendState) {
        self.sink.set_state(state);
    }

    /// Parks until the backend may read; `false` when the hub is gone.
    pub fn wait_reading(&mut self, timeout: Duration) -> bool {
        self.sink.wait_reading(timeout)
    }

    /// Whether the backend may read now.
    pub fn reading(&self) -> bool {
        self.sink.reading()
    }
}
