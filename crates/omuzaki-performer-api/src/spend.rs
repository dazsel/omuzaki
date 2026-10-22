//! Money: the one [`SpendGate`] every paid call of a performer goes through (`ai-performer-voice-face.md` §3.6).
//!
//! The LLM turn and hosted syntheses overlap, so two bounds checked separately against what is left could both pass
//! and overspend together (100 left: 80 and 30). The gate counts what is reserved but not yet billed, and refuses a
//! bound that, with every outstanding reservation, would pass the money left.
//!
//! **Only a completed request settles**, with its final bill: an LLM turn from its last `LlmEvent::Usage` (just before
//! `Stop`, ADR 0032), a hosted synthesis from its returned chunk. A reservation that is cancelled, fails or is dropped
//! is charged its whole bound, whatever partial usage it saw, because a provider may bill a cut request for more than
//! it reported so far.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_llm_api::usage::MicroUsd;
use omuzaki_ops_api::{SessionId, Sessions, Spend};

/// Where the money is: the session's remaining budget and its charges. [`SessionWallet`] is the real one.
pub trait Wallet: Send + Sync {
    /// Money left before the cap.
    ///
    /// # Errors
    /// [`SpendError`] when the session is gone.
    fn remaining(&self) -> Result<MicroUsd, SpendError>;

    /// Charges `spend`. A charge that crosses the cap pauses the session (`PauseReason::Budget`).
    ///
    /// # Errors
    /// [`SpendError`] when the session is gone or has ended.
    fn charge(&self, spend: Spend) -> Result<(), SpendError>;
}

/// A [`Wallet`] over one session of the executor's [`Sessions`]: the performer's own.
pub struct SessionWallet {
    sessions: Arc<dyn Sessions>,
    id: SessionId,
}

impl SessionWallet {
    /// The wallet of session `id`.
    pub fn new(sessions: Arc<dyn Sessions>, id: SessionId) -> Self {
        Self { sessions, id }
    }
}

impl Wallet for SessionWallet {
    fn remaining(&self) -> Result<MicroUsd, SpendError> {
        let info = self.sessions.session(self.id).ok_or(SpendError::NoSession)?;
        Ok(MicroUsd(info.budget.usd_micros.saturating_sub(info.spent.usd_micros)))
    }

    fn charge(&self, spend: Spend) -> Result<(), SpendError> {
        self.sessions.charge(self.id, spend).map(drop).map_err(|e| SpendError::Wallet(e.to_string()))
    }
}

/// Why the gate refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SpendError {
    /// The bound and the outstanding reservations together pass the money left: the cap is crossed
    /// (`PauseReason::Budget`; the sentence ends and the performer goes idle).
    #[error("{} µ$ more would pass the cap: {} µ$ left, {} µ$ already reserved", .bound.0, .remaining.0, .outstanding.0)]
    #[non_exhaustive]
    Exhausted {
        /// The bound asked for.
        bound: MicroUsd,
        /// Reserved and not yet billed.
        outstanding: MicroUsd,
        /// Money left before the cap.
        remaining: MicroUsd,
    },
    /// The session is gone.
    #[error("the performer's session is gone")]
    NoSession,
    /// The wallet failed to charge.
    #[error("charging the session failed: {0}")]
    Wallet(String),
}

impl SpendError {
    /// [`SpendError::Exhausted`]: `bound` refused with `outstanding` reserved and `remaining` left.
    pub fn exhausted(bound: MicroUsd, outstanding: MicroUsd, remaining: MicroUsd) -> Self {
        Self::Exhausted { bound, outstanding, remaining }
    }
}

#[derive(Default)]
struct State {
    outstanding: u64,
    /// A charge that failed where it could not be returned (a dropped reservation); the next `reserve` reports it.
    failed: Option<SpendError>,
}

struct Inner {
    wallet: Arc<dyn Wallet>,
    state: Mutex<State>,
}

/// One per performer, shared by its LLM brain and its TTS host: bounds reserved together never pass the cap. Clones
/// share one gate.
#[derive(Clone)]
pub struct SpendGate(Arc<Inner>);

impl SpendGate {
    /// A gate over `wallet`.
    pub fn new(wallet: Arc<dyn Wallet>) -> Self {
        Self(Arc::new(Inner { wallet, state: Mutex::new(State::default()) }))
    }

    /// Reserves `bound` for one request.
    ///
    /// # Errors
    /// [`SpendError::Exhausted`] when `bound` plus every outstanding reservation passes the money left; a wallet error;
    /// or the error of an earlier dropped reservation's charge.
    pub fn reserve(&self, bound: MicroUsd) -> Result<Reservation, SpendError> {
        let mut state = self.lock();
        if let Some(failed) = state.failed.take() {
            return Err(failed);
        }
        let remaining = self.0.wallet.remaining()?;
        let wanted = state.outstanding.saturating_add(bound.0);
        if wanted > remaining.0 {
            return Err(SpendError::exhausted(bound, MicroUsd(state.outstanding), remaining));
        }
        state.outstanding = wanted;
        Ok(Reservation { gate: self.clone(), bound, open: true })
    }

    /// Reserved and not yet billed.
    pub fn outstanding(&self) -> MicroUsd {
        MicroUsd(self.lock().outstanding)
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A poisoned lock means a thread panicked mid-update of two integers; the counts are still the best record.
        self.0.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Charges `spend` and releases `bound`, under the gate's lock, so a `reserve` never sees the money released
    /// before it is charged.
    fn close(&self, bound: MicroUsd, spend: Spend) -> Result<(), SpendError> {
        let mut state = self.lock();
        let charged = self.0.wallet.charge(spend);
        state.outstanding = state.outstanding.saturating_sub(bound.0);
        charged
    }
}

impl fmt::Debug for SpendGate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SpendGate").field("outstanding", &self.outstanding()).finish_non_exhaustive()
    }
}

/// Money held for one request until its final bill. Settle it when the request completes; anything else (forfeit,
/// drop) charges the whole bound.
#[derive(Debug)]
#[must_use = "a dropped reservation is charged its whole bound"]
pub struct Reservation {
    gate: SpendGate,
    bound: MicroUsd,
    open: bool,
}

impl Reservation {
    /// The bound held.
    pub fn bound(&self) -> MicroUsd {
        self.bound
    }

    /// The request completed: charges its final bill (`spend.usd_micros` from the final usage, its tokens) and
    /// releases the rest of the bound. A bill over the bound is charged in full.
    ///
    /// # Errors
    /// The wallet's error; the reservation is released either way.
    pub fn settle(mut self, spend: Spend) -> Result<(), SpendError> {
        self.open = false;
        self.gate.close(self.bound, spend)
    }

    /// The request was cancelled or failed: charges the whole bound.
    ///
    /// # Errors
    /// The wallet's error; the reservation is released either way.
    pub fn forfeit(mut self) -> Result<(), SpendError> {
        self.open = false;
        self.gate.close(self.bound, Spend { usd_micros: self.bound.0, ..Spend::default() })
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.open {
            let charged = self.gate.close(self.bound, Spend { usd_micros: self.bound.0, ..Spend::default() });
            if let Err(e) = charged {
                self.gate.lock().failed = Some(e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wallet with a fixed cap that records its charges.
    struct Purse {
        cap: u64,
        charged: Mutex<Vec<Spend>>,
        broken: bool,
    }

    impl Purse {
        fn new(cap: u64) -> Arc<Self> {
            Arc::new(Self { cap, charged: Mutex::new(Vec::new()), broken: false })
        }

        fn spent(&self) -> u64 {
            self.charged.lock().unwrap().iter().map(|s| s.usd_micros).sum()
        }
    }

    impl Wallet for Purse {
        fn remaining(&self) -> Result<MicroUsd, SpendError> {
            Ok(MicroUsd(self.cap.saturating_sub(self.spent())))
        }

        fn charge(&self, spend: Spend) -> Result<(), SpendError> {
            if self.broken {
                return Err(SpendError::NoSession);
            }
            self.charged.lock().unwrap().push(spend);
            Ok(())
        }
    }

    fn bill(usd: u64) -> Spend {
        Spend { usd_micros: usd, tokens: 0, seconds: 0 }
    }

    #[test]
    fn thirty_is_refused_after_eighty_with_a_hundred_left() {
        let purse = Purse::new(100);
        let gate = SpendGate::new(purse.clone());
        let llm = gate.reserve(MicroUsd(80)).unwrap();
        let err = gate.reserve(MicroUsd(30)).unwrap_err();
        assert_eq!(
            err,
            SpendError::Exhausted { bound: MicroUsd(30), outstanding: MicroUsd(80), remaining: MicroUsd(100) }
        );
        assert_eq!(err.to_string(), "30 µ$ more would pass the cap: 100 µ$ left, 80 µ$ already reserved");
        // It completed for 50: the rest is released and 30 now fits.
        llm.settle(bill(50)).unwrap();
        assert_eq!((purse.spent(), gate.outstanding()), (50, MicroUsd(0)));
        let tts = gate.reserve(MicroUsd(30)).unwrap();
        assert_eq!(tts.bound(), MicroUsd(30));
        tts.settle(bill(30)).unwrap();
        assert_eq!(purse.spent(), 80);
    }

    #[test]
    fn a_cut_or_failed_request_is_charged_its_whole_bound() {
        let purse = Purse::new(100);
        let gate = SpendGate::new(purse.clone());
        // Cancelled after partial usage worth 20: still 80 charged, and a new 80 is refused.
        gate.reserve(MicroUsd(80)).unwrap().forfeit().unwrap();
        assert_eq!(purse.spent(), 80);
        assert!(matches!(gate.reserve(MicroUsd(80)), Err(SpendError::Exhausted { .. })));
        // Dropped without settling: the same.
        let purse = Purse::new(100);
        let gate = SpendGate::new(purse.clone());
        drop(gate.reserve(MicroUsd(80)).unwrap());
        assert_eq!((purse.spent(), gate.outstanding()), (80, MicroUsd(0)));
    }

    #[test]
    fn an_llm_turn_and_two_syntheses_never_pass_the_cap() {
        let purse = Purse::new(1_000);
        let gate = SpendGate::new(purse.clone());
        let mut held = Vec::new();
        for bound in [600, 300, 200, 100] {
            if let Ok(r) = gate.reserve(MicroUsd(bound)) {
                held.push(r);
            }
        }
        let total: u64 = held.iter().map(|r| r.bound().0).sum();
        assert_eq!(total, 1_000);
        for r in held {
            r.forfeit().unwrap();
        }
        assert_eq!(purse.spent(), 1_000);
        assert!(gate.reserve(MicroUsd(1)).is_err());
    }

    #[test]
    fn a_failed_charge_on_drop_is_reported_by_the_next_reserve() {
        let purse = Arc::new(Purse { cap: 100, charged: Mutex::new(Vec::new()), broken: true });
        let gate = SpendGate::new(purse);
        drop(gate.reserve(MicroUsd(10)).unwrap());
        assert_eq!(gate.reserve(MicroUsd(10)).unwrap_err(), SpendError::NoSession);
        assert_eq!(gate.outstanding(), MicroUsd(0));
    }
}
