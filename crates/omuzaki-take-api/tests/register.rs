//! `register_take_state!` fills a registry that no list names. This test binary registers exactly two providers.

use omuzaki_take_api::register_take_state;
use omuzaki_take_api::state::{is_state_provider, state_providers};
use serde_json::{Value, json};

fn expressions() -> Value {
    json!({ "held": ["smile"] })
}

fn mic_mode() -> Value {
    json!("push_to_talk")
}

// Registered out of name order, to show the registry sorts.
register_take_state!("mic_mode", mic_mode);
register_take_state!("expressions", expressions);

#[test]
fn both_providers_are_seen_in_name_order() {
    let providers = state_providers();
    let names: Vec<&str> = providers.iter().map(|p| p.name).collect();
    assert_eq!(names, ["expressions", "mic_mode"]);
    assert_eq!((providers[0].capture)(), json!({ "held": ["smile"] }));
    assert_eq!((providers[1].capture)(), json!("push_to_talk"));
}

#[test]
fn the_link_gate_sees_the_registrations_by_name() {
    let names: Vec<&str> =
        omuzaki_core::link::registered().into_iter().filter(|r| r.kind == "take_state").map(|r| r.name).collect();
    assert_eq!(names.len(), 2, "{names:?}");
}

#[test]
fn a_reader_knows_exactly_the_linked_providers() {
    assert!(is_state_provider("expressions") && is_state_provider("mic_mode"));
    assert!(!is_state_provider("obs_scene"), "a provider this binary does not link is unknown");
}
