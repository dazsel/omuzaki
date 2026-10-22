//! The one module where key identities exist (design §3 rules 2 and 3). A backend hands each raw event to its
//! [`RawSink`] on its capture thread; the sink turns a key into its zone and, when a binding's chord matches, a resolved
//! hit, and pushes them to the backend's rings stamped with the epochs they were read in. The raw event never leaves
//! the capture thread's stack, and nothing here prints, logs or indexes by a raw code: lookups use `get`, and a debug
//! print of a raw value is a placeholder.
//!
//! The sink also keeps what a capture thread must keep: which keys are down (and which are unknown after a pause, so a
//! key held across it starts nothing), the modifiers, each held key's press record (so its release ends what its press
//! started, design §5), each source's held snapshot, the pointer coalescing, and the device to source map.

// Quantising (pointer counts to metres, the pen to its grid and bytes, time to the period) casts on purpose; every
// value is clamped first.
#![allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]

pub(crate) mod zone_map;

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use omuzaki_core::time::Seconds;
use omuzaki_input_api::binding::{BindingId, Edge, Intent};
use omuzaki_input_api::capture::{BackendState, Detail};
use omuzaki_input_api::event::{DeviceClass, MotionEvent, PadId, PadState, PadZones, SourceTag};
use omuzaki_input_api::zones::{KeyZone, Side};
use serde::{Deserialize, Serialize};

use crate::entry::{self, BindingWords, HitEntry, MotionWords};
use crate::epoch::{is_open, stamp};
use crate::matcher::{self, Chord, Mods};
use crate::rings::{Backlog, Keep};
use crate::shared::{BINDING_RING, MOTION_RING, Shared, Table, lock};

/// A key's USB HID keyboard-page usage: its physical position. Crate-private, and never printed.
#[derive(Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct HidUsage(pub(crate) u16);

impl std::fmt::Debug for HidUsage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<key>")
    }
}

/// What a backend reads from the OS, before normalising. Lives on the capture thread's stack only.
#[derive(Copy, Clone)]
pub(crate) enum RawEvent {
    /// A device connected; `device` is the backend's own handle for it.
    DeviceUp { device: u64, class: DeviceClass },
    /// It left.
    DeviceGone { device: u64 },
    /// A key went down (`repeat` when the OS says it is an auto-repeat) or up.
    Key { device: u64, usage: HidUsage, down: bool, repeat: bool },
    /// Mouse or pen buttons, one bit per button.
    Buttons { device: u64, class: DeviceClass, pressed: u8, released: u8 },
    /// Relative pointer motion in device counts (right and down positive), and wheel notches.
    Motion { device: u64, dx: i32, dy: i32, wheel: f32 },
    /// A pen sample: position on the tablet in `0..=1`, pressure in `0..=1`, tilt in degrees.
    Pen { device: u64, x: f32, y: f32, pressure: f32, tilt: [f32; 2], in_range: bool, contact: bool },
    /// A pad's state now: its buttons (the backend's numbering), the zones they make, sticks in `-1..=1` (right and
    /// up positive) and triggers in `0..=1`.
    Pad { device: u64, buttons: u32, zones: PadZones, sticks: [[f32; 2]; 2], triggers: [f32; 2] },
}

impl std::fmt::Debug for RawEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<raw input>")
    }
}

/// Metres on the virtual pad per mouse count: a 400 DPI-equivalent default (design §3).
const METRES_PER_COUNT: f32 = 0.0254 / 400.0;
/// The coalescing period: event times are quantised to it.
const PERIOD: f64 = 1.0 / 120.0;
/// A stick's dead zone.
pub(crate) const DEAD_ZONE: f32 = 0.12;

const UP: u8 = 0;
const DOWN: u8 = 1;
/// Not known: held across a pause, so it starts nothing until its up is seen.
const UNKNOWN: u8 = 2;

/// Which ring a hit goes to.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Lane {
    Intent,
    Binding,
}

/// What a press started (design §5): its release goes to the same lane with the same generation, slot and binding.
#[derive(Copy, Clone, Debug)]
struct Press {
    src: SourceTag,
    key: PressKey,
    lane: Lane,
    generation: u32,
    slot: u8,
    id: BindingId,
    intent: Option<Intent>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum PressKey {
    Key(HidUsage),
    Pad(u32),
}

struct Device {
    handle: u64,
    src: SourceTag,
    class: DeviceClass,
    keys: Box<[u8; 256]>,
    pad: Option<PadId>,
    buttons: u32,
    zones: PadZones,
    /// Pad buttons and zones held across a pause: ignored until released.
    unknown_buttons: u32,
    unknown_zones: PadZones,
}

/// Pointer motion summed within one period.
#[derive(Copy, Clone, Debug)]
struct Accumulated {
    src: SourceTag,
    period: i64,
    dx: f32,
    dy: f32,
    wheel: f32,
}

/// The output of normalising one raw event, waiting to be pushed.
#[derive(Default)]
struct Out {
    motion: Vec<(MotionWords, Keep)>,
    hits: Vec<(Lane, BindingWords, Keep)>,
    free: Vec<SourceTag>,
}

/// A backend's way into the hub: normalises raw events and pushes them to the backend's rings. One per backend; it
/// lives on, and is used from, the backend's capture thread only.
pub(crate) struct RawSink {
    shared: Arc<Shared>,
    backend: usize,
    seen: (u32, u32),
    running: bool,
    detail: (Detail, u32),
    devices: Vec<Device>,
    mods: [u8; 4],
    presses: Vec<Press>,
    table: Arc<Table>,
    pointer: Vec<Accumulated>,
    motion: Backlog<{ entry::MOTION_WORDS }>,
    intents: Backlog<{ entry::BINDING_WORDS }>,
    bindings: Backlog<{ entry::BINDING_WORDS }>,
    out: Out,
}

impl std::fmt::Debug for RawSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawSink").field("backend", &self.backend).finish_non_exhaustive()
    }
}

impl RawSink {
    pub(crate) fn new(shared: Arc<Shared>, backend: usize) -> Self {
        let table = shared.tables.current();
        if let Some(slot) = shared.backends.get(backend) {
            slot.motion.new_writer();
            slot.intents.new_writer();
            slot.bindings.new_writer();
        }
        Self {
            shared,
            backend,
            seen: (0, 0),
            running: false,
            detail: (Detail::Zones, 0),
            devices: Vec::new(),
            mods: [0; 4],
            presses: Vec::with_capacity(64),
            table,
            pointer: Vec::with_capacity(8),
            motion: Backlog::new(MOTION_RING),
            intents: Backlog::new(BINDING_RING),
            bindings: Backlog::new(BINDING_RING),
            out: Out::default(),
        }
    }

    /// Whether the backend may read now (capture on and this backend's epoch open).
    pub(crate) fn reading(&self) -> bool {
        self.shared.reading(self.backend)
    }

    /// Whether the hub is gone and the backend's thread should end.
    pub(crate) fn shutting_down(&self) -> bool {
        self.shared.shutdown.load(Ordering::Acquire)
    }

    /// Parks until the backend may read, the hub shuts down or `timeout` passes; `false` on shutdown. While parked the
    /// backend reads nothing, as the design asks of a paused backend.
    pub(crate) fn wait_reading(&mut self, timeout: Duration) -> bool {
        if !self.reading() && !self.shutting_down() {
            let guard = lock(&self.shared.wake.0);
            let _woken = self
                .shared
                .wake
                .1
                .wait_timeout_while(guard, timeout, |()| !self.shared.reading(self.backend) && !self.shutting_down());
        }
        if !self.reading() && self.running {
            self.end_run();
        }
        !self.shutting_down()
    }

    /// The global and backend epochs now: a backend that sees them change knows a new run began, even when a pause
    /// came and went between two of its looks.
    pub(crate) fn epochs(&self) -> (u32, u32) {
        let backend = self.shared.backends.get(self.backend).map_or(0, |s| s.epoch.get());
        (self.shared.capture.get(), backend)
    }

    /// The handles of the devices of `class` the sink knows, so a backend can tell it which left while it read nothing.
    pub(crate) fn devices(&self, class: DeviceClass) -> Vec<u64> {
        self.devices.iter().filter(|d| d.class == class).map(|d| d.handle).collect()
    }

    /// Reports the backend's state (running, a permission it needs, a fixed failure reason).
    pub(crate) fn set_state(&self, state: BackendState) {
        if let Some(slot) = self.shared.backends.get(self.backend) {
            *lock(&slot.state) = Some(state);
        }
    }

    /// Normalises `raw` and pushes the result.
    pub(crate) fn push(&mut self, raw: RawEvent) {
        if self.prepare(raw) {
            self.commit();
        }
    }

    /// Pushes pointer motion of finished periods and whatever waited for ring space. Call it from the capture loop
    /// at least every few milliseconds.
    pub(crate) fn idle(&mut self) {
        if !self.sync_epoch() {
            return;
        }
        let period = self.period();
        self.flush_pointer(|acc| acc.period < period);
        self.commit();
    }

    /// The first half of [`push`](Self::push): reads the epochs and normalises; `true` when something waits for
    /// [`commit`](Self::commit).
    pub(crate) fn prepare(&mut self, raw: RawEvent) -> bool {
        self.out.motion.clear();
        self.out.hits.clear();
        self.out.free.clear();
        if !self.sync_epoch() {
            return false;
        }
        self.sync_table();
        match raw {
            RawEvent::DeviceUp { device, class } => self.device_up(device, class),
            RawEvent::DeviceGone { device } => self.device_gone(device),
            RawEvent::Key { device, usage, down, repeat } => self.key(device, usage, down, repeat),
            RawEvent::Buttons { device, class, pressed, released } => self.buttons(device, class, pressed, released),
            RawEvent::Motion { device, dx, dy, wheel } => self.motion(device, dx, dy, wheel),
            RawEvent::Pen { device, x, y, pressure, tilt, in_range, contact } => {
                self.pen(device, [x, y], pressure, tilt, in_range, contact);
            }
            RawEvent::Pad { device, buttons, zones, sticks, triggers } => {
                self.pad(device, buttons, zones, sticks, triggers);
            }
        }
        !(self.out.motion.is_empty() && self.out.hits.is_empty() && self.out.free.is_empty())
    }

    /// The second half of [`push`](Self::push): pushes what [`prepare`](Self::prepare) made, with the stamps it read.
    pub(crate) fn commit(&mut self) {
        let Some(slot) = self.shared.backends.get(self.backend) else { return };
        for (words, keep) in self.out.motion.drain(..) {
            self.motion.push(&slot.motion, words, keep);
        }
        for (lane, words, keep) in self.out.hits.drain(..) {
            match lane {
                Lane::Intent => self.intents.push(&slot.intents, words, keep),
                Lane::Binding => self.bindings.push(&slot.bindings, words, keep),
            }
        }
        self.motion.flush(&slot.motion);
        self.intents.flush(&slot.intents);
        self.bindings.flush(&slot.bindings);
        for src in self.out.free.drain(..) {
            self.shared.free_source(src);
        }
    }

    /// Follows the epochs: ends the run when they close or move, begins one when they open. `false` while closed.
    fn sync_epoch(&mut self) -> bool {
        let capture = self.shared.capture.get();
        let backend = self.shared.backends.get(self.backend).map_or(0, |s| s.epoch.get());
        if !(is_open(capture) && is_open(backend)) {
            if self.running {
                self.end_run();
            }
            return false;
        }
        if self.seen != (capture, backend) {
            if self.running {
                self.end_run();
            }
            self.begin_run(capture, backend);
        }
        let generation = self.shared.detail_generation.load(Ordering::Acquire);
        if generation != self.detail.1 {
            self.detail = (self.shared.detail(), generation);
        }
        true
    }

    /// Design §4 step 3: discards what was not pushed, clears the held snapshots and the modifiers, and marks every
    /// held key unknown, so nothing read before the pause and no key held across it reaches a consumer.
    fn end_run(&mut self) {
        self.motion.clear();
        self.intents.clear();
        self.bindings.clear();
        self.out.motion.clear();
        self.out.hits.clear();
        self.presses.clear();
        self.pointer.clear();
        self.mods = [0; 4];
        let generation = self.table.generation;
        for device in &mut self.devices {
            for key in &mut *device.keys {
                if *key == DOWN {
                    *key = UNKNOWN;
                }
            }
            device.unknown_buttons |= device.buttons;
            device.unknown_zones = device.unknown_zones.or(device.zones);
            if let Some(snapshot) = self.shared.snapshots.get(usize::from(device.src.index())) {
                snapshot.write(generation, 0);
            }
        }
        self.running = false;
    }

    /// A new run: devices already connected announce themselves again, so consumers know them after the reset.
    fn begin_run(&mut self, capture: u32, backend: u32) {
        self.seen = (capture, backend);
        self.running = true;
        let at = self.at();
        for i in 0..self.devices.len() {
            if let Some(d) = self.devices.get(i) {
                let event = MotionEvent::source_up(d.src, d.class, d.pad, at);
                self.emit(event, Keep::Always);
            }
        }
    }

    fn sync_table(&mut self) {
        if self.shared.tables.generation() != self.table.generation {
            self.table = self.shared.tables.current();
            let sources: Vec<SourceTag> = self.devices.iter().map(|d| d.src).collect();
            for src in sources {
                self.write_snapshot(src);
            }
        }
    }

    fn period(&self) -> i64 {
        (self.shared.now().0 / PERIOD).floor() as i64
    }

    /// Now, quantised to the coalescing period.
    fn at(&self) -> Seconds {
        Seconds((self.shared.now().0 / PERIOD).floor() * PERIOD)
    }

    fn stamp(&self) -> u64 {
        stamp(self.seen.0, self.seen.1)
    }

    fn emit(&mut self, event: MotionEvent, keep: Keep) {
        let words = entry::motion(self.stamp(), self.detail.1, &event);
        self.out.motion.push((words, keep));
    }

    fn hit(&mut self, press: &Press, edge: Edge, at: Seconds) {
        let entry = HitEntry {
            generation: press.generation,
            slot: press.slot,
            src: press.src,
            id: press.id,
            edge,
            at,
            intent: press.intent,
        };
        let pair = u32::from(press.src.index()) << 24 ^ press.id.0;
        let keep = match edge {
            Edge::Press => Keep::Press { pair },
            Edge::Release => Keep::Release { pair },
            Edge::Repeat => Keep::Cheap,
        };
        let words = entry::hit(self.stamp(), &entry);
        self.out.hits.push((press.lane, words, keep));
    }

    fn device_index(&self, handle: u64) -> Option<usize> {
        self.devices.iter().position(|d| d.handle == handle)
    }

    /// The device for `handle`, connecting it as `class` if the backend never announced it.
    fn device(&mut self, handle: u64, class: DeviceClass) -> Option<usize> {
        if let Some(i) = self.device_index(handle) {
            return Some(i);
        }
        self.device_up(handle, class);
        self.device_index(handle)
    }

    fn device_up(&mut self, handle: u64, class: DeviceClass) {
        if self.device_index(handle).is_some() {
            return;
        }
        let Some(src) = self.shared.take_source() else { return };
        let pad = if class == DeviceClass::Gamepad { self.shared.take_pad() } else { None };
        if let Some(snapshot) = self.shared.snapshots.get(usize::from(src.index())) {
            snapshot.hand_over();
        }
        self.devices.push(Device {
            handle,
            src,
            class,
            keys: Box::new([UP; 256]),
            pad,
            buttons: 0,
            zones: PadZones::NONE,
            unknown_buttons: 0,
            unknown_zones: PadZones::NONE,
        });
        self.publish_sources();
        let at = self.at();
        self.emit(MotionEvent::source_up(src, class, pad, at), Keep::Always);
    }

    fn device_gone(&mut self, handle: u64) {
        let Some(i) = self.device_index(handle) else { return };
        let device = self.devices.remove(i);
        self.flush_pointer(|acc| acc.src == device.src);
        let at = self.at();
        let ended: Vec<Press> = self.presses.iter().filter(|p| p.src == device.src).copied().collect();
        self.presses.retain(|p| p.src != device.src);
        for press in &ended {
            self.hit(press, Edge::Release, at);
        }
        for (bit, count) in self.mods.iter_mut().enumerate() {
            let held = device.keys.iter().enumerate().filter(|(usage, state)| {
                **state == DOWN && Mods::of(HidUsage(u16::try_from(*usage).unwrap_or(0))) == 1 << bit
            });
            *count = count.saturating_sub(u8::try_from(held.count()).unwrap_or(u8::MAX));
        }
        if let Some(pad) = device.pad {
            self.shared.free_pad(pad);
        }
        self.write_snapshot(device.src);
        self.emit(MotionEvent::source_gone(device.src, at), Keep::Always);
        self.publish_sources();
        self.out.free.push(device.src);
    }

    fn publish_sources(&self) {
        let bits = self.devices.iter().fold(0u64, |bits, d| bits | 1 << d.src.index());
        if let Some(slot) = self.shared.backends.get(self.backend) {
            slot.sources.store(bits, Ordering::Release);
        }
    }

    fn write_snapshot(&self, src: SourceTag) {
        let slots = self
            .presses
            .iter()
            .filter(|p| p.src == src)
            .filter_map(|p| self.table.slot_of(p.id))
            .fold(0u64, |bits, slot| bits | 1 << slot);
        if let Some(snapshot) = self.shared.snapshots.get(usize::from(src.index())) {
            snapshot.write(self.table.generation, slots);
        }
    }

    fn mods(&self, except: u8) -> Mods {
        let held = self.mods.iter().enumerate().fold(0u8, |m, (bit, n)| if *n > 0 { m | 1 << bit } else { m });
        Mods(held & !except)
    }

    /// The zone a key's motion uses under the current detail.
    fn zone(&self, usage: HidUsage) -> KeyZone {
        let zone = zone_map::zone(usage);
        match self.detail.0 {
            Detail::Zones => zone,
            Detail::HandOnly => {
                let info = zone.info();
                let side = info.side.unwrap_or(if info.centre[0] < 1.0 / 3.0 { Side::Left } else { Side::Right });
                KeyZone::side_only(side)
            }
        }
    }

    fn key(&mut self, handle: u64, usage: HidUsage, down: bool, repeat: bool) {
        let Some(i) = self.device(handle, DeviceClass::Keyboard) else { return };
        let index = usize::from(usage.0);
        let Some((src, state)) = self.devices.get(i).and_then(|d| d.keys.get(index).map(|s| (d.src, *s))) else {
            return;
        };
        let modifier = Mods::of(usage);
        let set = |sink: &mut Self, value: u8| {
            if let Some(key) = sink.devices.get_mut(i).and_then(|d| d.keys.get_mut(index)) {
                *key = value;
            }
        };
        let count = |sink: &mut Self, up: bool| {
            if let Some(n) = (0..4).find(|b| modifier == 1 << b).and_then(|b| sink.mods.get_mut(b)) {
                *n = if up { n.saturating_add(1) } else { n.saturating_sub(1) };
            }
        };
        self.flush_pointer(|_| true);
        let at = self.at();
        let pair = u32::from(src.index()) << 16 | u32::from(self.zone(usage).index());
        match (down, state) {
            // Held across a pause (or an auto-repeat of a key never seen down): nothing until its up.
            (true, UNKNOWN | UP) if repeat || state == UNKNOWN => {}
            (true, DOWN) => {
                if let Some(press) = self.press_of(src, PressKey::Key(usage)) {
                    self.hit(&press, Edge::Repeat, at);
                }
            }
            (true, _) => {
                set(self, DOWN);
                let mods = self.mods(modifier);
                count(self, true);
                self.emit(MotionEvent::key(src, self.zone(usage), true, at), Keep::Press { pair });
                if !self.learn(Chord::Key { usage, mods }, modifier != 0) {
                    let slot = matcher::match_key(&self.table, usage, mods);
                    self.start_press(src, PressKey::Key(usage), slot, at);
                }
            }
            (false, UNKNOWN) => set(self, UP),
            (false, DOWN) => {
                set(self, UP);
                count(self, false);
                self.emit(MotionEvent::key(src, self.zone(usage), false, at), Keep::Release { pair });
                self.end_press(src, PressKey::Key(usage), at);
            }
            (false, _) => {}
        }
    }

    /// A pending `learn` takes this chord (a modifier alone is not a chord); `true` when it did.
    fn learn(&self, chord: Chord, modifier_only: bool) -> bool {
        let learn = &self.shared.learn;
        let wanted = learn.wanted.load(Ordering::Acquire);
        if wanted == 0 || modifier_only {
            return false;
        }
        if learn.wanted.compare_exchange(wanted, 0, Ordering::AcqRel, Ordering::Acquire).is_err() {
            return false;
        }
        *lock(&learn.answer) = Some((BindingId(wanted - 1), chord));
        learn.done.notify_all();
        true
    }

    fn press_of(&self, src: SourceTag, key: PressKey) -> Option<Press> {
        self.presses.iter().find(|p| p.src == src && p.key == key).copied()
    }

    fn start_press(&mut self, src: SourceTag, key: PressKey, slot: Option<usize>, at: Seconds) {
        let Some((slot, entry)) = slot.and_then(|s| self.table.entries.get(s).map(|e| (s, e))) else { return };
        let intent = entry.action.as_intent();
        let press = Press {
            src,
            key,
            lane: if intent.is_some() { Lane::Intent } else { Lane::Binding },
            generation: self.table.generation,
            slot: u8::try_from(slot).unwrap_or(u8::MAX),
            id: entry.id,
            intent,
        };
        if self.presses.len() < self.presses.capacity() {
            self.presses.push(press);
            self.hit(&press, Edge::Press, at);
            self.write_snapshot(src);
        }
    }

    fn end_press(&mut self, src: SourceTag, key: PressKey, at: Seconds) {
        if let Some(i) = self.presses.iter().position(|p| p.src == src && p.key == key) {
            let press = self.presses.swap_remove(i);
            self.hit(&press, Edge::Release, at);
            self.write_snapshot(src);
        }
    }

    fn flush_pointer(&mut self, which: impl Fn(&Accumulated) -> bool) {
        let mut i = 0;
        while let Some(acc) = self.pointer.get(i).copied() {
            if which(&acc) {
                self.pointer.swap_remove(i);
                let at = Seconds(acc.period as f64 * PERIOD);
                self.emit(MotionEvent::pointer(acc.src, acc.dx, acc.dy, acc.wheel, at), Keep::Cheap);
            } else {
                i += 1;
            }
        }
    }

    fn motion(&mut self, handle: u64, dx: i32, dy: i32, wheel: f32) {
        let Some(src) = self.device(handle, DeviceClass::Mouse).and_then(|i| self.devices.get(i)).map(|d| d.src) else {
            return;
        };
        let period = self.period();
        self.flush_pointer(|acc| acc.src == src && acc.period != period);
        let (dx, dy) = (dx as f32 * METRES_PER_COUNT, -(dy as f32) * METRES_PER_COUNT);
        match self.pointer.iter_mut().find(|acc| acc.src == src) {
            Some(acc) => {
                acc.dx += dx;
                acc.dy += dy;
                acc.wheel += wheel;
            }
            None => self.pointer.push(Accumulated { src, period, dx, dy, wheel }),
        }
    }

    fn buttons(&mut self, handle: u64, class: DeviceClass, pressed: u8, released: u8) {
        let Some(src) = self.device(handle, class).and_then(|i| self.devices.get(i)).map(|d| d.src) else { return };
        self.flush_pointer(|acc| acc.src == src);
        let at = self.at();
        let pair = u32::from(src.index()) << 16 | 0x8000 | u32::from(pressed | released);
        let keep = if released != 0 { Keep::Release { pair } } else { Keep::Press { pair } };
        self.emit(MotionEvent::buttons(src, class, pressed, released, at), keep);
    }

    fn pen(&mut self, handle: u64, position: [f32; 2], pressure: f32, tilt: [f32; 2], in_range: bool, contact: bool) {
        let Some(src) = self.device(handle, DeviceClass::Pen).and_then(|i| self.devices.get(i)).map(|d| d.src) else {
            return;
        };
        let cell = |v: f32| (v.clamp(0.0, 1.0) * 32.0).min(31.0) as u8;
        let degrees = |v: f32| v.round().clamp(-127.0, 127.0) as i8;
        let at = self.at();
        let event = MotionEvent::pen(
            src,
            cell(position[0]),
            cell(position[1]),
            (pressure.clamp(0.0, 1.0) * 255.0).round() as u8,
            [degrees(tilt[0]), degrees(tilt[1])],
            in_range,
            contact,
            at,
        );
        self.emit(event, Keep::Cheap);
    }

    fn pad(&mut self, handle: u64, buttons: u32, zones: PadZones, sticks: [[f32; 2]; 2], triggers: [f32; 2]) {
        let Some(i) = self.device(handle, DeviceClass::Gamepad) else { return };
        let Some(device) = self.devices.get_mut(i) else { return };
        // Held across a pause: ignored until released.
        device.unknown_buttons &= buttons;
        device.unknown_zones = PadZones::from_bits(device.unknown_zones.bits() & zones.bits());
        let (buttons, zones) = (buttons & !device.unknown_buttons, zones.without(device.unknown_zones));
        let (src, pad, before_buttons, before_zones) = (device.src, device.pad, device.buttons, device.zones);
        device.buttons = buttons;
        device.zones = zones;
        let at = self.at();
        let Some(pad) = pad else { return };
        let (pressed, released) = (zones.without(before_zones), before_zones.without(zones));
        if !pressed.is_empty() || !released.is_empty() {
            let pair = u32::from(pad.index()) << 16 | u32::from(pressed.or(released).bits());
            let keep = if released.is_empty() { Keep::Press { pair } } else { Keep::Release { pair } };
            self.emit(MotionEvent::pad_edges(pad, pressed, released, at), keep);
        }
        let levels = PadState {
            pad,
            sticks: sticks.map(|s| s.map(quantise_stick)),
            triggers: triggers.map(|t| (t.clamp(0.0, 1.0) * 255.0).round() as u8),
            zones,
            at,
        };
        if let Some(slot) = self.shared.pads.get(usize::from(pad.index())) {
            slot.write(self.backend, self.stamp(), &levels);
        }
        // A pad chord ends when any of its buttons goes up; a new button down may start one.
        if let Some(press) = self.presses.iter().find(|p| {
            p.src == src && matches!(p.key, PressKey::Pad(held) if held & buttons != held)
        }) {
            let key = press.key;
            self.end_press(src, key, at);
        }
        if buttons & !before_buttons != 0 && !self.learn(Chord::Pad { buttons }, false) {
            let slot = matcher::match_pad(&self.table, buttons);
            self.start_press(src, PressKey::Pad(buttons), slot, at);
        }
    }
}

/// A stick axis in `-1..=1` to `i8`, with the dead zone removed and the rest rescaled so the edges are exact.
pub(crate) fn quantise_stick(value: f32) -> i8 {
    let magnitude = value.abs().min(1.0);
    if magnitude <= DEAD_ZONE {
        return 0;
    }
    let scaled = (magnitude - DEAD_ZONE) / (1.0 - DEAD_ZONE) * 127.0;
    (scaled.round() as i8).saturating_mul(if value < 0.0 { -1 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sticks_are_exact_at_the_edges_of_the_dead_zone_and_the_travel() {
        assert_eq!(quantise_stick(0.0), 0);
        assert_eq!(quantise_stick(DEAD_ZONE), 0);
        assert_eq!(quantise_stick(-DEAD_ZONE), 0);
        assert_eq!(quantise_stick(1.0), 127);
        assert_eq!(quantise_stick(-1.0), -127);
        assert_eq!(quantise_stick(2.0), 127);
        assert!(quantise_stick(DEAD_ZONE + 0.01) > 0);
    }

    #[test]
    fn a_raw_value_never_prints() {
        assert_eq!(format!("{:?}", HidUsage(0x42)), "<key>");
        let raw = RawEvent::Key { device: 1, usage: HidUsage(0x42), down: true, repeat: false };
        assert_eq!(format!("{raw:?}"), "<raw input>");
    }
}
