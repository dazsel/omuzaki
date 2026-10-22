//! The state the hub's threads share, each piece with one writer (design §5a rule 2): the epochs, each backend's rings
//! and owned sources, the per-source held snapshots, the pads, the detail, the binding tables and the learn request.

// The pad levels are packed into one word and read back: the narrowing casts are the unpacking.
#![allow(clippy::cast_possible_truncation)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

use omuzaki_core::time::{Clock, Seconds};
use omuzaki_input_api::binding::{BindingAction, BindingId, MAX_BINDINGS};
use omuzaki_input_api::capture::{BackendState, Detail};
use omuzaki_input_api::event::{MAX_PADS, PadId, PadState, PadZones, SourceMask, SourceTag};

use crate::entry::{BINDING_WORDS, MOTION_WORDS};
use crate::epoch::{Epoch, is_open};
use crate::matcher::Chord;
use crate::rings::{Ring, WriterCheck};

/// Entries a motion ring holds: 500 ms at the worst event rate (design §6.1).
pub(crate) const MOTION_RING: usize = 512;
/// Entries an intent or binding ring holds.
pub(crate) const BINDING_RING: usize = 64;
/// Retired binding tables kept for entries still queued against them.
const HISTORY: usize = 8;

/// Locks a mutex, taking the data of a poisoned one: every value behind these locks is valid between writes.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One backend's share: its epoch, rings, the sources it owns and its state.
#[derive(Debug)]
pub(crate) struct BackendSlot {
    /// Its own epoch (design §4): closed while it is not reading.
    pub(crate) epoch: Epoch,
    /// Motion events, to the frame thread.
    pub(crate) motion: Ring<MOTION_WORDS>,
    /// Locomotion intent hits, to the frame thread.
    pub(crate) intents: Ring<BINDING_WORDS>,
    /// Other binding hits, to the dispatcher.
    pub(crate) bindings: Ring<BINDING_WORDS>,
    /// The sources it owns now (written by its capture thread).
    pub(crate) sources: AtomicU64,
    /// Its state; `None` before it was first started.
    pub(crate) state: Mutex<Option<BackendState>>,
}

impl BackendSlot {
    pub(crate) fn new() -> Self {
        Self {
            epoch: Epoch::default(),
            motion: Ring::new(MOTION_RING),
            intents: Ring::new(BINDING_RING),
            bindings: Ring::new(BINDING_RING),
            sources: AtomicU64::new(0),
            state: Mutex::new(None),
        }
    }

    /// The sources it owns now.
    pub(crate) fn sources(&self) -> SourceMask {
        SourceMask::from_bits(self.sources.load(Ordering::Acquire))
    }
}

/// One source's held snapshot (design §5): the binding slots that device holds down now, in the table of `generation`.
/// Written only by the capture thread that owns the source.
#[derive(Debug, Default)]
pub(crate) struct Snapshot {
    generation: AtomicU32,
    slots: AtomicU64,
    writer: WriterCheck,
}

impl Snapshot {
    pub(crate) fn write(&self, generation: u32, slots: u64) {
        self.writer.check();
        self.slots.store(slots, Ordering::Relaxed);
        self.generation.store(generation, Ordering::Release);
    }

    pub(crate) fn read(&self) -> (u32, u64) {
        let generation = self.generation.load(Ordering::Acquire);
        (generation, self.slots.load(Ordering::Relaxed))
    }

    /// The source is now another device's (or another backend's): forget the writer.
    pub(crate) fn hand_over(&self) {
        self.writer.release();
        self.slots.store(0, Ordering::Relaxed);
    }
}

/// A pad's newest levels: a seqlock over three words, written by the gamepad backend only.
#[derive(Debug, Default)]
pub(crate) struct PadSlot {
    seq: AtomicU32,
    /// The owning backend's index plus one; 0 for no pad.
    owner: AtomicU32,
    levels: AtomicU64,
    at: AtomicU64,
    /// The epochs the levels were read in: levels from an earlier run never reach a consumer.
    stamp: AtomicU64,
}

impl PadSlot {
    pub(crate) fn write(&self, backend: usize, stamp: u64, levels: &PadState) {
        let word = u64::from(levels.sticks[0][0].cast_unsigned())
            | u64::from(levels.sticks[0][1].cast_unsigned()) << 8
            | u64::from(levels.sticks[1][0].cast_unsigned()) << 16
            | u64::from(levels.sticks[1][1].cast_unsigned()) << 24
            | u64::from(levels.triggers[0]) << 32
            | u64::from(levels.triggers[1]) << 40
            | u64::from(levels.zones.bits()) << 48;
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(1), Ordering::Release);
        self.owner.store(u32::try_from(backend + 1).unwrap_or(0), Ordering::Relaxed);
        self.levels.store(word, Ordering::Relaxed);
        self.at.store(levels.at.0.to_bits(), Ordering::Relaxed);
        self.stamp.store(stamp, Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    pub(crate) fn clear(&self) {
        let seq = self.seq.load(Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(1), Ordering::Release);
        self.owner.store(0, Ordering::Relaxed);
        self.seq.store(seq.wrapping_add(2), Ordering::Release);
    }

    /// The levels, the owning backend's index and the epochs they were read in; `None` when no pad is there.
    /// Wait-free: a read that races a write tries again a few times and then reports the pad as absent for this frame.
    pub(crate) fn read(&self, pad: PadId) -> Option<(usize, u64, PadState)> {
        for _ in 0..4 {
            let before = self.seq.load(Ordering::Acquire);
            if before % 2 == 1 {
                std::hint::spin_loop();
                continue;
            }
            let owner = self.owner.load(Ordering::Relaxed);
            let word = self.levels.load(Ordering::Relaxed);
            let at = self.at.load(Ordering::Relaxed);
            let stamp = self.stamp.load(Ordering::Relaxed);
            std::sync::atomic::fence(Ordering::Acquire);
            if self.seq.load(Ordering::Relaxed) != before {
                continue;
            }
            let backend = usize::try_from(owner).ok()?.checked_sub(1)?;
            let b = |shift: u32| (word >> shift) as u8;
            let levels = PadState {
                pad,
                sticks: [[b(0).cast_signed(), b(8).cast_signed()], [b(16).cast_signed(), b(24).cast_signed()]],
                triggers: [b(32), b(40)],
                zones: PadZones::from_bits((word >> 48) as u16),
                at: Seconds(f64::from_bits(at)),
            };
            return Some((backend, stamp, levels));
        }
        None
    }
}

/// One binding of a table: its id, its chord (private to the crate) and its action.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TableEntry {
    pub(crate) id: BindingId,
    pub(crate) chord: Option<Chord>,
    pub(crate) action: BindingAction,
}

/// A binding table: a slot per binding, swapped in whole.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Table {
    pub(crate) generation: u32,
    pub(crate) entries: Vec<TableEntry>,
}

impl Table {
    pub(crate) fn slot_of(&self, id: BindingId) -> Option<usize> {
        self.entries.iter().position(|e| e.id == id)
    }
}

/// The current binding table, its generation, the ids in it (readable without a lock) and the recent ones.
#[derive(Debug)]
pub(crate) struct Tables {
    current: Mutex<Arc<Table>>,
    generation: AtomicU32,
    ids: [AtomicU32; MAX_BINDINGS],
    history: Mutex<VecDeque<Arc<Table>>>,
}

impl Default for Tables {
    fn default() -> Self {
        Self {
            current: Mutex::new(Arc::new(Table::default())),
            generation: AtomicU32::new(0),
            ids: std::array::from_fn(|_| AtomicU32::new(0)),
            history: Mutex::new(VecDeque::with_capacity(HISTORY + 1)),
        }
    }
}

impl Tables {
    /// The generation of the current table.
    pub(crate) fn generation(&self) -> u32 {
        self.generation.load(Ordering::Acquire)
    }

    /// The current table (a capture thread re-reads it when the generation moves).
    pub(crate) fn current(&self) -> Arc<Table> {
        Arc::clone(&lock(&self.current))
    }

    /// Whether `id` is in the current table, without a lock (for the frame thread).
    pub(crate) fn has(&self, id: BindingId) -> bool {
        self.ids.iter().any(|slot| slot.load(Ordering::Acquire) == id.0)
    }

    /// The table of `generation`, if it is current or recent.
    pub(crate) fn at(&self, generation: u32) -> Option<Arc<Table>> {
        let current = self.current();
        if current.generation == generation {
            return Some(current);
        }
        lock(&self.history).iter().find(|t| t.generation == generation).cloned()
    }

    /// Swaps in `entries` as a new generation; the old table is kept for entries still queued against it.
    pub(crate) fn install(&self, entries: Vec<TableEntry>) {
        let mut current = lock(&self.current);
        let generation = current.generation.wrapping_add(1);
        for (i, slot) in self.ids.iter().enumerate() {
            slot.store(entries.get(i).map_or(0, |e| e.id.0), Ordering::Release);
        }
        let old = std::mem::replace(&mut *current, Arc::new(Table { generation, entries }));
        let mut history = lock(&self.history);
        history.push_back(old);
        while history.len() > HISTORY {
            history.pop_front();
        }
        self.generation.store(generation, Ordering::Release);
    }
}

/// A pending `ChordStore::learn`: the binding whose chord the next key press becomes, and the answer.
#[derive(Debug, Default)]
pub(crate) struct Learn {
    /// The binding id plus one; 0 when nobody is learning.
    pub(crate) wanted: AtomicU32,
    pub(crate) answer: Mutex<Option<(BindingId, Chord)>>,
    pub(crate) done: Condvar,
}

/// Everything the threads share.
pub(crate) struct Shared {
    pub(crate) clock: Arc<dyn Clock>,
    /// The global capture epoch.
    pub(crate) capture: Epoch,
    pub(crate) backends: Vec<BackendSlot>,
    /// The source tags in use.
    pub(crate) source_tags: AtomicU64,
    pub(crate) snapshots: [Snapshot; SourceTag::MAX],
    pub(crate) pads: [PadSlot; MAX_PADS],
    pub(crate) pad_ids: AtomicU8,
    /// [`Detail`] as 0 (zones) or 1 (hand only).
    pub(crate) detail: AtomicU8,
    /// Bumped on every detail change (design §4).
    pub(crate) detail_generation: AtomicU32,
    pub(crate) tables: Tables,
    pub(crate) learn: Learn,
    /// Entries dropped because their generation's table is gone and their binding with it (diagnostics).
    pub(crate) stale_entries: AtomicU64,
    /// Set when the hub is dropped: every backend thread ends.
    pub(crate) shutdown: AtomicBool,
    /// Where parked backend threads wait for an open epoch.
    pub(crate) wake: (Mutex<()>, Condvar),
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").field("backends", &self.backends.len()).finish_non_exhaustive()
    }
}

impl Shared {
    pub(crate) fn new(clock: Arc<dyn Clock>, backends: Vec<BackendSlot>) -> Self {
        Self {
            clock,
            capture: Epoch::default(),
            backends,
            source_tags: AtomicU64::new(0),
            snapshots: std::array::from_fn(|_| Snapshot::default()),
            pads: std::array::from_fn(|_| PadSlot::default()),
            pad_ids: AtomicU8::new(0),
            detail: AtomicU8::new(0),
            detail_generation: AtomicU32::new(0),
            tables: Tables::default(),
            learn: Learn::default(),
            stale_entries: AtomicU64::new(0),
            shutdown: AtomicBool::new(false),
            wake: (Mutex::new(()), Condvar::new()),
        }
    }

    pub(crate) fn now(&self) -> Seconds {
        self.clock.now()
    }

    pub(crate) fn detail(&self) -> Detail {
        if self.detail.load(Ordering::Acquire) == 1 { Detail::HandOnly } else { Detail::Zones }
    }

    /// Whether backend `b` may read now: the global epoch and its own are open.
    pub(crate) fn reading(&self, b: usize) -> bool {
        is_open(self.capture.get()) && self.backends.get(b).is_some_and(|slot| is_open(slot.epoch.get()))
    }

    /// Wakes every parked backend thread to look at the epochs again.
    pub(crate) fn wake_backends(&self) {
        let _guard = lock(&self.wake.0);
        self.wake.1.notify_all();
    }

    /// Takes a free source tag.
    pub(crate) fn take_source(&self) -> Option<SourceTag> {
        take_bit(&self.source_tags, 64).and_then(SourceTag::new)
    }

    /// Gives a source tag back, after its `SourceGone` was queued.
    pub(crate) fn free_source(&self, src: SourceTag) {
        self.source_tags.fetch_and(!(1u64 << src.index()), Ordering::AcqRel);
    }

    /// Takes a free pad id.
    pub(crate) fn take_pad(&self) -> Option<PadId> {
        let mut current = self.pad_ids.load(Ordering::Acquire);
        loop {
            let free = (0..MAX_PADS as u8).find(|i| current & (1 << i) == 0)?;
            match self.pad_ids.compare_exchange(current, current | 1 << free, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return PadId::new(free),
                Err(now) => current = now,
            }
        }
    }

    /// Gives a pad id back.
    pub(crate) fn free_pad(&self, pad: PadId) {
        if let Some(slot) = self.pads.get(usize::from(pad.index())) {
            slot.clear();
        }
        self.pad_ids.fetch_and(!(1u8 << pad.index()), Ordering::AcqRel);
    }
}

fn take_bit(bits: &AtomicU64, count: u8) -> Option<u8> {
    let mut current = bits.load(Ordering::Acquire);
    loop {
        let free = (0..count).find(|i| current & (1u64 << i) == 0)?;
        match bits.compare_exchange(current, current | 1u64 << free, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Some(free),
            Err(now) => current = now,
        }
    }
}
