//! Epochs (design §4): how a pause, a stop or a backend leaving ends what is held without anyone writing into another
//! thread's ring. An epoch is a counter whose low bit says whether it is open (capture running). Every transition out
//! of running bumps it to a closed value, every start or resume to a new open one; every ring entry carries the global
//! epoch and its backend's epoch it was read in, and a feed accepts an entry only when both are current and open.

use std::sync::atomic::{AtomicU32, Ordering};

/// One epoch counter. Written only by the control thread (the global one, and a backend's on a stop or start) or by a
/// failing backend's own thread (its own, once, on the way out).
#[derive(Debug, Default)]
pub(crate) struct Epoch(AtomicU32);

impl Epoch {
    /// The current value.
    pub(crate) fn get(&self) -> u32 {
        self.0.load(Ordering::Acquire)
    }

    /// Opens a new epoch, unless one is open. Returns the value now current.
    pub(crate) fn open(&self) -> u32 {
        self.bump(true)
    }

    /// Closes the current epoch, unless it is closed. Returns the value now current.
    pub(crate) fn close(&self) -> u32 {
        self.bump(false)
    }

    fn bump(&self, open: bool) -> u32 {
        let mut current = self.get();
        loop {
            if is_open(current) == open {
                return current;
            }
            match self.0.compare_exchange(current, current.wrapping_add(1), Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return current.wrapping_add(1),
                Err(now) => current = now,
            }
        }
    }
}

/// Whether an epoch value is open.
pub(crate) const fn is_open(epoch: u32) -> bool {
    epoch & 1 == 1
}

/// The two stamps an entry carries, packed into one word: the global epoch low, the backend's high.
pub(crate) const fn stamp(capture: u32, backend: u32) -> u64 {
    capture as u64 | (backend as u64) << 32
}

/// Whether an entry stamped `word` may reach a consumer while the epochs are `capture` and `backend`.
pub(crate) const fn accepts(word: u64, capture: u32, backend: u32) -> bool {
    word == stamp(capture, backend) && is_open(capture) && is_open(backend)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_alternate_and_only_current_open_stamps_pass() {
        let epoch = Epoch::default();
        assert!(!is_open(epoch.get()));
        let first = epoch.open();
        assert_eq!(epoch.open(), first, "opening an open epoch changes nothing");
        let closed = epoch.close();
        let second = epoch.open();
        assert!(is_open(first) && !is_open(closed) && is_open(second) && first != second);
        assert!(accepts(stamp(second, 1), second, 1));
        assert!(!accepts(stamp(first, 1), second, 1), "an earlier run's entry");
        assert!(!accepts(stamp(closed, 1), closed, 1), "a closed epoch");
        assert!(!accepts(stamp(second, 3), second, 1), "an earlier run of the backend");
    }
}
