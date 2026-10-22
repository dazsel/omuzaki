//! The hub (design §2, §4): the one value an App builds. It owns the shared state, the binding store, the protected
//! list and the installed indicators, starts one capture thread per backend when capture first needs it, and runs the
//! control thread: the indicator check and the kill switch, which read atomics and the process list only, so a hung
//! surface or a stalled feed can never hold a pause up.
//!
//! Capture's phase is the control state here; the `CaptureState` everyone sees is derived from it and from each
//! backend's own state, never set on its own. Every transition out of `On` closes the global epoch first and publishes
//! the new state right after, without waiting for a backend or a feed (§4 steps 1 to 4).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_core::time::{Clock, Seconds};
use omuzaki_input_api::binding::{BindingAction, BindingId, BindingInfo, ChordLabel};
use omuzaki_input_api::capture::{
    BackendReport, BackendState, CaptureCaps, CaptureReport, CaptureState, Detail, IndicatorHandle, IndicatorView,
    InputError, PauseWhy, Support,
};
use omuzaki_input_api::event::{DeviceClass, DeviceMask, MAX_PADS, PadId};
use omuzaki_input_api::feed::{ChordStore, Feeds, IndicatorHost, InputControl, InputHub, ProtectList};

use crate::dispatch::{Bindings, Intents, MAX_BACKENDS, Motion};
use crate::indicator::Indicators;
use crate::kill_switch::{self, OsProcesses, Processes};
use crate::normalise::RawSink;
use crate::protect::Protected;
use crate::shared::{BackendSlot, Shared, lock};
use crate::store::{PRESETS, Store};

/// How often the control thread looks at the indicators' heartbeats (§4 asks for at least once a second; a pause must
/// follow a stale heartbeat within 1.1 s).
const INDICATOR_EVERY: Duration = Duration::from_millis(250);
/// How often the kill switch reads the process list while it has a list (§4 asks for every 2 s at most; a pause must
/// follow a protected program's start within 2.1 s).
const PROTECT_EVERY: Seconds = Seconds(1.0);
/// How long `ChordStore::learn` waits for the person's chord.
const LEARN_WAIT: Duration = Duration::from_secs(10);

/// A capture backend: one per OS input API, registered next to its code with [`register_input_backend!`].
#[derive(Copy, Clone, Debug)]
pub(crate) struct InputBackend {
    /// Its stable name (`windows.raw_input`, `gamepad.gilrs`, ...): reports name it, and backends are drained in the
    /// order of their names.
    pub(crate) name: &'static str,
    /// The device classes it serves.
    pub(crate) classes: DeviceMask,
    /// What works through it on this OS.
    pub(crate) caps: fn() -> CaptureCaps,
    /// Its capture loop, run on its own thread: it reads the OS and hands each raw event to the sink until the sink
    /// says the hub is shutting down, parking with `RawSink::wait_reading` while capture is not on. `None` for a test
    /// backend fed by a `test-hooks` injector.
    pub(crate) run: Option<fn(RawSink)>,
}

inventory::collect!(InputBackend);

/// Registers a capture backend next to its code: `register_input_backend!(RAW_INPUT);` with a `const` [`InputBackend`].
/// There is no list of backends anywhere: a backend is its folder (§2).
#[allow(unused_macros)] // the first backends (IN-4 to IN-7) use it
macro_rules! register_input_backend {
    ($backend:ident) => {
        ::inventory::submit! { $backend }
        ::omuzaki_core::__record_registration!("input_backend", $backend);
    };
}
#[allow(unused_imports)]
pub(crate) use register_input_backend;

/// How to build a [`Hub`].
pub struct HubConfig {
    clock: Arc<dyn Clock>,
    data_dir: Option<PathBuf>,
    pub(crate) backends: Option<Vec<InputBackend>>,
    pub(crate) manual: bool,
    pub(crate) processes: Arc<dyn Processes>,
}

impl std::fmt::Debug for HubConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HubConfig").field("data_dir", &self.data_dir).field("manual", &self.manual).finish_non_exhaustive()
    }
}

impl HubConfig {
    /// A hub on `clock` (the pipeline's clock: event times are on it) that keeps its binding store and protected list
    /// in `data_dir` (the machine-local data folder), or in memory only with `None`.
    pub fn new(clock: Arc<dyn Clock>, data_dir: Option<PathBuf>) -> Self {
        Self { clock, data_dir, backends: None, manual: false, processes: Arc::new(OsProcesses) }
    }
}

/// Capture's phase: what the person and the actors asked for, and the hub's own pauses.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Phase {
    Off,
    On,
    /// `Some` for the hub's own pauses, which undo themselves; `None` for a `pause` asked through the control.
    Paused(Option<PauseWhy>),
}

#[derive(Debug)]
struct Control {
    phase: Phase,
    /// The classes the person asked for.
    classes: DeviceMask,
    /// The protected program found running, if one paused capture.
    program: Option<String>,
    /// When the kill switch reads the process list next.
    next_protect: Seconds,
    /// The view the indicators were last given.
    view: Option<IndicatorView>,
}

/// Everything behind the hub's handles.
pub(crate) struct Core {
    pub(crate) shared: Arc<Shared>,
    specs: Vec<InputBackend>,
    store: Store,
    protected: Arc<Protected>,
    indicators: Indicators,
    processes: Arc<dyn Processes>,
    control: Mutex<Control>,
    /// Serialises a store change with the table it installs.
    edits: Mutex<()>,
    threads: Mutex<Vec<Option<JoinHandle<()>>>>,
    feeds_taken: AtomicBool,
    caps: CaptureCaps,
    /// The control thread's sleep, cut short when the hub goes.
    stop_signal: (Mutex<bool>, Condvar),
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core").field("backends", &self.specs.len()).finish_non_exhaustive()
    }
}

/// The input hub: build one with [`Hub::new`], hand its handles out, and drop it to end every capture thread.
pub struct Hub {
    pub(crate) core: Arc<Core>,
    control_thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Hub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Hub").field("core", &self.core).finish_non_exhaustive()
    }
}

impl Hub {
    /// Builds the hub with the backends registered in this binary (at most 16), reads the binding store and the
    /// protected list, and starts the control thread. Capture starts `Off`, whatever happened before (§4).
    ///
    /// # Errors
    /// [`InputError::Store`] when the binding file exists and cannot be read.
    pub fn new(config: HubConfig) -> Result<Self, InputError> {
        let mut specs = config.backends.unwrap_or_else(|| inventory::iter::<InputBackend>.into_iter().copied().collect());
        specs.sort_by_key(|b| b.name);
        specs.truncate(MAX_BACKENDS);
        let slots = specs.iter().map(|_| BackendSlot::new()).collect();
        let shared = Arc::new(Shared::new(Arc::clone(&config.clock), slots));
        let store = Store::open(config.data_dir.as_deref())?;
        shared.tables.install(store.entries());
        let caps = merge_caps(&specs);
        let threads = specs.iter().map(|_| None).collect();
        let core = Arc::new(Core {
            shared,
            specs,
            store,
            protected: Arc::new(Protected::open(config.data_dir.as_deref())),
            indicators: Indicators::default(),
            processes: config.processes,
            control: Mutex::new(Control {
                phase: Phase::Off,
                classes: DeviceMask::NONE,
                program: None,
                next_protect: Seconds(0.0),
                view: None,
            }),
            edits: Mutex::new(()),
            threads: Mutex::new(threads),
            feeds_taken: AtomicBool::new(false),
            caps,
            stop_signal: (Mutex::new(false), Condvar::new()),
        });
        let control_thread = if config.manual {
            None
        } else {
            let core = Arc::clone(&core);
            std::thread::Builder::new()
                .name("input-control".to_owned())
                .spawn(move || core.control_loop())
                .ok()
        };
        Ok(Self { core, control_thread })
    }
}

impl Drop for Hub {
    fn drop(&mut self) {
        let core = &self.core;
        core.close_all();
        core.shared.shutdown.store(true, Ordering::Release);
        core.shared.wake_backends();
        {
            let mut stopped = lock(&core.stop_signal.0);
            *stopped = true;
            core.stop_signal.1.notify_all();
        }
        if let Some(thread) = self.control_thread.take() {
            let _ended = thread.join();
        }
    }
}

/// The best support any backend offers, per part.
fn merge_caps(specs: &[InputBackend]) -> CaptureCaps {
    let rank = |s: Support| match s {
        Support::Available => 4,
        Support::Portal => 3,
        Support::NeedsOptIn => 2,
        Support::NeedsPermission => 1,
        Support::Unavailable => 0,
    };
    let best = |a: Support, b: Support| if rank(b) > rank(a) { b } else { a };
    specs.iter().map(|b| (b.caps)()).fold(CaptureCaps::default(), |acc, c| CaptureCaps {
        typing: best(acc.typing, c.typing),
        pointer: best(acc.pointer, c.pointer),
        pen: best(acc.pen, c.pen),
        pen_pressure: best(acc.pen_pressure, c.pen_pressure),
        gamepad: best(acc.gamepad, c.gamepad),
        bindings: best(acc.bindings, c.bindings),
    })
}

/// Runs a backend's capture loop on its own thread. A panic is caught and reported as the backend's fixed failure
/// reason; on the way out the backend closes its own epoch, so the feeds end what its sources held (§4), and gives
/// its source tags and pads back.
pub(crate) fn spawn_capture(
    shared: Arc<Shared>,
    backend: usize,
    name: &str,
    run: impl FnOnce(RawSink) + Send + 'static,
) -> Option<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("input-{name}"))
        .spawn(move || {
            let sink = RawSink::new(Arc::clone(&shared), backend);
            let outcome = catch_unwind(AssertUnwindSafe(move || run(sink)));
            backend_ended(&shared, backend, outcome.is_err());
        })
        .ok()
}

/// A backend's thread ended: report it, close its epoch and free what it held.
pub(crate) fn backend_ended(shared: &Shared, backend: usize, panicked: bool) {
    let Some(slot) = shared.backends.get(backend) else { return };
    slot.epoch.close();
    if panicked {
        *lock(&slot.state) = Some(BackendState::failed("the backend stopped unexpectedly"));
    } else if !shared.shutdown.load(Ordering::Acquire) {
        let mut state = lock(&slot.state);
        if matches!(*state, Some(BackendState::Running) | None) {
            *state = Some(BackendState::failed("the backend ended"));
        }
    }
    for src in slot.sources().iter() {
        shared.free_source(src);
    }
    slot.sources.store(0, Ordering::Release);
    for pad in (0..MAX_PADS).filter_map(|i| u8::try_from(i).ok().and_then(PadId::new)) {
        let slot = shared.pads.get(usize::from(pad.index()));
        if slot.and_then(|p| p.read(pad)).is_some_and(|(owner, _, _)| owner == backend) {
            shared.free_pad(pad);
        }
    }
}

impl Core {
    fn control_loop(&self) {
        loop {
            self.tick();
            let stopped = lock(&self.stop_signal.0);
            let (stopped, _timeout) = self
                .stop_signal
                .1
                .wait_timeout_while(stopped, INDICATOR_EVERY, |stopped| !*stopped)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *stopped {
                return;
            }
        }
    }

    /// One pass of the control thread: the indicator check, the kill switch and the indicators' view.
    pub(crate) fn tick(&self) {
        let now = self.shared.now();
        let mut control = lock(&self.control);
        let shown = self.indicators.shown();
        match control.phase {
            Phase::On if !shown => self.hub_pause(&mut control, PauseWhy::IndicatorLost),
            Phase::Paused(Some(PauseWhy::IndicatorLost)) if shown => {
                control.next_protect = now;
                self.resume(&mut control);
            }
            _ => {}
        }
        let protecting = matches!(control.phase, Phase::On | Phase::Paused(Some(PauseWhy::ProtectedApp)));
        if protecting && now.0 >= control.next_protect.0 {
            control.next_protect = Seconds(now.0 + PROTECT_EVERY.0);
            let found = kill_switch::running(&self.protected.programs(), self.processes.as_ref());
            self.protected.set_running(found.clone());
            match (control.phase, found) {
                (Phase::On, Some(program)) => {
                    control.program = Some(program);
                    self.hub_pause(&mut control, PauseWhy::ProtectedApp);
                }
                (Phase::Paused(Some(PauseWhy::ProtectedApp)), None) => {
                    control.program = None;
                    if shown {
                        self.resume(&mut control);
                    } else {
                        control.phase = Phase::Paused(Some(PauseWhy::IndicatorLost));
                    }
                }
                _ => {}
            }
        }
        self.publish(&mut control);
    }

    /// The hub's own pause: undone by `tick` when its cause clears.
    fn hub_pause(&self, control: &mut Control, why: PauseWhy) {
        self.shared.capture.close();
        control.phase = Phase::Paused(Some(why));
        self.publish(control);
    }

    /// Opens a new capture epoch for the classes asked, unless a protected program runs.
    fn resume(&self, control: &mut Control) {
        if let Some(program) = kill_switch::running(&self.protected.programs(), self.processes.as_ref()) {
            self.protected.set_running(Some(program.clone()));
            control.program = Some(program);
            self.shared.capture.close();
            control.phase = Phase::Paused(Some(PauseWhy::ProtectedApp));
            return;
        }
        self.protected.set_running(None);
        control.program = None;
        self.shared.capture.open();
        control.phase = Phase::On;
        self.shared.wake_backends();
        self.publish(control);
    }

    /// Closes every epoch (the hub is going).
    fn close_all(&self) {
        let mut control = lock(&self.control);
        self.shared.capture.close();
        control.phase = Phase::Off;
        for slot in &self.shared.backends {
            slot.epoch.close();
        }
    }

    /// Opens the backends serving `classes` (starting their threads the first time) and closes the others.
    fn open_backends(&self, classes: DeviceMask) {
        let mut threads = lock(&self.threads);
        for (b, (spec, slot)) in self.specs.iter().zip(&self.shared.backends).enumerate() {
            if spec.classes.and(classes).is_empty() {
                slot.epoch.close();
                continue;
            }
            let Some(thread) = threads.get_mut(b) else { continue };
            match spec.run {
                // Not started yet, or ended (a failure): a person's start tries it again.
                Some(run) if thread.as_ref().is_none_or(JoinHandle::is_finished) => {
                    *lock(&slot.state) = Some(BackendState::Running);
                    slot.epoch.open();
                    *thread = spawn_capture(Arc::clone(&self.shared), b, spec.name, run);
                    if thread.is_none() {
                        *lock(&slot.state) = Some(BackendState::failed("the capture thread could not start"));
                        slot.epoch.close();
                    }
                }
                // Its thread runs (perhaps waiting for a permission): it reads again.
                Some(_) => {
                    slot.epoch.open();
                }
                // A test backend has no thread: it comes back as a restarted one would.
                None => {
                    let mut state = lock(&slot.state);
                    if !matches!(*state, Some(BackendState::Running | BackendState::NeedsPermission { .. })) {
                        *state = Some(BackendState::Running);
                    }
                    slot.epoch.open();
                }
            }
        }
    }

    fn state(&self, control: &Control) -> CaptureState {
        match control.phase {
            Phase::Off => CaptureState::Off,
            Phase::Paused(why) => CaptureState::paused(why),
            Phase::On => {
                let mut reading = DeviceMask::NONE;
                for (spec, slot) in self.specs.iter().zip(&self.shared.backends) {
                    let serves = spec.classes.and(control.classes);
                    if !serves.is_empty() && matches!(*lock(&slot.state), Some(BackendState::Running)) {
                        reading = reading.or(serves);
                    }
                }
                if reading.is_empty() {
                    CaptureState::failed("no input backend is running")
                } else {
                    CaptureState::on(reading, control.classes.without(reading))
                }
            }
        }
    }

    fn backends(&self) -> Vec<BackendReport> {
        self.specs
            .iter()
            .zip(&self.shared.backends)
            .filter_map(|(spec, slot)| {
                let state = lock(&slot.state).clone()?;
                Some(BackendReport { name: spec.name.to_owned(), classes: spec.classes, state })
            })
            .collect()
    }

    /// Writes the view to the indicators when it changed. Never waits for a surface: a view a surface holds right now
    /// is written on a later tick.
    fn publish(&self, control: &mut Control) {
        let view = IndicatorView {
            state: self.state(control),
            detail: self.shared.detail(),
            program: control.program.clone(),
            backends: self.backends(),
        };
        if control.view.as_ref() != Some(&view) {
            self.indicators.publish(&view);
            control.view = Some(view);
        }
    }

    fn install_table(&self) {
        self.shared.tables.install(self.store.entries());
    }

    fn edit<T>(&self, f: impl FnOnce(&Store) -> Result<T, InputError>) -> Result<T, InputError> {
        let _edits = lock(&self.edits);
        let value = f(&self.store)?;
        self.install_table();
        Ok(value)
    }

    /// Whether capture is on now.
    fn is_on(&self) -> bool {
        let control = lock(&self.control);
        self.state(&control).is_on()
    }

    /// Ends a pending `learn` (capture left `On`).
    fn cancel_learn(&self) {
        self.shared.learn.wanted.store(0, Ordering::Release);
        let _answer = lock(&self.shared.learn.answer);
        self.shared.learn.done.notify_all();
    }

    /// Sets a binding's chord directly (the `test-hooks` way to bind without a person at a keyboard).
    #[cfg(feature = "test-hooks")]
    pub(crate) fn set_chord(&self, id: BindingId, chord: Option<crate::matcher::Chord>) -> Result<(), InputError> {
        self.edit(|store| store.set_chord(id, chord))
    }

    /// Simulates a backend's thread failing: what `spawn_capture` does when a backend panics.
    #[cfg(feature = "test-hooks")]
    pub(crate) fn fail_backend(&self, backend: usize) {
        backend_ended(&self.shared, backend, true);
        let mut control = lock(&self.control);
        self.publish(&mut control);
    }
}

impl InputControl for Core {
    fn start(&self, classes: DeviceMask) -> Result<(), InputError> {
        let mut control = lock(&self.control);
        if !self.indicators.shown() {
            return Err(InputError::NoIndicator);
        }
        control.classes = classes;
        self.open_backends(classes);
        control.next_protect = Seconds(self.shared.now().0 + PROTECT_EVERY.0);
        self.resume(&mut control);
        self.publish(&mut control);
        Ok(())
    }

    fn pause(&self) {
        let mut control = lock(&self.control);
        if control.phase != Phase::Off {
            self.shared.capture.close();
            control.phase = Phase::Paused(None);
            self.publish(&mut control);
        }
        drop(control);
        self.cancel_learn();
    }

    fn stop(&self) {
        let mut control = lock(&self.control);
        self.shared.capture.close();
        control.phase = Phase::Off;
        control.program = None;
        self.publish(&mut control);
        drop(control);
        self.cancel_learn();
    }

    fn report(&self) -> CaptureReport {
        let control = lock(&self.control);
        CaptureReport {
            state: self.state(&control),
            backends: self.backends(),
            caps: self.caps,
            detail: self.shared.detail(),
        }
    }

    fn protect(&self, programs: &[String]) {
        self.protected.set(programs);
        let mut control = lock(&self.control);
        control.next_protect = self.shared.now();
    }

    fn bindings(&self) -> Vec<BindingInfo> {
        self.store.infos()
    }

    fn add_binding(&self, name: &str, device: DeviceClass, action: BindingAction) -> Result<BindingId, InputError> {
        self.edit(|store| store.add(name, device, &action, None))
    }

    fn set_action(&self, id: BindingId, action: BindingAction) -> Result<(), InputError> {
        self.edit(|store| store.set_action(id, &action))
    }

    fn remove(&self, id: BindingId) -> Result<(), InputError> {
        self.edit(|store| store.remove(id))
    }

    fn presets(&self) -> Vec<String> {
        PRESETS.iter().map(|p| p.name.to_owned()).collect()
    }

    fn preset_actions(&self, preset: &str) -> Result<Vec<BindingAction>, InputError> {
        let preset = PRESETS.iter().find(|p| p.name == preset).ok_or_else(|| InputError::UnknownPreset(preset.to_owned()))?;
        Ok(preset.bindings.iter().map(|(_, _, intent, _)| BindingAction::Intent(*intent)).collect())
    }

    fn apply_preset(&self, preset: &str) -> Result<Vec<BindingId>, InputError> {
        let preset = PRESETS.iter().find(|p| p.name == preset).ok_or_else(|| InputError::UnknownPreset(preset.to_owned()))?;
        self.edit(|store| store.apply(preset))
    }
}

impl IndicatorHost for Core {
    fn install(&self) -> IndicatorHandle {
        let mut control = lock(&self.control);
        let view = IndicatorView {
            state: self.state(&control),
            detail: self.shared.detail(),
            program: control.program.clone(),
            backends: self.backends(),
        };
        let handle = self.indicators.install(Arc::clone(&self.shared.clock), &view);
        control.view = Some(view);
        handle
    }
}

impl ChordStore for Core {
    fn learn(&self, id: BindingId) -> Result<ChordLabel, InputError> {
        if !self.is_on() {
            return Err(InputError::NotCapturing);
        }
        if !self.store.contains(id) {
            return Err(InputError::UnknownBinding(id));
        }
        let learn = &self.shared.learn;
        let mut answer = lock(&learn.answer);
        *answer = None;
        learn.wanted.store(id.0.wrapping_add(1), Ordering::Release);
        let deadline = std::time::Instant::now() + LEARN_WAIT;
        let chord = loop {
            if let Some((learned, chord)) = answer.take()
                && learned == id
            {
                break Some(chord);
            }
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            // A cleared request with no answer yet is either a chord on its way (the sink clears it, then answers
            // under this lock) or capture leaving `On` (`cancel_learn`).
            if left.is_zero() || (learn.wanted.load(Ordering::Acquire) == 0 && !self.is_on()) {
                break None;
            }
            answer = learn.done.wait_timeout(answer, left).map_or_else(|e| e.into_inner().0, |(guard, _)| guard);
        };
        drop(answer);
        let _cleared = learn.wanted.compare_exchange(id.0.wrapping_add(1), 0, Ordering::AcqRel, Ordering::Acquire);
        let Some(chord) = chord else {
            return Err(if self.is_on() {
                InputError::Unavailable("no chord was pressed in time".to_owned())
            } else {
                InputError::NotCapturing
            });
        };
        self.edit(|store| store.set_chord(id, Some(chord)))?;
        Ok(chord.label())
    }

    fn label(&self, id: BindingId) -> Option<ChordLabel> {
        self.store.chord(id).map(crate::matcher::Chord::label)
    }

    fn clear(&self, id: BindingId) {
        let _cleared = self.edit(|store| store.set_chord(id, None));
    }

    fn conflicts(&self, id: BindingId, out: &mut Vec<BindingId>) {
        out.clear();
        out.extend(self.store.conflicts(id));
    }
}

impl InputHub for Hub {
    fn control(&self) -> Arc<dyn InputControl> {
        Arc::clone(&self.core) as Arc<dyn InputControl>
    }

    fn indicator_host(&self) -> Arc<dyn IndicatorHost> {
        Arc::clone(&self.core) as Arc<dyn IndicatorHost>
    }

    fn chord_store(&self) -> Arc<dyn ChordStore> {
        Arc::clone(&self.core) as Arc<dyn ChordStore>
    }

    fn protect_list(&self) -> Arc<dyn ProtectList> {
        Arc::clone(&self.core.protected) as Arc<dyn ProtectList>
    }

    fn take_feeds(&self) -> Option<Feeds> {
        if self.core.feeds_taken.swap(true, Ordering::AcqRel) {
            return None;
        }
        let shared = &self.core.shared;
        Some((
            Box::new(Motion::new(Arc::clone(shared))),
            Box::new(Intents::new(Arc::clone(shared))),
            Box::new(Bindings::new(Arc::clone(shared))),
        ))
    }

    fn set_detail(&self, detail: Detail) {
        let shared = &self.core.shared;
        let value = u8::from(detail == Detail::HandOnly);
        if shared.detail.swap(value, Ordering::AcqRel) != value {
            shared.detail_generation.fetch_add(1, Ordering::AcqRel);
            let mut control = lock(&self.core.control);
            self.core.publish(&mut control);
        }
    }
}
