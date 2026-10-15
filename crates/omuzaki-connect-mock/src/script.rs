//! What a fake connector does, scripted by the test.

use omuzaki_connect_api::ConnectError;

/// What a fake connector does: how long it waits, what it bills and where it fails.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Script {
    /// Seconds waited through `cx.pace` before each output (0 means none).
    pub latency_s: f64,
    /// Billed per output, in micro-dollars.
    pub usd_micros_per_output: u64,
    /// Fail at output index `.0` (0-based) with this error instead of producing it.
    pub fail: Option<(u32, ConnectError)>,
}

impl Script {
    /// Free, instant and never failing, billing `usd_micros` per output.
    pub fn priced(usd_micros: u64) -> Self {
        Self { usd_micros_per_output: usd_micros, ..Self::default() }
    }
}
