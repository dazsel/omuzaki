//! The money ledger: what jobs have reserved and what they were charged (ADR 0036 decision 3).

/// Reserved and charged money, in micro-dollars, against one budget.
///
/// A reservation is held from a job's start until its `work` has returned, not until its state ends: a detached
/// thread may still spend, so cancelling a paid call never frees the money it could still use.
pub(crate) struct Ledger {
    budget: u64,
    held: u64,
    charged: u64,
}

impl Ledger {
    pub(crate) fn new(budget: u64) -> Self {
        Self { budget, held: 0, charged: 0 }
    }

    /// Holds `need` for a job about to start; `false` (nothing held) when charged plus held plus `need` would pass
    /// the budget.
    pub(crate) fn reserve(&mut self, need: u64) -> bool {
        // A sum past `u64::MAX` is past any budget, the unlimited one included.
        let total = self.charged.checked_add(self.held).and_then(|sum| sum.checked_add(need));
        if total.is_none_or(|total| total > self.budget) {
            return false;
        }
        self.held += need;
        true
    }

    /// Ends a reservation of `reserved`: `spent` is charged and the rest is released.
    pub(crate) fn settle(&mut self, reserved: u64, spent: u64) {
        self.held = self.held.saturating_sub(reserved);
        self.charged = self.charged.saturating_add(spent);
    }

    /// Everything charged so far.
    pub(crate) fn charged(&self) -> u64 {
        self.charged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reservation_counts_until_it_is_settled() {
        let mut ledger = Ledger::new(1_000);
        assert!(ledger.reserve(600));
        assert!(!ledger.reserve(600), "the held money is not available twice");
        ledger.settle(600, 100);
        assert_eq!(ledger.charged(), 100);
        assert!(ledger.reserve(900), "the rest is released once the work has returned");
    }

    #[test]
    fn an_overflow_is_refused_even_against_the_unlimited_budget() {
        let mut ledger = Ledger::new(u64::MAX);
        assert!(ledger.reserve(u64::MAX));
        assert!(!ledger.reserve(1), "held plus need would overflow");
        ledger.settle(u64::MAX, 1);
        assert!(!ledger.reserve(u64::MAX), "charged plus need would overflow");
        assert!(ledger.reserve(u64::MAX - 1));
    }

    #[test]
    fn charged_money_stays_counted() {
        let mut ledger = Ledger::new(1_000);
        assert!(ledger.reserve(600));
        ledger.settle(600, 500);
        assert!(!ledger.reserve(600));
        assert!(ledger.reserve(500));
    }
}
