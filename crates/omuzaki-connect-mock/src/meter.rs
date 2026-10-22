//! [`RecordingMeter`]: a [`Meter`] that records every charge and can refuse past a cap.

use std::sync::{Mutex, PoisonError};

use omuzaki_connect_api::{ConnectError, Meter};

/// Records every charge, in order. With a cap it refuses, with `CapReached`, a charge that would pass it (the refused
/// charge is recorded too: the request it reports was already billed).
#[derive(Debug, Default)]
pub struct RecordingMeter {
    cap_usd_micros: Option<u64>,
    charges: Mutex<Vec<u64>>,
}

impl RecordingMeter {
    /// A meter with no cap.
    pub fn new() -> Self {
        Self::default()
    }

    /// A meter that refuses once the total would pass `cap_usd_micros`.
    pub fn capped(cap_usd_micros: u64) -> Self {
        Self { cap_usd_micros: Some(cap_usd_micros), charges: Mutex::default() }
    }

    /// Every charge, in order.
    pub fn charges(&self) -> Vec<u64> {
        self.charges.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The sum of every charge.
    pub fn total(&self) -> u64 {
        self.charges().iter().fold(0, |sum, c| sum.saturating_add(*c))
    }
}

impl Meter for RecordingMeter {
    fn charge(&self, usd_micros: u64) -> Result<(), ConnectError> {
        let mut charges = self.charges.lock().unwrap_or_else(PoisonError::into_inner);
        let before = charges.iter().fold(0u64, |sum, c| sum.saturating_add(*c));
        charges.push(usd_micros);
        let after = before.saturating_add(usd_micros);
        match self.cap_usd_micros {
            Some(cap) if after > cap => Err(ConnectError::cap_reached(cap, after)),
            _ => Ok(()),
        }
    }
}
