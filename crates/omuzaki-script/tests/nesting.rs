//! Deeply nested values can't overflow the stack: every run happens on a worker thread sized for the deepest nesting the
//! limits allow (`Limits::worker_stack_bytes`), so these scripts end with a limit error instead of aborting the process.
//! The tests run on the default test-thread stack, which is far too small for the nesting they build.

#![allow(clippy::unwrap_used, clippy::result_large_err)] // a test file: a failed unwrap is the failure

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};

use common::{Entry, FakeRunner, Flag, request, run_with};
use omuzaki_doc_api::DocPath;
use omuzaki_doc_api::Value;
use omuzaki_ops_api::{OpError, Outcome, Request};
use omuzaki_script::{Calls, LimitKind, Limits, ScriptErrorKind, ScriptHost};

/// Limits small enough to reach the deepest nesting quickly: the nesting is bounded by the array and map totals.
fn small_nesting_limits() -> Limits {
    Limits { array_len: 3_000, map_len: 3_000, operations: 1_000_000_000, seconds: 120, ..Limits::console() }
}

fn ends_with(source: &str, kinds: &[LimitKind]) {
    let (runner, result) = run_with(request(source), &Flag::default(), small_nesting_limits(), FakeRunner::default());
    let kind = result.unwrap_err().kind;
    assert!(kinds.iter().any(|expected| kind == ScriptErrorKind::Limit(*expected)), "{source}: {kind:?}");
    runner.assert_rolled_back();
}

#[test]
fn nesting_arrays_as_deep_as_the_limits_allow_ends_with_the_size_limit() {
    ends_with("let v = []; loop { v = [v]; }", &[LimitKind::ArraySize]);
}

#[test]
fn nesting_maps_as_deep_as_the_limits_allow_ends_with_the_size_limit() {
    ends_with("let v = #{}; loop { v = #{ a: v }; }", &[LimitKind::MapSize]);
}

#[test]
fn nesting_both_alternately_ends_with_a_size_limit() {
    ends_with("let v = []; loop { v = [#{ a: v }]; }", &[LimitKind::ArraySize, LimitKind::MapSize]);
}

#[test]
fn growing_a_chain_with_push_ends_with_the_size_limit() {
    ends_with("let v = []; loop { let w = []; w.push(v); v = w; }", &[LimitKind::ArraySize]);
}

#[test]
fn a_value_just_under_the_limit_can_be_copied_compared_and_printed_without_overflowing() {
    // 2,500 levels: built, then cloned (`let w = v`), compared (recursively) and turned into text, each once per level.
    let source = "
        let v = [];
        for i in 0..2500 { v = [v]; }
        let w = v;
        let same = v == w;
        let text = `${v}`;
        text.len()
    ";
    let (_, result) = run_with(request(source), &Flag::default(), small_nesting_limits(), FakeRunner::default());
    // The text of 2,500 nested pairs of brackets and the empty pair inside them is 5,002 bytes, well inside the result limit.
    assert_eq!(result.unwrap().value, serde_json::json!(5_002));
}

#[test]
fn a_nested_result_is_refused_for_its_depth_rather_than_converted() {
    let source = "let v = []; for i in 0..2500 { v = [v]; } v";
    let (_, result) = run_with(request(source), &Flag::default(), small_nesting_limits(), FakeRunner::default());
    assert_eq!(result.unwrap_err().kind, ScriptErrorKind::Limit(LimitKind::Depth));
}

#[test]
fn the_console_and_job_limits_need_a_stack_the_host_can_give() {
    let console = Limits::console().worker_stack_bytes().unwrap();
    let job = Limits::job(None).worker_stack_bytes().unwrap();
    assert_eq!(console, job, "the stack follows the size limits, which the two tables share");
    assert!(console <= Limits::MAX_WORKER_STACK_BYTES);
    // 110,001 levels of at least a kibibyte each, plus the engine's own frames.
    assert!(console > 110_001 * 1024, "{console}");
}

#[test]
fn limits_that_would_need_more_than_the_cap_are_refused_before_a_run() {
    for limits in [
        Limits { array_len: usize::MAX, ..Limits::console() },
        Limits { map_len: 1 << 40, ..Limits::console() },
        Limits { array_len: 1 << 30, ..Limits::console() },
    ] {
        assert_eq!(limits.worker_stack_bytes(), None);
        let (runner, result) = run_with(request("1"), &Flag::default(), limits, FakeRunner::default());
        let error = result.unwrap_err();
        assert_eq!(error.kind, ScriptErrorKind::Refused);
        assert!(error.message.contains("array_len"), "{error}");
        assert_eq!(runner.entries(), Vec::<Entry>::new(), "nothing was started");
    }
}

/// Panics in the middle of a run.
struct Panics(AtomicUsize);

impl Calls for Panics {
    fn call(&self, _: Request) -> Result<Outcome, OpError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        panic!("a bug in the calls");
    }

    fn get(&self, _: &DocPath) -> Result<Option<Value>, OpError> {
        Ok(None)
    }
}

#[test]
fn a_panic_in_the_run_is_raised_again_in_the_caller() {
    let runner = FakeRunner::default();
    let host = ScriptHost::new(&omuzaki_ops_api::Runner::ops(&runner), Limits::console());
    let calls = Panics(AtomicUsize::new(0));
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        host.run(request("ops::face::set_profile(#{ character: \"a\" })"), &calls, &Flag::default())
    }));
    let payload = outcome.expect_err("the panic reaches the caller");
    assert_eq!(payload.downcast_ref::<&str>(), Some(&"a bug in the calls"));
    assert_eq!(calls.0.load(Ordering::SeqCst), 1);
}

#[test]
fn the_run_happens_on_a_thread_of_its_own() {
    // `print` runs on the engine's thread: a script can't name it, but the calls handle can.
    struct Names(std::sync::Mutex<Vec<Option<String>>>);
    impl Calls for Names {
        fn call(&self, _: Request) -> Result<Outcome, OpError> {
            self.0.lock().unwrap().push(std::thread::current().name().map(str::to_owned));
            Ok(Outcome::new(omuzaki_ops_api::Plan::default()))
        }

        fn get(&self, _: &DocPath) -> Result<Option<Value>, OpError> {
            Ok(None)
        }
    }
    let runner = FakeRunner::default();
    let host = ScriptHost::new(&omuzaki_ops_api::Runner::ops(&runner), Limits::console());
    let calls = Names(std::sync::Mutex::new(Vec::new()));
    host.run(request("ops::face::set_profile(#{ character: \"a\" })"), &calls, &Flag::default()).unwrap();
    assert_eq!(*calls.0.lock().unwrap(), [Some("omuzaki-script".to_owned())]);
}
