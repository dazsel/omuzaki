//! The small fakes: [`FakePerception`] and [`FakeWallet`].

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use omuzaki_llm_api::usage::MicroUsd;
use omuzaki_ops_api::Spend;
use omuzaki_performer_api::perception::{Glance, Perception, PerceptionError};
use omuzaki_performer_api::spend::{SpendError, Wallet};

use crate::voice::lock;

/// [`Perception`] that hands out queued glances (or errors), then `None`.
#[derive(Default)]
pub struct FakePerception {
    queue: VecDeque<Result<Glance, PerceptionError>>,
}

impl FakePerception {
    /// A source that will return these, in order.
    pub fn new(queue: Vec<Result<Glance, PerceptionError>>) -> Self {
        Self { queue: queue.into() }
    }
}

impl Perception for FakePerception {
    fn glance(&mut self) -> Result<Option<Glance>, PerceptionError> {
        self.queue.pop_front().transpose()
    }
}

/// A [`Wallet`] with a fixed cap that records every charge. Clones share one purse.
#[derive(Clone)]
pub struct FakeWallet {
    cap: MicroUsd,
    charges: Arc<Mutex<Vec<Spend>>>,
}

impl FakeWallet {
    /// A wallet with `cap` to spend.
    pub fn new(cap: MicroUsd) -> Self {
        Self { cap, charges: Arc::default() }
    }

    /// Every charge, in order.
    pub fn charges(&self) -> Vec<Spend> {
        lock(&self.charges).clone()
    }

    /// The money charged so far.
    pub fn spent(&self) -> MicroUsd {
        MicroUsd(lock(&self.charges).iter().map(|s| s.usd_micros).sum())
    }
}

impl Wallet for FakeWallet {
    fn remaining(&self) -> Result<MicroUsd, SpendError> {
        Ok(MicroUsd(self.cap.0.saturating_sub(self.spent().0)))
    }

    fn charge(&self, spend: Spend) -> Result<(), SpendError> {
        lock(&self.charges).push(spend);
        Ok(())
    }
}
