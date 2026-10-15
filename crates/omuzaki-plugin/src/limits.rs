//! What one instance may use (design §4.2, §5.2): its memory, tables and instances, its share of every plugin's
//! memory, its host-call rate and its CPU share.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use omuzaki_core::time::Seconds;
use omuzaki_plugin_api::limits::{
    CPU_SHARE_FUEL_PER_SECOND, HOST_CALL_BURST, HOST_CALLS_PER_SECOND, MAX_INSTANCES, MAX_TABLE_ELEMENTS, MAX_TABLES,
};

/// The memory every running plugin shares: each one's host side plus its linear memory in use, against one cap (the
/// editor's 256 MiB, or performance mode's 128 MiB). Cheap to clone; clones share the count.
#[derive(Clone, Debug)]
pub struct MemoryBudget {
    cap: u64,
    used: Arc<AtomicU64>,
}

impl MemoryBudget {
    /// A budget of `cap` bytes, none used.
    pub fn new(cap: u64) -> Self {
        Self { cap, used: Arc::new(AtomicU64::new(0)) }
    }

    /// The cap, in bytes.
    pub fn cap(&self) -> u64 {
        self.cap
    }

    /// Bytes reserved now.
    pub fn used(&self) -> u64 {
        self.used.load(Ordering::Acquire)
    }

    /// Reserves `bytes` if they fit under the cap; false (and nothing reserved) when they don't.
    pub fn try_reserve(&self, bytes: u64) -> bool {
        self.used
            .try_update(Ordering::AcqRel, Ordering::Acquire, |used| {
                used.checked_add(bytes).filter(|total| *total <= self.cap)
            })
            .is_ok()
    }

    /// Gives back `bytes` reserved earlier.
    pub fn release(&self, bytes: u64) {
        // Saturating, so a double release can never wrap the count; the debug build says it happened.
        let before = self.used.try_update(Ordering::AcqRel, Ordering::Acquire, |used| Some(used.saturating_sub(bytes)));
        debug_assert!(before.is_ok_and(|used| used >= bytes), "released more plugin memory than was reserved");
    }
}

/// A reservation in a [`MemoryBudget`] that gives itself back when dropped.
#[derive(Debug)]
pub struct Reservation {
    budget: MemoryBudget,
    bytes: u64,
}

impl Reservation {
    /// Reserves `bytes`, or `None` when they don't fit.
    pub fn take(budget: &MemoryBudget, bytes: u64) -> Option<Self> {
        budget.try_reserve(bytes).then(|| Self { budget: budget.clone(), bytes })
    }

    /// Grows the reservation by `bytes`; false (unchanged) when they don't fit.
    pub fn grow(&mut self, bytes: u64) -> bool {
        let fits = self.budget.try_reserve(bytes);
        if fits {
            self.bytes += bytes;
        }
        fits
    }

    /// Bytes held.
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}

/// The store's `ResourceLimiter`: one memory of at most `memory_bytes`, [`MAX_TABLES`] table of
/// [`MAX_TABLE_ELEMENTS`], [`MAX_INSTANCES`] core instances, and every byte of linear memory also reserved in the
/// all-plugins budget. A refused grow makes `memory.grow` return -1 and is remembered, so the trap the guest's abort
/// then raises is reported as out of memory.
#[derive(Debug)]
pub struct Limiter {
    memory_bytes: u64,
    reservation: Reservation,
    /// Set when a grow was refused, until the next call starts.
    pub refused_grow: bool,
}

impl Limiter {
    /// A limiter capping memory at `memory_bytes`, charging linear memory to `reservation` (which already holds the
    /// plugin's host side).
    pub fn new(memory_bytes: u64, reservation: Reservation) -> Self {
        Self { memory_bytes, reservation, refused_grow: false }
    }

    /// The bytes this plugin holds in the all-plugins budget.
    pub fn reserved(&self) -> u64 {
        self.reservation.bytes()
    }
}

impl wasmtime::ResourceLimiter for Limiter {
    fn memory_growing(&mut self, current: usize, desired: usize, _maximum: Option<usize>) -> wasmtime::Result<bool> {
        let grow = (desired as u64).saturating_sub(current as u64);
        let fits = desired as u64 <= self.memory_bytes && self.reservation.grow(grow);
        self.refused_grow |= !fits;
        Ok(fits)
    }

    fn table_growing(&mut self, _current: usize, desired: usize, _maximum: Option<usize>) -> wasmtime::Result<bool> {
        Ok(desired <= MAX_TABLE_ELEMENTS)
    }

    fn instances(&self) -> usize {
        MAX_INSTANCES
    }

    fn tables(&self) -> usize {
        MAX_TABLES as usize
    }

    fn memories(&self) -> usize {
        1
    }
}

/// A token bucket on the runner's clock: [`HOST_CALLS_PER_SECOND`] sustained, [`HOST_CALL_BURST`] at once.
#[derive(Debug, Clone)]
pub struct HostCallBucket {
    tokens: f64,
    at: Seconds,
}

impl HostCallBucket {
    /// A full bucket at `now`.
    pub fn new(now: Seconds) -> Self {
        Self { tokens: f64::from(HOST_CALL_BURST), at: now }
    }

    /// Takes one token at `now`; false when the bucket is empty.
    pub fn take(&mut self, now: Seconds) -> bool {
        let elapsed = (now.0 - self.at.0).max(0.0);
        self.tokens = (self.tokens + elapsed * f64::from(HOST_CALLS_PER_SECOND)).min(f64::from(HOST_CALL_BURST));
        self.at = Seconds(self.at.0.max(now.0));
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// A plugin's CPU share (§5.2): [`CPU_SHARE_FUEL_PER_SECOND`] of fuel per second of the runner's clock, host-call
/// charges included. Every call's fuel is the smaller of its cap and what is left this second.
#[derive(Debug, Clone, Default)]
pub struct CpuShare {
    second: i64,
    used: u64,
}

impl CpuShare {
    /// Fuel left in the second `now` falls in.
    pub fn left(&mut self, now: Seconds) -> u64 {
        self.roll(now);
        CPU_SHARE_FUEL_PER_SECOND.saturating_sub(self.used)
    }

    /// Records `fuel` used at `now`.
    pub fn spend(&mut self, now: Seconds, fuel: u64) {
        self.roll(now);
        self.used = self.used.saturating_add(fuel);
    }

    fn roll(&mut self, now: Seconds) {
        // The runner's clock is seconds since its epoch, far inside i64 for any session.
        #[allow(clippy::cast_possible_truncation)]
        let second = now.0.floor() as i64;
        if second != self.second {
            self.second = second;
            self.used = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use wasmtime::ResourceLimiter as _;

    use super::*;

    #[test]
    fn the_budget_refuses_past_its_cap_and_reservations_give_back() {
        let budget = MemoryBudget::new(100);
        let mut a = Reservation::take(&budget, 60).unwrap();
        assert!(Reservation::take(&budget, 50).is_none());
        assert!(a.grow(40) && !a.grow(1));
        assert_eq!(budget.used(), 100);
        drop(a);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn the_limiter_caps_memory_and_remembers_a_refusal() {
        let budget = MemoryBudget::new(1 << 30);
        let mut limiter = Limiter::new(1 << 20, Reservation::take(&budget, 0).unwrap());
        assert!(limiter.memory_growing(0, 1 << 20, None).unwrap());
        assert!(!limiter.refused_grow);
        assert!(!limiter.memory_growing(1 << 20, (1 << 20) + 65536, None).unwrap());
        assert!(limiter.refused_grow);
        assert_eq!(budget.used(), 1 << 20);
        assert!(limiter.table_growing(0, MAX_TABLE_ELEMENTS, None).unwrap());
        assert!(!limiter.table_growing(0, MAX_TABLE_ELEMENTS + 1, None).unwrap());
    }

    #[test]
    fn the_bucket_allows_its_burst_then_its_rate() {
        let mut bucket = HostCallBucket::new(Seconds(0.0));
        let burst = (0..2 * HOST_CALL_BURST).filter(|_| bucket.take(Seconds(0.0))).count();
        assert_eq!(burst, HOST_CALL_BURST as usize);
        let later = (0..HOST_CALLS_PER_SECOND).filter(|_| bucket.take(Seconds(0.05))).count();
        assert_eq!(later, (HOST_CALLS_PER_SECOND / 20) as usize);
    }

    #[test]
    fn the_share_refills_each_second() {
        let mut share = CpuShare::default();
        assert_eq!(share.left(Seconds(3.2)), CPU_SHARE_FUEL_PER_SECOND);
        share.spend(Seconds(3.5), CPU_SHARE_FUEL_PER_SECOND - 10);
        assert_eq!(share.left(Seconds(3.9)), 10);
        assert_eq!(share.left(Seconds(4.0)), CPU_SHARE_FUEL_PER_SECOND);
    }
}
