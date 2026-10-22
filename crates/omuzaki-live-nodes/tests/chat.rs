//! `chat.command` to its acceptance (#738), through the node harness on fake time with fixture chat.

// Helpers outside #[test] functions unwrap too: a panic there is a test failure (clippy.toml).
#![allow(clippy::unwrap_used, clippy::float_cmp)]

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::payloads::Role;
use omuzaki_live_api::node::ParamErrors;
use omuzaki_live_api::value::Val;
use omuzaki_live_mock::fixtures;
use omuzaki_live_mock::harness::NodeHarness;
use serde_json::{Value, json};

fn node(params: Value) -> Result<NodeHarness, ParamErrors> {
    omuzaki_live_nodes::link();
    NodeHarness::by_id("chat.command", params)
}

fn ports(h: &NodeHarness) -> Vec<String> {
    h.fired().iter().map(|f| f.port.to_string()).collect()
}

fn out(h: &NodeHarness, i: usize, port: &str) -> Option<Val> {
    h.fired()[i].values.iter().find(|(p, _)| p.as_str() == port).map(|(_, v)| v.clone())
}

fn texts(list: Option<Val>) -> Vec<String> {
    let Some(Val::List(items)) = list else { panic!("not a list: {list:?}") };
    items.iter().map(|v| v.to_json().as_str().unwrap().to_owned()).collect()
}

#[test]
fn matching_and_arguments() {
    // (message, fires, args)
    let table: [(&str, bool, &[&str]); 9] = [
        ("!hug @Mochi", true, &["mochi"]),
        ("!HUG", true, &[]),
        ("!cuddle Pell", true, &["Pell"]),
        ("! hug", false, &[]),
        ("!hugs", false, &[]),
        ("hug", false, &[]),
        ("!hug \"big warm\" one", true, &["big warm", "one"]),
        ("！ｈｕｇ　ことり", true, &["ことり"]),
        ("  !hug\tJuno", true, &["Juno"]),
    ];
    for (text, fires, args) in table {
        let mut h = node(json!({ "name": "hug", "aliases": ["cuddle"] })).unwrap();
        h.trigger(fixtures::chat("Kiri", text));
        if fires {
            assert_eq!(ports(&h), ["fired"], "{text}");
            assert_eq!(texts(out(&h, 0, "args")), args, "{text}");
        } else {
            assert_eq!(h.fired().len(), 0, "{text}");
        }
    }
}

#[test]
fn the_outputs_carry_the_sender_and_the_rest() {
    let mut h = node(json!({ "name": "hug" })).unwrap();
    let kiri = fixtures::user_with("Kiri", [Role::Vip, Role::Subscriber]);
    h.trigger(fixtures::chat_from(kiri.clone(), "!hug   everyone here"));
    assert_eq!(out(&h, 0, "user"), Some(Val::text("Kiri")));
    assert_eq!(out(&h, 0, "user_id"), Some(Val::text(kiri.id.as_str())));
    assert_eq!(out(&h, 0, "login"), Some(Val::text("kiri")));
    assert_eq!(out(&h, 0, "platform"), Some(Val::text(fixtures::PLATFORM)));
    assert_eq!(out(&h, 0, "rest"), Some(Val::text("everyone here")));
    let mut roles = texts(out(&h, 0, "roles"));
    roles.sort();
    assert_eq!(roles, ["subscriber", "vip"]);
}

#[test]
fn each_role_meets_or_misses_the_least_role() {
    let ladder = ["everyone", "subscriber", "vip", "moderator", "broadcaster"];
    let users: [(&str, Vec<Role>); 5] = [
        ("Pell", vec![]),
        ("Mochi", vec![Role::Subscriber]),
        ("Juno", vec![Role::Vip]),
        ("Kiri", vec![Role::Moderator]),
        ("Host", vec![Role::Broadcaster]),
    ];
    for (need, min_role) in ladder.iter().enumerate() {
        for (rank, (name, roles)) in users.iter().enumerate() {
            let mut h = node(json!({ "name": "so", "min_role": min_role })).unwrap();
            h.trigger(fixtures::chat_from(fixtures::user_with(name, roles.clone()), "!so"));
            let want = if rank >= need { "fired" } else { "denied" };
            assert_eq!(ports(&h), [want], "{name} against {min_role}");
        }
    }
}

#[test]
fn global_and_per_user_cooldowns_on_fake_time() {
    let mut h = node(json!({ "name": "hug", "cooldown_s": 30, "user_cooldown_s": 60 })).unwrap();
    for (i, name) in ["Kiri", "Mochi", "Pell"].into_iter().enumerate() {
        h.advance_to(Seconds(5.0 * f64::from(u8::try_from(i).unwrap())));
        h.trigger(fixtures::chat(name, "!hug"));
    }
    assert_eq!(ports(&h), ["fired", "cooling", "cooling"]);
    assert_eq!(out(&h, 1, "left"), Some(Val::Number(25.0)));

    h.advance_to(Seconds(45.0));
    h.trigger(fixtures::chat("Kiri", "!hug"));
    assert_eq!(ports(&h)[3], "cooling");
    assert_eq!(out(&h, 3, "left"), Some(Val::Number(15.0)));

    h.trigger(fixtures::chat("Mochi", "!hug"));
    assert_eq!(ports(&h)[4], "fired", "another user, past the global cooldown");
}

#[test]
fn the_broadcaster_bypasses_cooldowns_unless_told_not_to() {
    let host = || fixtures::chat_from(fixtures::user_with("Host", [Role::Broadcaster]), "!hug");
    let mut h = node(json!({ "name": "hug", "cooldown_s": 30 })).unwrap();
    h.trigger(fixtures::chat("Kiri", "!hug"));
    h.trigger(host());
    h.trigger(host());
    assert_eq!(ports(&h), ["fired", "fired", "fired"]);

    let mut h = node(json!({ "name": "hug", "cooldown_s": 30, "broadcaster_bypass": false })).unwrap();
    h.trigger(host());
    h.trigger(host());
    assert_eq!(ports(&h), ["fired", "cooling"]);
}

#[test]
fn a_denied_command_doesnt_start_a_cooldown() {
    let mut h = node(json!({ "name": "hug", "cooldown_s": 30, "min_role": "moderator" })).unwrap();
    h.trigger(fixtures::chat("Pell", "!hug"));
    h.trigger(fixtures::chat_from(fixtures::user_with("Kiri", [Role::Moderator]), "!hug"));
    assert_eq!(ports(&h), ["denied", "fired"]);
}

#[test]
fn other_platforms_are_ignored_when_listed() {
    let mut h = node(json!({ "name": "hug", "platforms": ["youtube"] })).unwrap();
    h.trigger(fixtures::chat("Kiri", "!hug"));
    assert_eq!(h.fired().len(), 0);
}

#[test]
fn check_refuses_names_with_spaces_and_negative_cooldowns() {
    let Err(errors) = node(json!({ "name": "big hug", "aliases": ["ok", ""], "cooldown_s": -1 })) else {
        panic!("took a bad command")
    };
    let mut paths: Vec<_> = errors.0.iter().map(|e| e.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, ["aliases.1", "cooldown_s", "name"]);
    assert!(node(json!({})).is_err(), "a fresh node asks for a name");
}
