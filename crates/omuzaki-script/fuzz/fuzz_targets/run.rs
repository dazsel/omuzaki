//! `ScriptHost::run` on any bytes against the fake runner: no panic, no hang past the limits, every error positioned,
//! and a failed run leaves nothing committed.

#![no_main]

// One fake, not two: the same runner the tests use (`tests/common/mod.rs`).
#[path = "../../tests/common/mod.rs"]
mod common;

use common::{Flag, request, run_with};
use libfuzzer_sys::fuzz_target;
use omuzaki_script::Limits;

fuzz_target!(|data: &[u8]| {
    let source = String::from_utf8_lossy(data).into_owned();
    // The console limits, with a short wall clock so one input can't take a whole fuzzing run.
    let limits = Limits { seconds: 2, ..Limits::console() };
    let mut request = request(&source);
    // The first byte picks a dry run, so both paths are exercised from the same corpus.
    request.dry_run = data.first().is_some_and(|byte| byte & 1 == 1);
    let (runner, result) = run_with(request, &Flag::default(), limits, common::FakeRunner::default());
    match result {
        Ok(outcome) => assert!(outcome.calls <= Limits::console().calls),
        Err(error) => {
            assert!(error.line >= 1 && error.column >= 1, "unpositioned error: {error:?}");
            runner.assert_rolled_back();
        }
    }
});
