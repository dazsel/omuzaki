//! `MockPlugins` passes the conformance suite, and its own promises: injected faults, deterministic scripts.

use omuzaki_ops_api::{Grants, Scope};
use omuzaki_plugin_api::Plugins;
use omuzaki_plugin_api::limits::LOG_BURST;
use omuzaki_plugin_api::state::{FaultKind, LogLevel, PluginState};
use omuzaki_plugin_mock::conformance::{self, Behaviour, Subject, World, manifest, person};
use omuzaki_plugin_mock::{MockGuest, MockOp, MockPlugins, MockSubject, ScriptedCall};

#[test]
fn the_mock_passes_the_conformance_suite() {
    conformance::run(&MockSubject);
}

#[test]
fn an_injected_fault_is_seen_in_info() {
    let world = World::new();
    let plugins = MockSubject.build(&world, vec![(manifest("example", &[]), Behaviour::Ok)]);
    let id = plugins.list()[0].id.clone();
    plugins.enable(&id, &person()).unwrap();
    let faulted = plugins.fault(&id, FaultKind::Trap).unwrap();
    assert_eq!(faulted.state, PluginState::faulted(FaultKind::Trap));
    assert_eq!(faulted.session, None);
}

#[test]
fn two_runs_of_a_script_make_the_same_calls() {
    let run = || {
        let world = World::new();
        let script =
            (0..3).map(|i| ScriptedCall { op: format!("test.op{i}"), args: serde_json::json!({ "n": i }) }).collect();
        let plugins = MockSubject.build(&world, vec![(manifest("example", &[Scope::Read]), Behaviour::Calls(script))]);
        for i in 0..3 {
            plugins.add_op(format!("test.op{i}"), MockOp { scope: Scope::Read, output: Ok("1".into()) });
        }
        let id = plugins.list()[0].id.clone();
        plugins.enable(&id, &person()).unwrap();
        (plugins.calls(), plugins.log(&id, 0, 100).unwrap())
    };
    let (calls, log) = run();
    assert_eq!(calls.len(), 3);
    assert_eq!(calls[2].args, r#"{"n":2}"#);
    assert_eq!(run(), (calls, log));
}

#[test]
fn a_log_flood_keeps_the_burst_and_reports_the_rest_as_the_host_does() {
    let world = World::new();
    let plugins = MockPlugins::new(world.clock.clone(), world.sessions.clone(), Grants::person());
    let guest = MockGuest::new(|host, _| {
        for i in 0..1_000 {
            host.log(LogLevel::Info, &format!("line {i}"));
        }
        Ok(Ok(()))
    });
    let id = plugins.add(manifest("flood", &[]), guest).unwrap();
    plugins.enable(&id, &person()).unwrap();
    let kept = plugins.log(&id, 0, usize::MAX).unwrap();
    assert_eq!(kept.len(), LOG_BURST as usize);
    world.clock.advance(omuzaki_core::time::Seconds(1.0));
    plugins.disable(&id, &person()).unwrap();
    plugins.enable(&id, &person()).unwrap();
    let texts: Vec<String> = plugins.log(&id, 0, usize::MAX).unwrap().into_iter().map(|l| l.text).collect();
    assert_eq!(texts[LOG_BURST as usize], "900 lines dropped");
    assert_eq!(texts[LOG_BURST as usize + 1], "line 0", "the second init's first line follows the notice");
}
