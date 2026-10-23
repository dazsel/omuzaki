//! An in-memory [`Store`] for tests: ops, MCP and UI tests run against it without touching disk, and
//! [`conformance`] holds the checks every `Store` implementation (this one and `omuzaki-store`) must pass, so the mock
//! and the real store cannot drift apart.
//!
//! Correctness over speed: bytes live in a `HashMap` behind a `Mutex`, and `get` re-hashes like the disk store does.
#![deny(missing_docs)]

pub mod conformance;
pub mod explore;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::SystemTime;

use omuzaki_store_api::{ContentHash, Provenance, Removal, Store, StoreError, StoreGc};

/// One stored asset: its bytes, the provenance recorded by the first `put`, and when that put committed.
#[derive(Debug)]
struct Entry {
    bytes: Vec<u8>,
    provenance: Provenance,
    stored_at: SystemTime,
}

/// An asset store held in memory. Cheap to create, so every test gets its own.
pub struct MemStore {
    entries: Mutex<HashMap<ContentHash, Entry>>,
    /// Reads the time a `put` commits at. The wall clock unless a test supplied one (see [`MemStore::with_clock`]).
    clock: Clock,
}

/// A source of commit times.
type Clock = Arc<dyn Fn() -> SystemTime + Send + Sync>;

impl std::fmt::Debug for MemStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemStore").field("entries", &self.entries).finish_non_exhaustive()
    }
}

impl Default for MemStore {
    fn default() -> Self {
        Self::with_clock(SystemTime::now)
    }
}

impl MemStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty store that stamps each commit with `clock` instead of the wall clock. The clock is read once per
    /// `put`, under the lock, which is the commit point; a test makes a put slow by having the clock
    /// jump forward when it is read.
    pub fn with_clock(clock: impl Fn() -> SystemTime + Send + Sync + 'static) -> Self {
        Self { entries: Mutex::default(), clock: Arc::new(clock) }
    }

    /// How many distinct assets are stored (duplicate `put`s count once).
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    /// True if nothing is stored.
    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    /// Damages the stored bytes of `hash` by flipping one bit, so tests can check how callers handle
    /// [`StoreError::Corrupt`]. Returns false if the hash isn't stored or its bytes are empty.
    pub fn flip_byte(&self, hash: &ContentHash) -> bool {
        match self.lock().get_mut(hash).and_then(|e| e.bytes.first_mut()) {
            Some(byte) => {
                *byte ^= 1;
                true
            }
            None => false,
        }
    }

    /// Locks the map. A poisoned lock is still consistent here: every critical section is a single map operation.
    fn lock(&self) -> MutexGuard<'_, HashMap<ContentHash, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Store for MemStore {
    fn put(&self, bytes: &[u8], provenance: Provenance) -> Result<ContentHash, StoreError> {
        let hash = ContentHash::of(bytes);
        let copy = bytes.to_vec();
        let mut entries = self.lock();
        // Read under the lock, at the moment of publication: waiting for the lock cannot age the object, and a later
        // put never leaves an earlier time than an overlapping one.
        let stored_at = (self.clock)();
        match entries.get_mut(&hash) {
            // A put of bytes already held freshens them, as in the disk store, so a re-put gets a first put's grace.
            Some(entry) => entry.stored_at = entry.stored_at.max(stored_at),
            None => {
                entries.insert(hash, Entry { bytes: copy, provenance, stored_at });
            }
        }
        Ok(hash)
    }

    fn get(&self, hash: &ContentHash) -> Result<Vec<u8>, StoreError> {
        // Hash outside the lock: verifying large assets must not block other callers.
        let bytes = self.lock().get(hash).map(|e| e.bytes.clone()).ok_or(StoreError::Missing(*hash))?;
        if ContentHash::of(&bytes) == *hash { Ok(bytes) } else { Err(StoreError::Corrupt(*hash)) }
    }

    fn provenance(&self, hash: &ContentHash) -> Result<Provenance, StoreError> {
        self.lock().get(hash).map(|e| e.provenance.clone()).ok_or(StoreError::Missing(*hash))
    }

    fn contains(&self, hash: &ContentHash) -> bool {
        self.lock().contains_key(hash)
    }

    fn size(&self, hash: &ContentHash) -> Result<u64, StoreError> {
        self.lock().get(hash).map(|e| e.bytes.len() as u64).ok_or(StoreError::Missing(*hash))
    }
}

impl StoreGc for MemStore {
    fn hashes(&self) -> Result<Vec<ContentHash>, StoreError> {
        let mut hashes: Vec<ContentHash> = self.lock().keys().copied().collect();
        hashes.sort_unstable();
        Ok(hashes)
    }

    fn stored_at(&self, hash: &ContentHash) -> Result<SystemTime, StoreError> {
        self.lock().get(hash).map(|e| e.stored_at).ok_or(StoreError::Missing(*hash))
    }

    /// The check and the removal happen under the one lock, so they are atomic with a `put`.
    fn remove_if_older(&self, hash: &ContentHash, cutoff: SystemTime) -> Result<Removal, StoreError> {
        let mut entries = self.lock();
        match entries.get(hash) {
            None => Ok(Removal::Absent),
            Some(entry) if entry.stored_at > cutoff => Ok(Removal::Fresh),
            Some(_) => {
                entries.remove(hash);
                Ok(Removal::Removed)
            }
        }
    }

    /// Removal here is one map operation, so an interrupted removal leaves nothing behind.
    fn purge_retired(&self) -> Result<usize, StoreError> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conformance::{payload, sample_provenance};
    use omuzaki_store_api::Commercial;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::Duration;

    #[test]
    fn passes_the_conformance_suite() {
        conformance::run_all(&MemStore::new());
    }

    #[test]
    fn passes_the_gc_conformance_check() {
        conformance::remove_and_sweep(&MemStore::new());
    }

    #[test]
    fn a_slow_put_survives_a_sweep_straight_after_its_commit() {
        let grace = Duration::from_hours(1);
        let start = SystemTime::now();
        let now = Arc::new(Mutex::new(start));
        // The first read of the clock is the commit; it happens two graces after the put began.
        let stall = Arc::new(AtomicBool::new(true));
        let clock = {
            let (now, stall) = (Arc::clone(&now), Arc::clone(&stall));
            move || {
                let mut now = now.lock().unwrap();
                if stall.swap(false, Ordering::SeqCst) {
                    *now += grace * 2;
                }
                *now
            }
        };
        let store = MemStore::with_clock(clock);
        let slow_put = || store.put(&payload("slow", 64), sample_provenance(Commercial::Ok)).unwrap();
        let read_now = || *now.lock().unwrap();
        conformance::slow_commit_survives_sweep(&store, &slow_put, &read_now, grace);
        assert!(read_now() >= start + grace * 2, "the put really did take longer than grace");
    }

    #[test]
    fn a_re_put_freshens_the_object_and_a_removal_rechecks_the_commit_time() {
        let now = Arc::new(Mutex::new(SystemTime::now()));
        let clock = {
            let now = Arc::clone(&now);
            move || *now.lock().unwrap()
        };
        let store = MemStore::with_clock(clock);
        let bytes = payload("freshen", 64);
        let hash = store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        let first = store.stored_at(&hash).unwrap();
        let cutoff = first + Duration::from_secs(1);

        // The sweep's age check passed; then the object is re-put an hour later; then the removal runs.
        *now.lock().unwrap() += Duration::from_hours(1);
        store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        assert_eq!(store.remove_if_older(&hash, cutoff).unwrap(), Removal::Fresh, "the re-put freshened it");
        assert!(store.contains(&hash));
        assert_eq!(store.remove_if_older(&hash, first + Duration::from_hours(2)).unwrap(), Removal::Removed);
    }

    #[test]
    fn waiting_for_the_lock_cannot_age_a_put() {
        let grace = Duration::from_hours(1);
        let start = SystemTime::now();
        let now = Arc::new(Mutex::new(start));
        let slot: Arc<std::sync::OnceLock<std::sync::Weak<MemStore>>> = Arc::default();
        let held = Arc::new(AtomicBool::new(false));
        let clock = {
            let (now, slot, held) = (Arc::clone(&now), Arc::clone(&slot), Arc::clone(&held));
            move || {
                // Record whether the store's lock is held at the moment the commit time is read.
                if let Some(store) = slot.get().and_then(std::sync::Weak::upgrade) {
                    held.store(store.entries.try_lock().is_err(), Ordering::SeqCst);
                }
                *now.lock().unwrap()
            }
        };
        let store = Arc::new(MemStore::with_clock(clock));
        slot.set(Arc::downgrade(&store)).unwrap();

        // Another thread holds the store's lock for longer than the grace while a put waits for it.
        let guard = store.lock();
        std::thread::scope(|scope| {
            let put = scope.spawn(|| store.put(&payload("contended", 64), sample_provenance(Commercial::Ok)).unwrap());
            *now.lock().unwrap() += grace * 2;
            drop(guard);
            let hash = put.join().unwrap();
            assert!(held.load(Ordering::SeqCst), "the commit time is read while the lock is held");
            assert!(store.stored_at(&hash).unwrap() >= start + grace * 2, "the wait did not age the object");
        });
    }

    /// The grace every interleaving test sweeps with.
    const GRACE: Duration = Duration::from_hours(1);

    /// Two writers put the same bytes while a sweeper sweeps, in every possible order. Time moves either by a clock
    /// actor that jumps past the grace once, at any point, or (`timed`) by an eighth of the grace with each step, up to
    /// twelve times, with the sweeper first waiting another three quarters of a grace. Whatever the order: an object
    /// whose last `put` returned less than a grace before the sweep's time survives and reads back (every actor is in
    /// one process here), and old unreferenced garbage is always collected.
    fn interleaving_scenario(preexisting: bool, timed: bool, clock_now: &Arc<AtomicU64>) -> explore::Scenario {
        let nanos =
            |time: SystemTime| u64::try_from(time.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_nanos()).unwrap();
        let at = |nanos: u64| SystemTime::UNIX_EPOCH + Duration::from_nanos(nanos);
        clock_now.store(nanos(SystemTime::now()), Ordering::SeqCst);
        let start_nanos = clock_now.load(Ordering::SeqCst);
        let store = Arc::new(MemStore::with_clock({
            let clock_now = Arc::clone(clock_now);
            move || at(clock_now.load(Ordering::SeqCst))
        }));
        let bytes = payload("interleave", 64);
        let hash = ContentHash::of(&bytes);
        let garbage = store.put(&payload("garbage", 64), sample_provenance(Commercial::Ok)).unwrap();
        if preexisting {
            store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        }
        clock_now.fetch_add(nanos(SystemTime::UNIX_EPOCH + GRACE * 4), Ordering::SeqCst);

        let returned: Vec<Arc<AtomicU64>> = (0..2).map(|_| Arc::new(AtomicU64::new(u64::MAX))).collect();
        let sweep_at = Arc::new(AtomicU64::new(u64::MAX));
        let mut actors: Vec<explore::Actor> = Vec::new();
        for returned in &returned {
            let (store, clock_now, returned, bytes) =
                (Arc::clone(&store), Arc::clone(clock_now), Arc::clone(returned), bytes.clone());
            actors.push(Box::new(move |stepper| {
                let writer = explore::SteppedStore::new(store, stepper);
                writer.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
                returned.store(clock_now.load(Ordering::SeqCst), Ordering::SeqCst);
            }));
        }
        {
            let (store, clock_now, sweep_at) = (Arc::clone(&store), Arc::clone(clock_now), Arc::clone(&sweep_at));
            actors.push(Box::new(move |stepper| {
                let sweeper = explore::SteppedStore::new(store, stepper);
                if timed {
                    clock_now.fetch_add(u64::try_from(GRACE.as_nanos()).unwrap() * 6 / 8, Ordering::SeqCst);
                }
                let now = clock_now.load(Ordering::SeqCst);
                sweep_at.store(now, Ordering::SeqCst);
                sweeper.sweep(&|_| false, GRACE, at(now)).unwrap();
            }));
        }
        if !timed {
            let clock_now = Arc::clone(clock_now);
            actors.push(Box::new(move |_| {
                clock_now.fetch_add(u64::try_from(GRACE.as_nanos()).unwrap() * 2, Ordering::SeqCst);
            }));
        }
        let clock_now = Arc::clone(clock_now);
        let check = Box::new(move |schedule: &str| {
            if timed {
                // Each schedule gets its own clock steps: the setup, the sweeper's wait, and at least one step.
                let elapsed = clock_now.load(Ordering::SeqCst) - start_nanos;
                let least = u64::try_from((GRACE * 4 + GRACE * 6 / 8 + GRACE / 8).as_nanos()).unwrap();
                assert!(elapsed >= least, "schedule {schedule}: the clock did not step ({elapsed} ns)");
            }
            assert!(!store.contains(&garbage), "schedule {schedule}: old garbage must be collected");
            let sweep_at = sweep_at.load(Ordering::SeqCst);
            for (writer, returned) in returned.iter().enumerate() {
                let returned = returned.load(Ordering::SeqCst);
                assert_ne!(returned, u64::MAX, "schedule {schedule}: writer {writer} did not finish");
                let age = u128::from(sweep_at.saturating_sub(returned));
                if age < GRACE.as_nanos() {
                    assert_eq!(
                        store.get(&hash).as_deref(),
                        Ok(bytes.as_slice()),
                        "schedule {schedule}: writer {writer}'s put returned within the grace of the sweep"
                    );
                }
            }

            // Eventually collectable: long after every put, a sweep takes the now unreferenced object.
            clock_now.fetch_add(u64::try_from(GRACE.as_nanos()).unwrap() * 2, Ordering::SeqCst);
            let later = at(clock_now.load(Ordering::SeqCst));
            store.sweep(&|_| false, GRACE, later).unwrap();
            assert!(!store.contains(&hash), "schedule {schedule}: unreferenced and old, so collected");
        });
        explore::Scenario { actors, check }
    }

    #[test]
    fn every_interleaving_of_two_puts_and_a_sweep_keeps_a_fresh_object() {
        let clock_now = Arc::new(AtomicU64::new(0));
        let ticks = AtomicU64::new(0);
        for timed in [false, true] {
            for preexisting in [false, true] {
                let explored = explore::explore_timed(
                    20_000,
                    &|| {
                        // Every schedule gets its own twelve clock steps.
                        ticks.store(0, Ordering::SeqCst);
                        interleaving_scenario(preexisting, timed, &clock_now)
                    },
                    &|| {
                        if timed && ticks.fetch_add(1, Ordering::SeqCst) < 12 {
                            clock_now.fetch_add(u64::try_from(GRACE.as_nanos()).unwrap() / 8, Ordering::SeqCst);
                        }
                    },
                );
                assert!(explored > 100, "timed={timed} preexisting={preexisting}: only {explored} schedules");
                println!("MemStore timed={timed} preexisting={preexisting}: {explored} schedules explored");
            }
        }
    }

    #[test]
    fn duplicate_puts_store_one_asset() {
        let store = MemStore::new();
        assert!(store.is_empty());
        let bytes = payload("dup", 1024);
        let a = store.put(&bytes, sample_provenance(Commercial::Ok)).unwrap();
        let b = store.put(&bytes, sample_provenance(Commercial::No)).unwrap();
        assert_eq!(a, b);
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn corruption_is_detected_like_on_disk() {
        let store = MemStore::new();
        let hash = store.put(&payload("rot", 64), sample_provenance(Commercial::Ok)).unwrap();
        assert!(store.flip_byte(&hash));
        assert_eq!(store.get(&hash), Err(StoreError::Corrupt(hash)));
        assert!(store.contains(&hash), "a corrupt asset is still present");
        assert!(!store.flip_byte(&ContentHash([0; 32])), "nothing to damage for an unknown hash");
    }
}
