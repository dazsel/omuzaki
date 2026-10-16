//! The provider conformance suite (llm-api D10): the cases every [`LlmClient`] must pass, written as [`Script`]s.
//! `FakeLlm` passes it directly; a real provider passes it by building its client against a loopback server that
//! plays the same script on the wire, and handing [`run`] a [`Harness`] that builds those clients.
//!
//! ## The D10 rows and the cases that cover them
//!
//! | D10 row | Case |
//! |---|---|
//! | text only | `text only` |
//! | one tool call | `one tool call` |
//! | two parallel calls | `two parallel calls` |
//! | opaque round trip | `opaque round trip` (providers with opaque blocks) |
//! | cumulative usage | `cumulative usage` |
//! | refusal; `MaxOutput` | `refusal and max output` |
//! | 429 with `retry-after`, then success (1 retry) | `429 with retry-after retries once` |
//! | 429 without it (no retry) | `429 without retry-after is not retried` |
//! | 529 five times (`Overloaded` after 4) | `retries stop after four` |
//! | 401 (`Auth`, no retry) | `401 is Auth and not retried` |
//! | context error | `context error names the window` |
//! | a drop after `Start` (`Network`, no retry) | `a drop after Start is never retried` |
//! | `Start → Usage → Network` keeps that usage | `usage survives a drop` |
//! | malformed SSE | `malformed transport data` |
//! | cancel mid-stream | `cancel mid-stream` (the 50 ms close is measured at the server, in `omuzaki-llm`'s tests) |
//! | cancel during backoff | `cancel during backoff` |
//! | any split gives the same events | `any split gives the same turn` (byte splits of the wire: `omuzaki-llm`'s tests) |
//! | a key never appears | `the key never appears` |
//! | §4 priced to the micro-dollar (633, 101 and 734 µ$) | `the worked example costs 633, 101 and 734` (providers that report cache writes) |
//! | the shared request checks (#1697) | `requests the model cannot take are refused first` |
//!
//! ## What a server harness plays for a script
//!
//! A real provider's harness turns a [`Script`] into replies on the wire. The mapping is fixed so a case means the
//! same thing for every provider:
//!
//! - **events**: the provider's own encoding of the events (`Start` becomes its start event, `TextDelta` a text delta,
//!   and so on); `chunk` splits deltas.
//! - **`retries: n`** with no error: `n` answers of `429` with `retry-after` of `retry_after_ms` (whole seconds, rounded
//!   up), then the turn.
//! - **an error before event 0** (the request fails): `RateLimited { retry_after }` is a `429` (with that
//!   `retry-after` when `Some`), `Overloaded` a `529`, `Auth` a `401`, `ContextTooLong` the provider's context error.
//!   With `retries: n` the failing answer is given `n + 1` times.
//! - **an error before a later event** (the stream fails): `Network` breaks the connection after that many events,
//!   `Malformed` sends bytes that are not an event stream after them.

mod broken;
mod cases;

pub use broken::RetriesAfterStart;

use omuzaki_llm_api::LlmClient;

use crate::Script;

/// Builds a client that plays `script`: `FakeLlm::new` for the fake, a loopback server for a real provider.
pub type Make<'a> = &'a dyn Fn(Script) -> Box<dyn LlmClient>;

/// What the suite needs to know about the provider under test.
#[derive(Clone, Copy)]
#[non_exhaustive]
pub struct Harness<'a> {
    /// Builds the client.
    pub make: Make<'a>,
    /// The `provider` that [`Opaque`](omuzaki_llm_api::stream::LlmEvent::Opaque) blocks carry, for a provider that has
    /// them; `None` skips the opaque case.
    pub opaque_provider: Option<&'a str>,
    /// The provider reports cache writes, so the §4 worked example can run.
    pub cache_writes: bool,
    /// The key the clients were built with; it must appear in no output. Empty for none.
    pub secret: &'a str,
}

impl<'a> Harness<'a> {
    /// A harness for clients from `make`, with no opaque blocks, no cache writes and no key.
    pub fn new(make: Make<'a>) -> Self {
        Self { make, opaque_provider: None, cache_writes: false, secret: "" }
    }

    /// The provider whose opaque blocks the clients return.
    #[must_use]
    pub fn with_opaque_provider(mut self, provider: &'a str) -> Self {
        self.opaque_provider = Some(provider);
        self
    }

    /// The clients report cache writes.
    #[must_use]
    pub fn with_cache_writes(mut self) -> Self {
        self.cache_writes = true;
        self
    }

    /// The key the clients were built with.
    #[must_use]
    pub fn with_secret(mut self, secret: &'a str) -> Self {
        self.secret = secret;
        self
    }
}

/// Runs every case against clients from `harness`.
///
/// # Errors
/// The first failing case, as `"<case>: <what went wrong>"`.
pub fn run(harness: &Harness<'_>) -> Result<(), String> {
    for (name, case) in cases::all(harness) {
        case(harness).map_err(|why| format!("{name}: {why}"))?;
    }
    Ok(())
}

/// The names of the cases `run` plays against `harness`, in order.
pub fn case_names(harness: &Harness<'_>) -> Vec<&'static str> {
    cases::all(harness).into_iter().map(|(name, _)| name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::FakeLlm;

    fn fake(script: Script) -> Box<dyn LlmClient> {
        Box::new(FakeLlm::new(script))
    }

    #[test]
    fn fake_llm_passes_every_case() {
        let harness =
            Harness::new(&fake).with_opaque_provider("p").with_cache_writes().with_secret("sk-conformance-0001");
        assert_eq!(run(&harness), Ok(()));
    }

    #[test]
    fn the_suite_lists_every_d10_row() {
        let harness = Harness::new(&fake).with_opaque_provider("p").with_cache_writes();
        let names = case_names(&harness);
        for row in [
            "text only",
            "one tool call",
            "two parallel calls",
            "opaque round trip",
            "cumulative usage",
            "refusal and max output",
            "429 with retry-after retries once",
            "429 without retry-after is not retried",
            "retries stop after four",
            "401 is Auth and not retried",
            "context error names the window",
            "a drop after Start is never retried",
            "usage survives a drop",
            "malformed transport data",
            "cancel mid-stream",
            "cancel during backoff",
            "any split gives the same turn",
            "the key never appears",
            "the worked example costs 633, 101 and 734",
            "requests the model cannot take are refused first",
        ] {
            assert!(names.contains(&row), "missing case {row:?}");
        }
    }

    #[test]
    fn optional_cases_are_skipped_when_the_provider_lacks_the_feature() {
        let names = case_names(&Harness::new(&fake));
        assert!(!names.contains(&"opaque round trip"));
        assert!(!names.contains(&"the worked example costs 633, 101 and 734"));
    }

    #[test]
    fn a_fake_that_retries_after_start_fails_the_named_case() {
        let broken = |script: Script| -> Box<dyn LlmClient> { Box::new(RetriesAfterStart::new(script)) };
        let got = run(&Harness::new(&broken));
        let why = got.err().unwrap_or_default();
        assert!(why.starts_with("a drop after Start is never retried:"), "{why}");
    }
}
