//! [`FakeInput`]: the whole input hub as one scripted value, so a consumer (motion, locomotion, the indicator, the
//! binding editor, the ops) tests against it before any OS backend exists.
//!
//! **Scripting.** Motion events are pushed per backend name and drained the way the hub drains its rings: each
//! backend's queue in its order, the backends in turn by name, nothing after `up_to`, nothing dropped. Binding hits are
//! resolved when scripted, as the hub's feeds resolve them: a press records the action it started, its release and
//! repeats carry that action whatever the table says by then, and a release with no press is never handed out.
//!
//! **Capture.** The feeds deliver scripted events whether or not capture was started, so a motion test needs no
//! indicator. What capture does is modelled where consumers see it: [`InputControl::start`] needs a shown indicator;
//! a pause or stop (by the control, a lost indicator in [`FakeInput::tick`] or a protected program) drops what is
//! queued, ends every held binding and intent with a release, resets every connected source in the next frames, hides
//! the pads, and ignores pushes until the next start.
//!
//! The mock locks a mutex inside `drain`; the real hub's rings never lock. `drain` allocates nothing.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_input_api::binding::{
    BindingAction, BindingHit, BindingId, BindingInfo, ChordLabel, Edge, IntentEvent, MAX_BINDINGS,
};
use omuzaki_input_api::capture::{
    BackendReport, BackendState, CaptureCaps, CaptureReport, CaptureState, Detail, IndicatorHandle, IndicatorView,
    InputError, PauseWhy,
};
use omuzaki_input_api::event::{
    DeviceClass, DeviceMask, MAX_PADS, MotionEvent, PadId, PadSet, PadState, SourceMask, SourceTag,
};
use omuzaki_input_api::feed::{
    BindingFeed, ChordStore, Feeds, IndicatorHost, InputControl, InputHub, IntentFeed, IntentFrame, MotionFeed,
    MotionFrame, ProtectList,
};

/// The scripted input hub. Cheap to clone; every clone shares the script and the state.
pub struct FakeInput {
    inner: Arc<Inner>,
    /// The hit [`BindingFeed::poll`] last handed out, owned by this clone so the borrow outlives the lock.
    current: Option<Hit>,
}

impl Clone for FakeInput {
    fn clone(&self) -> Self {
        Self { inner: Arc::clone(&self.inner), current: None }
    }
}

impl std::fmt::Debug for FakeInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeInput").field("state", &self.lock().capture).finish_non_exhaustive()
    }
}

struct Inner {
    clock: Arc<FakeClock>,
    state: Mutex<State>,
    feeds_taken: AtomicBool,
}

#[derive(Clone)]
struct Hit {
    id: BindingId,
    action: BindingAction,
    edge: Edge,
    at: Seconds,
}

struct Backend {
    name: String,
    classes: DeviceMask,
    motion: VecDeque<MotionEvent>,
}

struct Binding {
    id: BindingId,
    name: String,
    device: DeviceClass,
    action: BindingAction,
    chord: Option<String>,
}

/// A preset: bindings with their chords, which stay inside the hub.
struct Preset {
    name: String,
    bindings: Vec<(String, DeviceClass, BindingAction, String)>,
}

/// Each connected source's class, by tag.
struct Sources([Option<DeviceClass>; SourceTag::MAX]);

impl Default for Sources {
    fn default() -> Self {
        Self([None; SourceTag::MAX])
    }
}

impl Sources {
    fn get(&self, index: usize) -> Option<&Option<DeviceClass>> {
        self.0.get(index)
    }

    fn get_mut(&mut self, index: usize) -> Option<&mut Option<DeviceClass>> {
        self.0.get_mut(index)
    }
}

#[derive(Default)]
struct State {
    capture: CaptureState,
    /// Capture was paused or stopped: queues dropped, pushes ignored, pads hidden, until the next start.
    suspended: bool,
    detail: Detail,
    caps: CaptureCaps,
    backends: Vec<Backend>,
    /// Each connected source's class.
    sources: Sources,
    pads: [Option<PadState>; MAX_PADS],
    motion_reset: SourceMask,
    intent_reset: SourceMask,
    intents: VecDeque<IntentEvent>,
    hits: VecDeque<Hit>,
    /// The action each held binding's press started.
    held: Vec<(BindingId, BindingAction)>,
    bindings: Vec<Binding>,
    next_id: u32,
    presets: Vec<Preset>,
    indicators: Vec<IndicatorHandle>,
    protected: Vec<String>,
    running: Option<String>,
    chords: VecDeque<String>,
    learnt: u32,
}

impl FakeInput {
    /// A hub with no backends, no bindings and capture off, timing on `clock`.
    pub fn new(clock: Arc<FakeClock>) -> Self {
        let mut state = State::default();
        state.presets.push(Preset {
            name: "locomotion".to_owned(),
            bindings: crate::LOCOMOTION_PRESET
                .iter()
                .map(|&(name, intent, chord)| {
                    (name.to_owned(), DeviceClass::Keyboard, BindingAction::Intent(intent), chord.to_owned())
                })
                .collect(),
        });
        let inner = Inner { clock, state: Mutex::new(state), feeds_taken: AtomicBool::new(false) };
        Self { inner: Arc::new(inner), current: None }
    }

    /// The clock it times on.
    pub fn clock(&self) -> &Arc<FakeClock> {
        &self.inner.clock
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.inner.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Queues `event` on `backend`'s ring (created on first use, serving the event's class). Ignored while suspended.
    pub fn push(&self, backend: &str, event: MotionEvent) {
        let mut state = self.lock();
        if state.suspended {
            return;
        }
        if let MotionEvent::SourceUp { src, class, .. } = event
            && let Some(slot) = state.sources.get_mut(usize::from(src.index()))
        {
            *slot = Some(class);
        }
        if let MotionEvent::SourceGone { src, .. } = event {
            forget_source(&mut state, src);
        }
        let class = match event {
            MotionEvent::Key { .. } => Some(DeviceClass::Keyboard),
            MotionEvent::Buttons { device, .. } => Some(device),
            MotionEvent::Pointer { .. } => Some(DeviceClass::Mouse),
            MotionEvent::Pen { .. } => Some(DeviceClass::Pen),
            MotionEvent::PadEdges { .. } => Some(DeviceClass::Gamepad),
            MotionEvent::SourceUp { class, .. } => Some(class),
            MotionEvent::SourceGone { .. } => None,
        };
        let backend = backend_mut(&mut state, backend);
        if let Some(class) = class {
            backend.classes = backend.classes.with(class);
        }
        backend.motion.push_back(event);
    }

    /// Queues every event of `events` on `backend`, in order.
    pub fn push_all(&self, backend: &str, events: impl IntoIterator<Item = MotionEvent>) {
        for event in events {
            self.push(backend, event);
        }
    }

    /// Connects a device: queues its [`MotionEvent::SourceUp`] and, for a pad, shows it neutral.
    pub fn connect(&self, backend: &str, src: SourceTag, class: DeviceClass, pad: Option<PadId>) {
        let at = self.inner.clock.now_seconds();
        self.push(backend, MotionEvent::source_up(src, class, pad, at));
        if let Some(pad) = pad {
            self.set_pad(PadState::neutral(pad, at));
        }
    }

    /// Disconnects a device: queues its [`MotionEvent::SourceGone`], after everything it sent.
    pub fn disconnect(&self, backend: &str, src: SourceTag) {
        self.push(backend, MotionEvent::source_gone(src, self.inner.clock.now_seconds()));
    }

    /// Sets a pad's levels (newest wins).
    pub fn set_pad(&self, levels: PadState) {
        if let Some(slot) = self.lock().pads.get_mut(usize::from(levels.pad.index())) {
            *slot = Some(levels);
        }
    }

    /// Removes a pad from the connected list.
    pub fn remove_pad(&self, pad: PadId) {
        if let Some(slot) = self.lock().pads.get_mut(usize::from(pad.index())) {
            *slot = None;
        }
    }

    /// A binding's chord went down, up or repeated at `at`. Resolved now, as the hub's feeds resolve it: an intent
    /// binding's hit goes to the intent feed, any other to the binding feed.
    pub fn hit(&self, id: BindingId, edge: Edge, at: Seconds) {
        let mut state = self.lock();
        if state.suspended {
            return;
        }
        let held = state.held.iter().position(|(held, _)| *held == id);
        let action = match (edge, held) {
            (Edge::Press, None) => {
                let Some(action) = state.bindings.iter().find(|b| b.id == id).map(|b| b.action.clone()) else {
                    return;
                };
                state.held.push((id, action.clone()));
                action
            }
            (Edge::Repeat, Some(i)) => match state.held.get(i) {
                Some((_, action)) => action.clone(),
                None => return,
            },
            (Edge::Release, Some(i)) => state.held.remove(i).1,
            // A second press of a held binding, or a release or repeat with no press: nothing reaches a consumer.
            (Edge::Press, Some(_)) | (Edge::Repeat | Edge::Release, None) => return,
        };
        queue_hit(&mut state, Hit { id, action, edge, at });
    }

    /// The label the next [`ChordStore::learn`] returns.
    pub fn script_chord(&self, text: &str) {
        self.lock().chords.push_back(text.to_owned());
    }

    /// Which program is running now, for the protected-program check of [`tick`](Self::tick).
    pub fn set_running(&self, program: Option<&str>) {
        self.lock().running = program.map(str::to_owned);
    }

    /// What works here, as [`InputControl::report`] says.
    pub fn set_caps(&self, caps: CaptureCaps) {
        self.lock().caps = caps;
    }

    /// The detail [`InputHub::set_detail`] last set.
    pub fn detail(&self) -> Detail {
        self.lock().detail
    }

    /// One tick of the hub's control thread: a stale indicator or a running protected program pauses capture, and
    /// their pauses undo themselves when the cause clears; then every indicator's view is written.
    pub fn tick(&self) {
        let now = self.inner.clock.now_seconds();
        let mut state = self.lock();
        let shown = state.indicators.iter().any(IndicatorHandle::is_shown);
        let protected = protected_running(&state);
        match state.capture {
            CaptureState::On { .. } if !shown => pause(&mut state, Some(PauseWhy::IndicatorLost), now),
            CaptureState::On { .. } if protected.is_some() => pause(&mut state, Some(PauseWhy::ProtectedApp), now),
            // The hub's own pauses undo themselves once their cause clears; a pause asked for does not.
            CaptureState::Paused { why: Some(_), .. } if shown && protected.is_none() => resume(&mut state),
            _ => {}
        }
        publish(&state);
    }

    /// Makes `backend` fail: its queue is dropped, its sources reset and their held bindings end; the others run on.
    pub fn fail_backend(&self, backend: &str) {
        let mut state = self.lock();
        let Some(i) = state.backends.iter().position(|b| b.name == backend) else { return };
        let gone = state.backends.remove(i);
        let mut sources = SourceMask::NONE;
        for event in &gone.motion {
            if let Some(src) = event.source() {
                sources = sources.with(src);
            }
        }
        for src in sources.iter() {
            forget_source(&mut state, src);
        }
        state.motion_reset = state.motion_reset.or(sources);
        if gone.classes.contains(DeviceClass::Gamepad) {
            state.pads = [None; MAX_PADS];
        }
    }
}

trait NowSeconds {
    fn now_seconds(&self) -> Seconds;
}

impl NowSeconds for FakeClock {
    fn now_seconds(&self) -> Seconds {
        omuzaki_core::time::Clock::now(self)
    }
}

fn backend_mut<'a>(state: &'a mut State, name: &str) -> &'a mut Backend {
    let at = state.backends.binary_search_by(|b| b.name.as_str().cmp(name)).unwrap_or_else(|at| {
        let backend = Backend { name: name.to_owned(), classes: DeviceMask::NONE, motion: VecDeque::new() };
        state.backends.insert(at, backend);
        at
    });
    let len = state.backends.len();
    // `at` is in range: it was found or just inserted.
    &mut state.backends[at.min(len - 1)]
}

fn forget_source(state: &mut State, src: SourceTag) {
    if let Some(slot) = state.sources.get_mut(usize::from(src.index())) {
        *slot = None;
    }
}

fn queue_hit(state: &mut State, hit: Hit) {
    match hit.action.as_intent() {
        Some(intent) => state.intents.push_back(IntentEvent { id: hit.id, intent, edge: hit.edge, at: hit.at }),
        None => state.hits.push_back(hit),
    }
}

fn protected_running(state: &State) -> Option<String> {
    state.running.as_ref().filter(|running| state.protected.iter().any(|p| p.eq_ignore_ascii_case(running))).cloned()
}

/// Ends everything held: queues dropped, held bindings and intents released, connected sources reset, pads hidden.
fn release_all(state: &mut State, at: Seconds) {
    for backend in &mut state.backends {
        backend.motion.clear();
    }
    state.intents.clear();
    state.hits.clear();
    for (id, action) in std::mem::take(&mut state.held) {
        queue_hit(state, Hit { id, action, edge: Edge::Release, at });
    }
    let connected = (0u8..64)
        .filter(|i| state.sources.get(usize::from(*i)).is_some_and(Option::is_some))
        .filter_map(SourceTag::new)
        .fold(SourceMask::NONE, SourceMask::with);
    state.motion_reset = state.motion_reset.or(connected);
    state.intent_reset = state.intent_reset.or(connected);
    state.suspended = true;
}

fn pause(state: &mut State, why: Option<PauseWhy>, at: Seconds) {
    release_all(state, at);
    state.capture = CaptureState::paused(why);
}

fn resume(state: &mut State) {
    let classes = match state.capture {
        CaptureState::On { classes, .. } => classes,
        _ => DeviceMask::ALL,
    };
    state.suspended = false;
    state.capture = CaptureState::on(classes, DeviceMask::NONE);
}

fn view(state: &State) -> IndicatorView {
    IndicatorView {
        state: state.capture.clone(),
        detail: state.detail,
        program: match state.capture {
            CaptureState::Paused { why: Some(PauseWhy::ProtectedApp), .. } => protected_running(state),
            _ => None,
        },
        backends: reports(state),
    }
}

fn publish(state: &State) {
    let view = view(state);
    for handle in &state.indicators {
        handle.publish(&view);
    }
}

fn reports(state: &State) -> Vec<BackendReport> {
    if !state.capture.is_on() {
        return Vec::new();
    }
    state
        .backends
        .iter()
        .map(|b| BackendReport { name: b.name.clone(), classes: b.classes, state: BackendState::Running })
        .collect()
}

impl MotionFeed for FakeInput {
    fn drain(&mut self, up_to: Seconds, out: &mut MotionFrame) {
        out.clear();
        let mut state = self.lock();
        out.add_reset(std::mem::take(&mut state.motion_reset));
        'backends: for backend in &mut state.backends {
            while let Some(event) = backend.motion.front().copied() {
                if event.at().0 > up_to.0 {
                    break;
                }
                if out.push(event).is_err() {
                    break 'backends;
                }
                backend.motion.pop_front();
            }
        }
    }

    fn pad(&self, pad: PadId) -> Option<PadState> {
        let state = self.lock();
        if state.suspended {
            return None;
        }
        state.pads.get(usize::from(pad.index())).copied().flatten()
    }

    fn pads(&self, out: &mut PadSet) {
        out.clear();
        let state = self.lock();
        if state.suspended {
            return;
        }
        for pad in state.pads.iter().flatten() {
            out.push(pad.pad);
        }
    }
}

impl IntentFeed for FakeInput {
    fn drain(&mut self, up_to: Seconds, out: &mut IntentFrame) {
        out.clear();
        let mut state = self.lock();
        out.add_reset(std::mem::take(&mut state.intent_reset));
        while let Some(event) = state.intents.front().copied() {
            if event.at.0 > up_to.0 || out.push(event).is_err() {
                break;
            }
            state.intents.pop_front();
        }
    }
}

impl BindingFeed for FakeInput {
    fn poll(&mut self) -> Option<BindingHit<'_>> {
        let next = self.lock().hits.pop_front();
        self.current = next;
        self.current.as_ref().map(|hit| BindingHit { id: hit.id, action: &hit.action, edge: hit.edge, at: hit.at })
    }
}

impl InputControl for FakeInput {
    fn start(&self, classes: DeviceMask) -> Result<(), InputError> {
        let now = self.inner.clock.now_seconds();
        let mut state = self.lock();
        if !state.indicators.iter().any(IndicatorHandle::is_shown) {
            return Err(InputError::NoIndicator);
        }
        state.suspended = false;
        state.capture = CaptureState::on(classes, DeviceMask::NONE);
        if protected_running(&state).is_some() {
            pause(&mut state, Some(PauseWhy::ProtectedApp), now);
        }
        publish(&state);
        Ok(())
    }

    fn pause(&self) {
        let now = self.inner.clock.now_seconds();
        let mut state = self.lock();
        if state.capture.is_on() {
            pause(&mut state, None, now);
        } else if let CaptureState::Paused { .. } = state.capture {
            // A pause asked for while the hub had paused stays until a person starts again.
            state.capture = CaptureState::paused(None);
        }
        publish(&state);
    }

    fn stop(&self) {
        let now = self.inner.clock.now_seconds();
        let mut state = self.lock();
        if state.capture != CaptureState::Off {
            release_all(&mut state, now);
            state.capture = CaptureState::Off;
        }
        publish(&state);
    }

    fn report(&self) -> CaptureReport {
        let state = self.lock();
        CaptureReport {
            state: state.capture.clone(),
            backends: reports(&state),
            caps: state.caps,
            detail: state.detail,
        }
    }

    fn protect(&self, programs: &[String]) {
        self.lock().protected = programs.to_vec();
    }

    fn bindings(&self) -> Vec<BindingInfo> {
        let state = self.lock();
        state
            .bindings
            .iter()
            .map(|b| BindingInfo {
                id: b.id,
                name: b.name.clone(),
                device: b.device,
                action: b.action.clone(),
                conflict: b.chord.is_some()
                    && state.bindings.iter().any(|other| other.id != b.id && other.chord == b.chord),
            })
            .collect()
    }

    fn add_binding(&self, name: &str, device: DeviceClass, action: BindingAction) -> Result<BindingId, InputError> {
        add(&mut self.lock(), name.to_owned(), device, action, None)
    }

    fn set_action(&self, id: BindingId, action: BindingAction) -> Result<(), InputError> {
        let mut state = self.lock();
        let binding = state.bindings.iter_mut().find(|b| b.id == id).ok_or(InputError::UnknownBinding(id))?;
        binding.action = action;
        Ok(())
    }

    fn remove(&self, id: BindingId) -> Result<(), InputError> {
        let mut state = self.lock();
        let at = state.bindings.iter().position(|b| b.id == id).ok_or(InputError::UnknownBinding(id))?;
        state.bindings.remove(at);
        if let Some(held) = state.held.iter().position(|(held, _)| *held == id) {
            let (id, action) = state.held.remove(held);
            let at = self.inner.clock.now_seconds();
            queue_hit(&mut state, Hit { id, action, edge: Edge::Release, at });
        }
        Ok(())
    }

    fn presets(&self) -> Vec<String> {
        self.lock().presets.iter().map(|p| p.name.clone()).collect()
    }

    fn preset_actions(&self, preset: &str) -> Result<Vec<BindingAction>, InputError> {
        let state = self.lock();
        let preset = state.presets.iter().find(|p| p.name == preset).ok_or_else(|| unknown_preset(preset))?;
        Ok(preset.bindings.iter().map(|(_, _, action, _)| action.clone()).collect())
    }

    fn apply_preset(&self, preset: &str) -> Result<Vec<BindingId>, InputError> {
        let mut state = self.lock();
        let bindings = state
            .presets
            .iter()
            .find(|p| p.name == preset)
            .map(|p| p.bindings.clone())
            .ok_or_else(|| unknown_preset(preset))?;
        bindings
            .into_iter()
            .map(|(name, device, action, chord)| add(&mut state, name, device, action, Some(chord)))
            .collect()
    }
}

fn unknown_preset(preset: &str) -> InputError {
    InputError::UnknownPreset(preset.to_owned())
}

fn add(
    state: &mut State,
    name: String,
    device: DeviceClass,
    action: BindingAction,
    chord: Option<String>,
) -> Result<BindingId, InputError> {
    if state.bindings.len() >= MAX_BINDINGS {
        return Err(InputError::TooManyBindings);
    }
    state.next_id += 1;
    let id = BindingId(state.next_id);
    state.bindings.push(Binding { id, name, device, action, chord });
    Ok(id)
}

impl IndicatorHost for FakeInput {
    fn install(&self) -> IndicatorHandle {
        let clock: Arc<dyn omuzaki_core::time::Clock> = Arc::clone(&self.inner.clock) as _;
        let handle = IndicatorHandle::new(clock);
        let mut state = self.lock();
        handle.publish(&view(&state));
        state.indicators.push(handle.clone());
        handle
    }
}

impl ChordStore for FakeInput {
    fn learn(&self, id: BindingId) -> Result<ChordLabel, InputError> {
        let mut state = self.lock();
        if !state.capture.is_on() || !state.indicators.iter().any(IndicatorHandle::is_shown) {
            return Err(InputError::NotCapturing);
        }
        if !state.bindings.iter().any(|b| b.id == id) {
            return Err(InputError::UnknownBinding(id));
        }
        state.learnt += 1;
        let learnt = state.learnt;
        let chord = state.chords.pop_front().unwrap_or_else(|| format!("Chord {learnt}"));
        if let Some(binding) = state.bindings.iter_mut().find(|b| b.id == id) {
            binding.chord = Some(chord.clone());
        }
        Ok(ChordLabel::new(chord))
    }

    fn label(&self, id: BindingId) -> Option<ChordLabel> {
        self.lock().bindings.iter().find(|b| b.id == id).and_then(|b| b.chord.clone()).map(ChordLabel::new)
    }

    fn clear(&self, id: BindingId) {
        if let Some(binding) = self.lock().bindings.iter_mut().find(|b| b.id == id) {
            binding.chord = None;
        }
    }

    fn conflicts(&self, id: BindingId, out: &mut Vec<BindingId>) {
        out.clear();
        let state = self.lock();
        let Some(chord) = state.bindings.iter().find(|b| b.id == id).and_then(|b| b.chord.as_ref()) else { return };
        out.extend(state.bindings.iter().filter(|b| b.id != id && b.chord.as_ref() == Some(chord)).map(|b| b.id));
    }
}

impl ProtectList for FakeInput {
    fn set(&self, programs: &[String]) {
        self.lock().protected = programs.to_vec();
    }

    fn get(&self) -> Vec<String> {
        self.lock().protected.clone()
    }

    fn matching(&self) -> Option<String> {
        protected_running(&self.lock())
    }
}

impl InputHub for FakeInput {
    fn control(&self) -> Arc<dyn InputControl> {
        Arc::new(self.clone())
    }

    fn indicator_host(&self) -> Arc<dyn IndicatorHost> {
        Arc::new(self.clone())
    }

    fn chord_store(&self) -> Arc<dyn ChordStore> {
        Arc::new(self.clone())
    }

    fn protect_list(&self) -> Arc<dyn ProtectList> {
        Arc::new(self.clone())
    }

    fn take_feeds(&self) -> Option<Feeds> {
        if self.inner.feeds_taken.swap(true, Ordering::AcqRel) {
            return None;
        }
        Some((Box::new(self.clone()), Box::new(self.clone()), Box::new(self.clone())))
    }

    fn set_detail(&self, detail: Detail) {
        let mut state = self.lock();
        if state.detail == detail {
            return;
        }
        state.detail = detail;
        // Zones resolved under the old detail must not meet holders kept under the new one (design §4).
        let keyboards = (0u8..64)
            .filter(|i| state.sources.get(usize::from(*i)).copied().flatten() == Some(DeviceClass::Keyboard))
            .filter_map(SourceTag::new)
            .fold(SourceMask::NONE, SourceMask::with);
        state.motion_reset = state.motion_reset.or(keyboards);
        for backend in &mut state.backends {
            backend.motion.retain(|e| !matches!(e, MotionEvent::Key { .. }));
        }
        publish(&state);
    }
}
