//! A variant switches anime grammar rules by path (FE-05, G7): `face/grammar/<rule>/enabled` on a face that has no entry
//! for that rule creates it, the rule's other fields keep their defaults, and the variant chain applies as it does for
//! every other override.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{CharacterDef, VariantError, VariantId, VariantSet, resolve_variant};
use omuzaki_face_api::{RuleConfig, RuleId, Validate};
use serde_json::json;

fn base() -> CharacterDef {
    serde_json::from_value(json!({ "name": "Mira", "template": "soft-sd" })).unwrap()
}

fn variants(value: serde_json::Value) -> VariantSet {
    serde_json::from_value(value).unwrap()
}

fn vid(s: &str) -> VariantId {
    VariantId::new(s).unwrap()
}

fn rule(id: &str) -> RuleId {
    RuleId::new(id).unwrap()
}

#[test]
fn a_variant_switches_one_rule_and_the_rest_keep_their_defaults() {
    let vs = variants(json!({
        "game": {
            "description": "no laugh squint",
            "overrides": { "face/grammar/laugh-squint/enabled": false }
        }
    }));
    let out = resolve_variant(&base(), &vs, &vid("game")).unwrap();
    assert_eq!(out.face.grammar.rule(&rule("laugh-squint")), &RuleConfig { enabled: false, ..RuleConfig::default() });
    assert_eq!(out.face.grammar.rule(&rule("grin-fangs")), &RuleConfig::default(), "other rules are unchanged");
    out.face.validate().unwrap();
    assert!(base().face.grammar.0.is_empty(), "the base character is not touched");
}

#[test]
fn a_child_variant_wins_over_the_variant_it_extends() {
    let vs = variants(json!({
        "game": {
            "overrides": {
                "face/grammar/laugh-squint/enabled": false,
                "face/grammar/smile-blink-arc/strength": 0.5
            }
        },
        "close-up": {
            "extends": "game",
            "overrides": {
                "face/grammar/laugh-squint/enabled": true,
                "face/grammar/grin-fangs/params/threshold": 0.4
            }
        }
    }));
    let out = resolve_variant(&base(), &vs, &vid("close-up")).unwrap();
    let laugh = out.face.grammar.rule(&rule("laugh-squint"));
    assert!(laugh.enabled, "the child re-enables the rule the parent switched off");
    let smile = out.face.grammar.rule(&rule("smile-blink-arc"));
    assert_eq!((smile.enabled, smile.strength), (true, 0.5), "the parent's strength survives the child");
    let fangs = out.face.grammar.rule(&rule("grin-fangs"));
    assert_eq!(fangs.params.get(&omuzaki_core::id::Slug::new("threshold").unwrap()), Some(&0.4));
    out.face.validate().unwrap();
}

#[test]
fn a_mistyped_rule_override_names_the_variant_and_the_override() {
    let vs = variants(json!({
        "typo": { "overrides": { "face/grammar/laugh-squint/enabeld": false } },
        "wrong-type": { "overrides": { "face/grammar/laugh-squint/strength": "high" } }
    }));
    for (v, needle) in [("typo", "enabeld"), ("wrong-type", "invalid type")] {
        let e = resolve_variant(&base(), &vs, &vid(v)).unwrap_err();
        assert!(matches!(e, VariantError::Override { .. }), "{e}");
        let text = e.to_string();
        assert!(text.contains(&format!("variant '{v}'")) && text.contains(needle), "{text}");
    }
}
