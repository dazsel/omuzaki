//! `op.run`'s acceptance (#780) through the node harness, with test ops registered here: a placeholder becomes a typed
//! input and the request carries exactly the filled-in arguments; a misspelt key and a scope a show never gets are
//! refused when the show compiles, the latter naming the node that does it instead; `$$` writes a literal `$`; a
//! refused reply fires `failed` with the op's code and message; `needs` lists a broadcast op's scope and the op.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use omuzaki_core::time::Seconds;
use omuzaki_live_api::need::Need;
use omuzaki_live_api::node::ParamErrors;
use omuzaki_live_api::value::{PortType, Val};
use omuzaki_live_mock::harness::NodeHarness;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Switch the program scene, with a fade.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Cut {
    /// The scene.
    scene: String,
    /// The fade, in milliseconds.
    fade_ms: u32,
}

impl Op for Cut {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.cut",
            summary: "Switch the scene.",
            kind: OpKind::Edit,
            scope: Scope::Broadcast,
            undo: Undo::None,
        }
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "scene": self.scene })))
    }
}
register_op!(Cut);

/// Fetch a web page.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Fetch {
    /// Where.
    url: String,
}

impl Op for Fetch {
    fn meta() -> OpMeta {
        OpMeta { name: "test.fetch", summary: "Fetch a page.", kind: OpKind::Edit, scope: Scope::Net, undo: Undo::None }
    }

    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "url": self.url })))
    }
}
register_op!(Fetch);

fn node(params: Value) -> Result<NodeHarness, ParamErrors> {
    omuzaki_live_nodes::link();
    NodeHarness::by_id("op.run", params)
}

fn english(errors: &ParamErrors) -> String {
    errors.0.iter().map(|e| format!("{}: {:?}", e.path, e.text)).collect::<Vec<_>>().join("\n")
}

#[test]
fn a_text_placeholder_becomes_an_input_and_the_request_is_exactly_the_filled_in_args() {
    let mut h = node(json!({ "op": "test.cut", "args": { "scene": "$in.scene", "fade_ms": 300 } })).unwrap();
    assert_eq!(h.ports().input("scene").unwrap().ty, PortType::Text);
    h.set_input("scene", "Main");
    h.send("in");
    assert_eq!(h.calls().len(), 1);
    assert_eq!(h.calls()[0].op, "test.cut");
    assert_eq!(h.calls()[0].args, json!({ "scene": "Main", "fade_ms": 300 }));
}

#[test]
fn a_misspelt_argument_is_refused_naming_it() {
    let errors = node(json!({ "op": "test.cut", "args": { "scene": "Main", "fade_msec": 300 } })).err().unwrap();
    assert!(errors.0.iter().any(|e| e.path == "args.fade_msec"), "{}", english(&errors));
}

#[test]
fn a_missing_argument_or_a_constant_of_the_wrong_type_is_refused_before_any_event() {
    let missing = node(json!({ "op": "test.cut", "args": { "scene": "$in.scene" } })).err().unwrap();
    assert!(english(&missing).contains("fade_ms"), "{}", english(&missing));
    let wrong = node(json!({ "op": "test.cut", "args": { "scene": "Main", "fade_ms": "slow" } })).err().unwrap();
    assert!(wrong.0[0].path.starts_with("args"), "{}", english(&wrong));
    // A placeholder stands in with its type's default, so a wired argument alone is enough.
    assert!(node(json!({ "op": "test.cut", "args": { "scene": "$in.scene", "fade_ms": "$in.fade" } })).is_ok());
}

#[test]
fn an_op_in_the_net_scope_is_refused_with_the_node_that_does_it() {
    let errors = node(json!({ "op": "test.fetch", "args": { "url": "https://example.com" } })).err().unwrap();
    assert_eq!(errors.0[0].path, "op");
    assert!(english(&errors).contains("net.http"), "{}", english(&errors));
}

#[test]
fn two_dollars_write_one() {
    let mut h = node(json!({ "op": "test.cut", "args": { "scene": "$$x", "fade_ms": 0 } })).unwrap();
    h.send("in");
    assert_eq!(h.calls()[0].args, json!({ "scene": "$x", "fade_ms": 0 }));
}

#[test]
fn a_reply_error_fires_failed_with_its_code_then_its_message() {
    let mut h = node(json!({ "op": "test.cut", "args": { "scene": "Main", "fade_ms": 0 } })).unwrap();
    h.respond("test.cut", Seconds(0.0), |_| Err(OpError::conflict("OBS isn't connected")));
    h.send("in");
    h.advance(Seconds(0.0));
    let fired = h.fired();
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].port.as_str(), "failed");
    let error = fired[0].values.iter().find(|(p, _)| p.as_str() == "error").map(|(_, v)| v.clone());
    assert_eq!(error, Some(Val::text("conflict: OBS isn't connected")));
}

#[test]
fn a_broadcast_op_needs_its_scope_and_the_op() {
    let h = node(json!({ "op": "test.cut", "args": { "scene": "Main", "fade_ms": 0 } })).unwrap();
    assert_eq!(h.needs(), &[Need::Scope(Scope::Broadcast), Need::op("test.cut")]);
}
