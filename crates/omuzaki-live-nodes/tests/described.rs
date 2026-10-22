//! Every node kind, port and parameter of this crate is described in English (#1274): the summary, each port's
//! summary (or a dynamic port's label) and each parameter's doc comment are the `node-<id>` texts translators start
//! from.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used)]

use omuzaki_live_api::node::node_kinds;
use serde_json::{Value, json};

/// Parameters that make a kind's ports, for kinds whose defaults ask for a choice first.
fn sample(id: &str) -> Option<Value> {
    if id == "var.toggle" {
        return Some(json!({ "variable": "lights", "type": "bool" }));
    }
    Some(match id.split('.').next()? {
        "var" => json!({ "variable": "count", "type": "number" }),
        "group" => json!({ "group": "chat_games" }),
        "chat" => json!({ "name": "hug" }),
        "text" => json!({ "template": "Hi {user}" }),
        _ => return None,
    })
}

#[test]
fn every_node_port_and_parameter_is_described() {
    omuzaki_live_nodes::link();
    let mut missing = Vec::new();
    let mut kinds = 0;
    for kind in node_kinds() {
        let meta = (kind.meta)();
        kinds += 1;
        if meta.summary.trim().is_empty() {
            missing.push(format!("{}: no summary", meta.id));
        }
        let params = sample(meta.id).unwrap_or_else(|| (kind.defaults)());
        let ports = (kind.ports)(&params).or_else(|_| (kind.ports)(&(kind.defaults)()));
        let Ok(ports) = ports else {
            missing.push(format!("{}: no ports from its defaults or a sample", meta.id));
            continue;
        };
        for port in ports.inputs.iter().chain(&ports.outputs) {
            let described = port.summary.is_some_and(|s| !s.trim().is_empty()) || port.label.is_some();
            if !described {
                missing.push(format!("{}: port {} has no summary", meta.id, port.name));
            }
        }
        let schema = serde_json::to_value((kind.schema)()).unwrap();
        for (key, property) in schema.get("properties").and_then(Value::as_object).into_iter().flatten() {
            let described = property.get("description").and_then(Value::as_str).is_some_and(|s| !s.trim().is_empty());
            if !described {
                missing.push(format!("{}: parameter {key} has no description", meta.id));
            }
        }
    }
    assert!(kinds >= 39, "only {kinds} kinds registered");
    assert_eq!(missing, Vec::<String>::new());
}
