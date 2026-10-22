//! The feeds (design §2, §4, §5): what the frame thread drains and the dispatcher polls. Each reads every backend's
//! ring in turn, accepts only entries whose two epoch stamps are current and open, and on a new epoch first ends what it
//! holds. The intent and binding feeds keep a held record per binding with a set of holders (one bit per source), so
//! two devices holding one binding end it only when the last lets go, a release always ends the action its press
//! started, and a record left holding after a ring dropped something is reconciled against the sources' snapshots.
//!
//! The motion and intent feeds run on the frame thread: no lock, no allocation (fixed arrays). The binding feed runs on
//! the ops side and may lock and allocate (it resolves actions from the binding tables).

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use omuzaki_core::time::Seconds;
use omuzaki_input_api::binding::{BindingAction, BindingHit, BindingId, Edge, Intent, IntentEvent, MAX_BINDINGS};
use omuzaki_input_api::event::{DeviceClass, MAX_PADS, MotionEvent, PadId, PadSet, PadState, SourceMask, SourceTag};
use omuzaki_input_api::feed::{BindingFeed, IntentFeed, IntentFrame, MotionFeed, MotionFrame};

use crate::entry::{self, HitEntry};
use crate::epoch::{accepts, is_open};
use crate::rings::Ring;
use crate::shared::{BackendSlot, Shared};

/// The most backends a feed follows (fixed arrays on the frame thread).
pub(crate) const MAX_BACKENDS: usize = 16;

/// The epochs a feed last saw per backend, and whether that was an open run.
#[derive(Copy, Clone, Debug, Default)]
struct Seen {
    capture: u32,
    backend: u32,
}

impl Seen {
    fn open(self) -> bool {
        is_open(self.capture) && is_open(self.backend)
    }
}

/// Which backends' epochs moved since the last look: their indices as bits, and the epochs now.
fn moved(shared: &Shared, seen: &mut [Seen; MAX_BACKENDS]) -> u32 {
    let capture = shared.capture.get();
    let mut moved = 0;
    for (b, (slot, seen)) in shared.backends.iter().zip(seen.iter_mut()).enumerate() {
        let now = Seen { capture, backend: slot.epoch.get() };
        if now.capture != seen.capture || now.backend != seen.backend {
            if seen.open() {
                moved |= 1 << b;
            }
            *seen = now;
        }
    }
    moved
}

/// The motion feed.
pub(crate) struct Motion {
    shared: Arc<Shared>,
    seen: [Seen; MAX_BACKENDS],
    /// The sources each backend's entries named, so a closed epoch resets them.
    known: [SourceMask; MAX_BACKENDS],
    keyboards: SourceMask,
    detail: u32,
}

impl Motion {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        let detail = shared.detail_generation.load(Ordering::Acquire);
        Self { shared, seen: [Seen::default(); MAX_BACKENDS], known: [SourceMask::NONE; MAX_BACKENDS], keyboards: SourceMask::NONE, detail }
    }
}

impl MotionFeed for Motion {
    fn drain(&mut self, up_to: Seconds, out: &mut MotionFrame) {
        out.clear();
        let moved = moved(&self.shared, &mut self.seen);
        for (b, slot) in self.shared.backends.iter().enumerate() {
            if moved & 1 << b != 0
                && let Some(known) = self.known.get(b)
            {
                out.add_reset(known.or(slot.sources()));
            }
        }
        let detail = self.shared.detail_generation.load(Ordering::Acquire);
        if detail != self.detail {
            self.detail = detail;
            out.add_reset(self.keyboards);
        }
        let shared = Arc::clone(&self.shared);
        let capture = shared.capture.get();
        'backends: for (b, slot) in shared.backends.iter().enumerate() {
            let backend = slot.epoch.get();
            while let Some(words) = slot.motion.peek() {
                let stale = entry::motion_is_key(&words) && entry::motion_detail(&words) != detail;
                let Some(event) = entry::motion_event(&words).filter(|_| accepts(words[0], capture, backend) && !stale)
                else {
                    slot.motion.pop();
                    continue;
                };
                if event.at().0 > up_to.0 {
                    break;
                }
                if out.push(event).is_err() {
                    break 'backends;
                }
                slot.motion.pop();
                self.track(b, &event);
            }
        }
    }

    fn pad(&self, pad: PadId) -> Option<PadState> {
        let (backend, stamp, levels) = self.shared.pads.get(usize::from(pad.index()))?.read(pad)?;
        let epoch = self.shared.backends.get(backend)?.epoch.get();
        accepts(stamp, self.shared.capture.get(), epoch).then_some(levels)
    }

    fn pads(&self, out: &mut PadSet) {
        out.clear();
        for pad in (0..MAX_PADS).filter_map(|i| u8::try_from(i).ok().and_then(PadId::new)) {
            if self.pad(pad).is_some() {
                out.push(pad);
            }
        }
    }
}

impl Motion {
    fn track(&mut self, b: usize, event: &MotionEvent) {
        let Some(known) = self.known.get_mut(b) else { return };
        match *event {
            MotionEvent::SourceUp { src, class, .. } => {
                *known = known.with(src);
                if class == DeviceClass::Keyboard {
                    self.keyboards = self.keyboards.with(src);
                }
            }
            MotionEvent::SourceGone { src, .. } => {
                *known = known.without(src);
                self.keyboards = self.keyboards.without(src);
            }
            MotionEvent::Key { src, .. } => {
                *known = known.with(src);
                self.keyboards = self.keyboards.with(src);
            }
            _ => {
                if let Some(src) = event.source() {
                    *known = known.with(src);
                }
            }
        }
    }
}

/// A held binding: who holds it and what its press started.
#[derive(Clone, Debug)]
struct Held<A> {
    id: BindingId,
    holders: SourceMask,
    /// The action its first press started: what every repeat and the release carry.
    action: A,
}

/// The holder bookkeeping both hit feeds share.
struct Holders<A: Clone> {
    shared: Arc<Shared>,
    seen: [Seen; MAX_BACKENDS],
    /// Which backend each source belongs to (index plus one; 0 unknown).
    owner: [u8; SourceTag::MAX],
    held: Vec<Held<A>>,
    table: u32,
}

/// What a hit entry does to the held records: the edge to hand out, if any, and the record's action.
enum Outcome<A> {
    Out(Edge, A),
    Nothing,
}

impl<A: Clone> Holders<A> {
    fn new(shared: Arc<Shared>) -> Self {
        let table = shared.tables.generation();
        Self { shared, seen: [Seen::default(); MAX_BACKENDS], owner: [0; SourceTag::MAX], held: Vec::with_capacity(MAX_BINDINGS), table }
    }

    /// Applies one entry: a press from a new holder starts the action only when it is the first, a release ends it
    /// only when it is the last, a repeat repeats the recorded action, and anything from a source that holds nothing
    /// is ignored. `start` gives the action a first press starts (`None` drops the entry).
    fn apply(&mut self, b: usize, hit: &HitEntry, start: impl FnOnce(&HitEntry) -> Option<A>) -> Outcome<A> {
        if let Some(owner) = self.owner.get_mut(usize::from(hit.src.index())) {
            *owner = u8::try_from(b + 1).unwrap_or(0);
        }
        let record = self.held.iter().position(|h| h.id == hit.id);
        match (hit.edge, record) {
            (Edge::Press, Some(i)) => {
                if let Some(held) = self.held.get_mut(i) {
                    held.holders = held.holders.with(hit.src);
                }
                Outcome::Nothing
            }
            // A press queued before its binding was removed: dropped and counted (design §5).
            (Edge::Press, None) if !self.shared.tables.has(hit.id) => {
                self.shared.stale_entries.fetch_add(1, Ordering::Relaxed);
                Outcome::Nothing
            }
            (Edge::Press, None) => match start(hit) {
                Some(action) if self.held.len() < MAX_BINDINGS => {
                    let holders = SourceMask::of(hit.src);
                    self.held.push(Held { id: hit.id, holders, action: action.clone() });
                    Outcome::Out(Edge::Press, action)
                }
                _ => Outcome::Nothing,
            },
            (Edge::Repeat, Some(i)) => match self.held.get(i) {
                Some(held) if held.holders.contains(hit.src) => Outcome::Out(Edge::Repeat, held.action.clone()),
                _ => Outcome::Nothing,
            },
            (Edge::Release, Some(i)) => self.let_go(i, SourceMask::of(hit.src)),
            (Edge::Repeat | Edge::Release, None) => {
                if !self.shared.tables.has(hit.id) {
                    self.shared.stale_entries.fetch_add(1, Ordering::Relaxed);
                }
                Outcome::Nothing
            }
        }
    }

    /// Removes `sources` from record `i`'s holders; the action ends when none is left.
    fn let_go(&mut self, i: usize, sources: SourceMask) -> Outcome<A> {
        let Some(held) = self.held.get_mut(i) else { return Outcome::Nothing };
        if (held.holders.bits() & sources.bits()) == 0 {
            return Outcome::Nothing;
        }
        held.holders = SourceMask::from_bits(held.holders.bits() & !sources.bits());
        if held.holders.is_empty() {
            let held = self.held.swap_remove(i);
            return Outcome::Out(Edge::Release, held.action);
        }
        Outcome::Nothing
    }

    /// Ends what the moved epochs, a removed binding or a dropped entry leave held, calling `out` for each release.
    fn settle(&mut self, out: &mut impl FnMut(BindingId, A), ring: impl Fn(&BackendSlot) -> &Ring<{ entry::BINDING_WORDS }>) {
        let moved = moved(&self.shared, &mut self.seen);
        if moved != 0 {
            let mut gone = SourceMask::NONE;
            for (src, owner) in self.owner.iter().enumerate() {
                let owner = usize::from(*owner);
                if owner > 0
                    && moved & 1 << (owner - 1) != 0
                    && let Some(tag) = u8::try_from(src).ok().and_then(SourceTag::new)
                {
                    gone = gone.with(tag);
                }
            }
            self.release_from(gone, out);
        }
        let table = self.shared.tables.generation();
        if table != self.table {
            self.table = table;
            let mut i = 0;
            while let Some(held) = self.held.get(i) {
                if self.shared.tables.has(held.id) {
                    i += 1;
                } else {
                    let held = self.held.swap_remove(i);
                    out(held.id, held.action);
                }
            }
        }
        let dropped = self.shared.backends.iter().fold(false, |any, slot| ring(slot).take_dropped() | any);
        if dropped {
            self.reconcile(out);
        }
    }

    /// Ends every record held only by `sources`.
    fn release_from(&mut self, sources: SourceMask, out: &mut impl FnMut(BindingId, A)) {
        let mut i = 0;
        while let Some(id) = self.held.get(i).map(|h| h.id) {
            match self.let_go(i, sources) {
                Outcome::Out(_, action) => out(id, action),
                Outcome::Nothing => i += 1,
            }
        }
    }

    /// Design §5: after a ring dropped something, every holder whose source's snapshot shows the binding up is
    /// cleared. A slot held in a snapshot with no record is left alone: a missed press is not replayed.
    fn reconcile(&mut self, out: &mut impl FnMut(BindingId, A)) {
        let mut i = 0;
        while let Some(held) = self.held.get(i) {
            let mut up = SourceMask::NONE;
            for src in held.holders.iter() {
                let Some(snapshot) = self.shared.snapshots.get(usize::from(src.index())) else { continue };
                let (generation, slots) = snapshot.read();
                let Some(table) = self.shared.tables.at(generation) else { continue };
                let still = table.slot_of(held.id).is_some_and(|slot| slots & 1 << slot != 0);
                if !still {
                    up = up.with(src);
                }
            }
            let id = held.id;
            match self.let_go(i, up) {
                Outcome::Out(_, action) => out(id, action),
                Outcome::Nothing => i += 1,
            }
        }
    }
}

/// The frame thread's intent feed.
pub(crate) struct Intents {
    holders: Holders<Intent>,
    /// Releases made by `settle` that did not fit the frame, handed out first next time.
    pending: [Option<IntentEvent>; MAX_BINDINGS],
}

impl Intents {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { holders: Holders::new(shared), pending: [None; MAX_BINDINGS] }
    }
}

impl IntentFeed for Intents {
    fn drain(&mut self, up_to: Seconds, out: &mut IntentFrame) {
        out.clear();
        let now = self.holders.shared.now();
        let pending = &mut self.pending;
        self.holders.settle(
            &mut |id, intent| {
                if let Some(slot) = pending.iter_mut().find(|p| p.is_none()) {
                    *slot = Some(IntentEvent { id, intent, edge: Edge::Release, at: now });
                }
            },
            |slot| &slot.intents,
        );
        for slot in &mut self.pending {
            if let Some(event) = *slot {
                if out.push(event).is_err() {
                    return;
                }
                *slot = None;
            }
        }
        let shared = Arc::clone(&self.holders.shared);
        let capture = shared.capture.get();
        'backends: for (b, slot) in shared.backends.iter().enumerate() {
            let backend = slot.epoch.get();
            while let Some(words) = slot.intents.peek() {
                let Some(hit) = entry::hit_of(&words).filter(|_| accepts(words[0], capture, backend)) else {
                    slot.intents.pop();
                    continue;
                };
                if hit.at.0 > up_to.0 || out.is_full() {
                    break 'backends;
                }
                slot.intents.pop();
                if let Outcome::Out(edge, intent) = self.holders.apply(b, &hit, |hit| hit.intent) {
                    let _fits = out.push(IntentEvent { id: hit.id, intent, edge, at: hit.at });
                }
            }
        }
    }
}

/// A hit the binding feed hands out: it owns the action, so the borrow outlives any table.
#[derive(Clone, Debug)]
struct Out {
    id: BindingId,
    action: BindingAction,
    edge: Edge,
    at: Seconds,
}

/// The dispatcher's binding feed.
pub(crate) struct Bindings {
    holders: Holders<BindingAction>,
    queue: VecDeque<Out>,
    current: Option<Out>,
}

impl Bindings {
    pub(crate) fn new(shared: Arc<Shared>) -> Self {
        Self { holders: Holders::new(shared), queue: VecDeque::new(), current: None }
    }

    /// The action an entry's press starts: from the table of its generation, checked by id; a binding gone from
    /// every kept table is dropped and counted.
    fn action(shared: &Shared, hit: &HitEntry) -> Option<BindingAction> {
        let from = |table: Option<std::sync::Arc<crate::shared::Table>>| {
            table.and_then(|t| t.entries.get(usize::from(hit.slot)).filter(|e| e.id == hit.id).map(|e| e.action.clone()))
        };
        let action = from(shared.tables.at(hit.generation)).or_else(|| {
            let current = shared.tables.current();
            current.slot_of(hit.id).and_then(|slot| current.entries.get(slot)).map(|e| e.action.clone())
        });
        if action.is_none() {
            shared.stale_entries.fetch_add(1, Ordering::Relaxed);
        }
        action
    }

    fn fill(&mut self) {
        let now = self.holders.shared.now();
        let queue = &mut self.queue;
        self.holders.settle(
            &mut |id, action| queue.push_back(Out { id, action, edge: Edge::Release, at: now }),
            |slot| &slot.bindings,
        );
        let shared = Arc::clone(&self.holders.shared);
        let capture = shared.capture.get();
        for (b, slot) in shared.backends.iter().enumerate() {
            let backend = slot.epoch.get();
            while let Some(words) = slot.bindings.peek() {
                slot.bindings.pop();
                let Some(hit) = entry::hit_of(&words).filter(|_| accepts(words[0], capture, backend)) else { continue };
                if let Outcome::Out(edge, action) = self.holders.apply(b, &hit, |hit| Self::action(&shared, hit)) {
                    self.queue.push_back(Out { id: hit.id, action, edge, at: hit.at });
                }
            }
        }
    }
}

impl BindingFeed for Bindings {
    fn poll(&mut self) -> Option<BindingHit<'_>> {
        if self.queue.is_empty() {
            self.fill();
        }
        self.current = self.queue.pop_front();
        self.current.as_ref().map(|out| BindingHit { id: out.id, action: &out.action, edge: out.edge, at: out.at })
    }
}
