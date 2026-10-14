//! The anime grammar switches (FE-05, G7): defaults, the file format, validation, the schema, and the design-doc row.
//! The variant override through `resolve_variant` is in `omuzaki-char-api/tests/grammar_override.rs`, because that
//! function lives in the character crate.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::id::Slug;
use omuzaki_face_api::{FaceProfile, GrammarRules, RESERVED_RULES, RuleConfig, RuleId, Validate};
use serde_json::json;

const DOC: &str = include_str!("../../../docs/design/char-face-api.md");

fn rule(id: &str) -> RuleId {
    RuleId::new(id).unwrap()
}

fn slug(s: &str) -> Slug {
    Slug::new(s).unwrap()
}

#[test]
fn absent_rules_run_at_their_defaults() {
    assert_eq!(
        RuleConfig::default(),
        RuleConfig { enabled: true, strength: 1.0, params: std::collections::BTreeMap::new() }
    );
    let rules = GrammarRules::default();
    assert_eq!(rules.rule(&rule("laugh-squint")), &RuleConfig::default());
    assert!(FaceProfile::default().grammar.0.is_empty(), "a new face switches no rule");
    assert_eq!(serde_json::to_value(FaceProfile::default()).unwrap()["grammar"], json!({}));
}

#[test]
fn a_rule_that_lists_only_what_differs_fills_in_the_rest() {
    let rules: GrammarRules = serde_json::from_str(r#"{"laugh-squint":{"enabled":false}}"#).unwrap();
    let cfg = rules.rule(&rule("laugh-squint"));
    assert_eq!((cfg.enabled, cfg.strength), (false, 1.0));
    assert!(cfg.params.is_empty());
    let typo = serde_json::from_str::<GrammarRules>(r#"{"laugh-squint":{"enabled":false,"stength":1}}"#);
    assert!(typo.unwrap_err().to_string().contains("stength"), "typos fail loudly");
}

#[test]
fn a_switch_set_survives_a_json_round_trip_and_keeps_unknown_rules() {
    let mut rules = GrammarRules::default();
    rules.0.insert(rule("laugh-squint"), RuleConfig { enabled: false, ..RuleConfig::default() });
    rules.0.insert(
        rule("from-a-newer-build"),
        RuleConfig { strength: 0.5, params: [(slug("threshold"), 0.25)].into(), ..RuleConfig::default() },
    );
    let text = serde_json::to_string(&rules).unwrap();
    assert!(text.contains("from-a-newer-build"), "an unknown rule is kept, not dropped: {text}");
    let back: GrammarRules = serde_json::from_str(&text).unwrap();
    assert_eq!(back, rules);

    let p = FaceProfile { grammar: rules, ..FaceProfile::default() };
    let back: FaceProfile = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
    assert_eq!(back, p);
}

#[test]
fn an_old_face_without_the_section_parses_with_no_switches() {
    let old: FaceProfile = serde_json::from_str(r#"{"eyes":{"iris":{"width":0.7}},"expressions":{}}"#).unwrap();
    assert_eq!(old.grammar, GrammarRules::default());
    old.validate().unwrap();
}

#[test]
fn strength_runs_from_none_to_double_and_params_must_be_finite() {
    for strength in [0.0, 1.0, 2.0] {
        let mut rules = GrammarRules::default();
        rules.0.insert(rule("smile-blink-arc"), RuleConfig { strength, ..RuleConfig::default() });
        assert!(rules.validate().is_ok(), "strength {strength} is allowed");
    }
    for strength in [-0.1, 2.01, f64::NAN] {
        let mut rules = GrammarRules::default();
        rules.0.insert(rule("smile-blink-arc"), RuleConfig { strength, ..RuleConfig::default() });
        assert!(rules.validate().is_err(), "strength {strength} is refused");
    }
    let mut rules = GrammarRules::default();
    rules.0.insert(rule("from-a-newer-build"), RuleConfig::default());
    assert!(rules.validate().is_ok(), "a rule this build doesn't know is allowed");
}

#[test]
fn validation_names_the_rule_and_the_field() {
    let mut p = FaceProfile::default();
    p.grammar.0.insert(rule("laugh-squint"), RuleConfig { strength: 2.5, ..RuleConfig::default() });
    p.grammar.0.insert(
        rule("grin-fangs"),
        RuleConfig { params: [(slug("threshold"), f64::NAN)].into(), ..RuleConfig::default() },
    );
    p.grammar.0.insert(rule("pout-cheek-puff"), RuleConfig { strength: f64::INFINITY, ..RuleConfig::default() });
    let e = p.validate().unwrap_err();
    let paths: Vec<_> = e.0.iter().map(|x| x.path.as_str()).collect();
    assert_eq!(
        paths,
        ["grammar/grin-fangs/params/threshold", "grammar/laugh-squint/strength", "grammar/pout-cheek-puff/strength"]
    );
}

#[test]
fn the_rule_config_schema_matches_the_snapshot() {
    // The agent-facing schema: `strength` carries its 0..=2 bound, and `params` only takes slug names.
    let schema = serde_json::to_value(schemars::schema_for!(RuleConfig)).unwrap();
    let expected = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": "RuleConfig",
        "description": "How one rule is set for one character. Every field has a default, so a file may list only what differs.",
        "type": "object",
        "properties": {
            "enabled": {
                "description": "Whether the rule runs at all. Off leaves the channels it would touch as they are.",
                "default": true,
                "type": "boolean"
            },
            "strength": {
                "description": "How strongly it acts: 0 none, 1 as designed, 2 twice as much.",
                "default": 1.0,
                "format": "double",
                "maximum": 2.0,
                "minimum": 0.0,
                "type": "number"
            },
            "params": {
                "description": "Rule-specific numbers by name (a threshold, say). Each rule documents its own; a name no rule reads is kept.",
                "default": {},
                "type": "object",
                "patternProperties": {
                    "^[a-z0-9][a-z0-9_-]{0,63}$": { "format": "double", "type": "number" }
                },
                "additionalProperties": false
            }
        },
        "additionalProperties": false
    });
    assert_eq!(schema, expected, "the schema changed: update the snapshot if the change is intended");
}

#[test]
fn the_reserved_rules_are_slugs_and_the_design_doc_names_them() {
    assert_eq!(RESERVED_RULES, ["smile-blink-arc", "laugh-squint", "pout-cheek-puff", "grin-fangs"]);
    assert!(DOC.contains("| `grammar` |"), "the design doc needs a row for the grammar section");
    for id in RESERVED_RULES {
        RuleId::new(id).expect("reserved rules are slugs");
        assert!(DOC.contains(id), "the design doc doesn't name the reserved rule `{id}`");
    }
}
