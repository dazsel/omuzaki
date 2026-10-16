//! `ScriptHost::check` on any bytes: no panic, no hang, every problem positioned.

#![no_main]

// One fake, not two: the same runner the tests use (`tests/common/mod.rs`).
#[path = "../../tests/common/mod.rs"]
mod common;

use std::sync::OnceLock;

use common::FakeRunner;
use libfuzzer_sys::fuzz_target;
use omuzaki_ops_api::Runner;
use omuzaki_script::{Limits, ScriptHost};

static HOST: OnceLock<ScriptHost> = OnceLock::new();

fuzz_target!(|data: &[u8]| {
    let host = HOST.get_or_init(|| ScriptHost::new(&FakeRunner::default().ops(), Limits::console()));
    let source = String::from_utf8_lossy(data);
    let report = host.check(&source);
    // A problem at the end of the input sits on the line after the last break.
    let last_line = source.chars().filter(|c| matches!(c, '\n' | '\r')).count() + 1;
    for problem in report.problems() {
        assert!(problem.line >= 1 && problem.column >= 1, "unpositioned problem: {problem:?}");
        assert!(problem.line as usize <= last_line, "a line past the end of the source: {problem:?}");
    }
    for call in report.ops.iter().chain(&report.unknown_ops) {
        assert!(call.line >= 1 && call.column >= 1, "unpositioned call: {call:?}");
    }
    // The report is the JSON the console and `script.check` send: it must always serialise.
    assert!(serde_json::to_string(&report).is_ok());
});
