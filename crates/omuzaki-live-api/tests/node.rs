//! The node contract (#952): a test kind's registration, parameter checks and schema; the D3 coercion table; value
//! rules; and the port rules.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use omuzaki_live_api::need::{Need, NetScheme};
use omuzaki_live_api::node::{
    Category, DynNodeKind, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamErrors, Ports,
    find_node_kind, node_kinds, validate_ports,
};
use omuzaki_live_api::value::{Coercion, PortType, Val, ValError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Parameters of the test kind.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct EchoParams {
    /// What to prefix each value with.
    prefix: String,
    /// The host it pretends to send to.
    target: Target,
}

/// Where the echo pretends to go.
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Target {
    /// The host.
    host: String,
    /// The port.
    port: u16,
}

impl Default for EchoParams {
    fn default() -> Self {
        Self { prefix: ">".into(), target: Target { host: "127.0.0.1".into(), port: 7474 } }
    }
}

/// Passes `value` on, prefixed.
struct Echo {
    prefix: String,
}

impl NodeKind for Echo {
    type Params = EchoParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("test.echo", "Passes a value on.", NodeClass::Active, Category::Flow)
    }

    fn ports(_: &EchoParams) -> Ports {
        Ports::new()
            .event_in("in", "Each event.")
            .value_in("value", PortType::Text, Val::text(""), "The value.")
            .event_out("out", "After the value is set.")
            .value_out("value", PortType::Text, "The value, prefixed.")
    }

    fn needs(params: &EchoParams) -> Vec<Need> {
        vec![Need::host(NetScheme::Http, params.target.host.clone(), params.target.port)]
    }

    fn check(params: &EchoParams) -> Result<(), ParamErrors> {
        if params.target.port == 0 {
            return Err(ParamErrors::one("target.port", omuzaki_live_api::l10n::params_invalid("port 0")));
        }
        Ok(())
    }

    fn new(params: EchoParams) -> Self {
        Self { prefix: params.prefix }
    }
}

impl Node for Echo {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Event { port: "in", .. } => {
                let value = cx.input_text("value")?;
                cx.set("value", format!("{}{value}", self.prefix))?;
                cx.fire("out")
            }
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Trigger { .. } | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}

omuzaki_live_api::register_node!(Echo);

#[test]
fn a_kind_registers_once_and_is_found_by_id() {
    let echoes: Vec<_> = node_kinds().filter(|r| (r.meta)().id == "test.echo").collect();
    assert_eq!(echoes.len(), 1);
    let echo = find_node_kind("test.echo").unwrap();
    assert_eq!(echo.meta().category, Category::Flow);
    assert_eq!(echo.kind_id().unwrap().message_stem(), "test-echo");
    assert!(find_node_kind("test.nothing").is_none());
    let recorded =
        omuzaki_core::link::registered().into_iter().filter(|r| r.kind == "node" && r.name == "Echo").count();
    assert_eq!(recorded, 1);
}

#[test]
fn the_params_schema_is_inlined() {
    let schema = serde_json::to_string(&find_node_kind("test.echo").unwrap().schema()).unwrap();
    assert!(!schema.contains("$ref") && !schema.contains("$defs"), "{schema}");
    assert!(schema.contains("The host it pretends to send to."));
}

#[test]
fn check_fills_defaults_and_refuses_unknown_parameters_at_their_path() {
    let echo = find_node_kind("test.echo").unwrap();
    assert_eq!(echo.check(&json!({})).unwrap(), json!({"prefix": ">", "target": {"host": "127.0.0.1", "port": 7474}}));
    assert_eq!(echo.check(&json!(null)).unwrap(), echo.defaults());
    assert_eq!(echo.check(&json!({"prefix": "#"})).unwrap()["prefix"], json!("#"));

    let unknown = echo.check(&json!({"prefx": "#"})).unwrap_err();
    assert_eq!(unknown.0[0].path, "prefx");
    let nested = echo.check(&json!({"target": {"host": "h", "port": 1, "tls": true}})).unwrap_err();
    assert_eq!(nested.0[0].path, "target.tls");
    let wrong_type = echo.check(&json!({"target": {"host": "h", "port": "x"}})).unwrap_err();
    assert_eq!(wrong_type.0[0].path, "target.port");
    let missing = echo.check(&json!({"target": {"host": "h"}})).unwrap_err();
    assert_eq!(missing.0[0].path, "target.port");
    let refused = echo.check(&json!({"target": {"host": "h", "port": 0}})).unwrap_err();
    assert_eq!(refused.0[0].path, "target.port");
    assert_eq!(echo.check(&json!([1])).unwrap_err().0[0].text.id, "live-api-params-not-object");
}

#[test]
fn the_registration_derives_ports_needs_and_instances_from_json() {
    let echo = find_node_kind("test.echo").unwrap();
    let ports = echo.ports(&json!({})).unwrap();
    assert_eq!(ports.input("value").unwrap().default, Some(Val::text("")));
    assert_eq!(
        echo.needs(&json!({"target": {"host": "obs.local", "port": 4455}})).unwrap(),
        [Need::host(NetScheme::Http, "obs.local", 4455)]
    );
    assert_eq!(echo.listens(&json!({})).unwrap(), Vec::new());
    assert!(echo.instantiate(&json!({"prefx": 1})).is_err() && echo.instantiate(&json!({})).is_ok());
}

/// A value of each type, for the coercion table.
fn sample(ty: PortType) -> Val {
    match ty {
        PortType::Bool => Val::Bool(true),
        PortType::Number => Val::Number(3.0),
        PortType::Text => Val::text("3"),
        PortType::List => Val::from(vec![Val::Number(1.0)]),
        PortType::Json => Val::from(json!({"a": 1})),
        PortType::Event => unreachable!("events carry no value"),
    }
}

#[test]
fn the_coercion_table_is_the_designs() {
    use Coercion::{CheckedWhenRead as Checked, Converts, Refused, Same};
    use PortType::{Bool, Event, Json, List, Number, Text};
    // Rows: from; columns: to, in the order of PortType::ALL (event, bool, number, text, list, json).
    let table = [
        (Event, [Same, Refused, Refused, Refused, Refused, Refused]),
        (Bool, [Refused, Same, Refused, Converts, Refused, Converts]),
        (Number, [Refused, Refused, Same, Converts, Refused, Converts]),
        (Text, [Refused, Refused, Refused, Same, Refused, Converts]),
        (List, [Refused, Refused, Refused, Refused, Same, Converts]),
        (Json, [Refused, Checked, Checked, Checked, Checked, Same]),
    ];
    for (from, row) in table {
        for (to, expected) in PortType::ALL.into_iter().zip(row) {
            assert_eq!(PortType::coercion(from, to), expected, "{from:?} to {to:?}");
            if from == Event || expected == Checked {
                continue;
            }
            let converted = sample(from).coerce(to);
            match expected {
                Same | Converts => assert_eq!(converted.unwrap().ty(), to, "{from:?} to {to:?}"),
                Refused => assert_eq!(converted, Err(ValError::refused(from, to))),
                Checked => unreachable!(),
            }
        }
    }
}

#[test]
fn json_is_checked_when_read() {
    assert_eq!(Val::from(json!(true)).coerce(PortType::Bool), Ok(Val::Bool(true)));
    assert_eq!(Val::from(json!(2.5)).coerce(PortType::Number), Ok(Val::Number(2.5)));
    assert_eq!(Val::from(json!("hi")).coerce(PortType::Text), Ok(Val::text("hi")));
    assert_eq!(
        Val::from(json!([1, "a"])).coerce(PortType::List),
        Ok(Val::from(vec![Val::Number(1.0), Val::text("a")]))
    );
    assert_eq!(Val::from(json!("hi")).coerce(PortType::Number), Err(ValError::mismatch(PortType::Number, "text")));
    assert!(Val::from(json!({"a": 1})).coerce(PortType::List).is_err());
}

#[test]
fn numbers_are_finite_and_print_in_their_shortest_form() {
    assert_eq!(Val::number(f64::NAN), Err(ValError::NotFinite));
    assert_eq!(Val::number(f64::INFINITY), Err(ValError::NotFinite));
    assert_eq!(Val::number(f64::NEG_INFINITY), Err(ValError::NotFinite));
    assert_eq!(Val::Number(3.0).coerce(PortType::Text), Ok(Val::text("3")));
    assert_eq!(Val::Number(0.1 + 0.2).coerce(PortType::Text), Ok(Val::text("0.30000000000000004")));
    assert_eq!(Val::Number(-2.5).coerce(PortType::Text), Ok(Val::text("-2.5")));
    assert_eq!(Val::Bool(false).coerce(PortType::Text), Ok(Val::text("false")));
    assert_eq!(Val::Number(f64::NAN).check(), Err(ValError::NotFinite));
}

#[test]
fn port_rules_are_enforced() {
    let codes = |ports: &Ports, class| -> Vec<String> {
        validate_ports(ports, class)
            .err()
            .map(|e| e.0.into_iter().map(|e| e.text.id.to_string()).collect())
            .unwrap_or_default()
    };
    let ok = Ports::new().event_in("in", "In.").value_out("v", PortType::Number, "V.").event_out("out", "Out.");
    assert_eq!(codes(&ok, NodeClass::Active), Vec::<String>::new());

    let duplicate = Ports::new().event_in("in", "In.").event_in("in", "Again.");
    assert_eq!(codes(&duplicate, NodeClass::Active), ["live-api-port-duplicate"]);

    let event_default = Ports::new().value_in("in", PortType::Event, Val::Bool(true), "In.");
    assert_eq!(codes(&event_default, NodeClass::Active), ["live-api-port-event-value"]);

    let pure_event = Ports::new().event_in("in", "In.").live_out("v", PortType::Number, "V.");
    assert_eq!(codes(&pure_event, NodeClass::Pure), ["live-api-port-pure-event"]);

    let pure_latched = Ports::new().value_out("v", PortType::Number, "V.");
    assert_eq!(codes(&pure_latched, NodeClass::Pure), ["live-api-port-pure-not-live"]);
    assert_eq!(codes(&pure_latched.all_live(), NodeClass::Pure), Vec::<String>::new());

    let trigger_in = Ports::new().event_in("in", "In.").event_out("fired", "Fired.");
    assert_eq!(codes(&trigger_in, NodeClass::Trigger), ["live-api-port-trigger-event-in"]);

    let wrong_default = Ports::new().value_in("n", PortType::Number, Val::text("1"), "N.");
    assert_eq!(codes(&wrong_default, NodeClass::Active), ["live-api-port-default-type"]);

    let bad_name = Ports::new().event_in("In", "In.");
    assert_eq!(codes(&bad_name, NodeClass::Active), ["live-api-port-bad-name"]);

    let names: Vec<&'static str> = (0..65).map(|i| &*Box::leak(format!("p{i}").into_boxed_str())).collect();
    let many = names.iter().fold(Ports::new(), |ports, name| ports.event_out(name, "Out."));
    assert_eq!(codes(&many, NodeClass::Active), ["live-api-port-too-many"]);
    let sixty_four = names[..64].iter().fold(Ports::new(), |ports, name| ports.event_out(name, "Out."));
    assert_eq!(codes(&sixty_four, NodeClass::Active), Vec::<String>::new());
}

#[test]
fn an_unhandled_input_names_itself() {
    let error = NodeError::unhandled(&Input::wake(omuzaki_live_api::node::WakeId(1)));
    assert_eq!(error.text.id, "live-api-unhandled");
    assert_eq!(error.text.arg("input"), Some(&omuzaki_l10n_api::Arg::Id("wake".into())));
}
