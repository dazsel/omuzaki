//! Anime grammar rule switches (spec FE-05, decision G7): which of the anime grammar rules run for one character, and
//! how strongly. A smile that blinks into `^^`, a laugh that squints, a pout that puffs the cheeks: the rules
//! themselves are a `Bus -> Bus` stage that `omuzaki-anim` owns, after the life layer. This module only stores the
//! switches, so a character's face file says which rules are on and a variant can turn one off by path.
//!
//! The set of rules is open. A rule registers itself where it is implemented, so there is no central enum here. A
//! switch set may name a rule this build doesn't know (a project saved by a newer build): it is kept on round trip and
//! simply has no effect. An absent rule runs with [`RuleConfig::default`]: on, at full strength, no params.

use std::collections::BTreeMap;

use omuzaki_core::id::Slug;
use omuzaki_core::slug_id;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::check::{Check, Validate};

slug_id!(
    /// A grammar rule, by its slug (`laugh-squint`, `grin-fangs`...). Open: any rule may register one.
    RuleId
);

/// The four rules FE-05 names. The grammar stage in `omuzaki-anim` implements them under these ids, so every build reads
/// them the same way. Other rules are open and need no entry here.
pub const RESERVED_RULES: [&str; 4] = ["smile-blink-arc", "laugh-squint", "pout-cheek-puff", "grin-fangs"];

/// What an absent rule runs with. A static, so [`GrammarRules::rule`] can hand out a reference without allocating.
static DEFAULT_RULE: RuleConfig = RuleConfig { enabled: true, strength: 1.0, params: BTreeMap::new() };

/// How one rule is set for one character. Every field has a default, so a file may list only what differs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RuleConfig {
    /// Whether the rule runs at all. Off leaves the channels it would touch as they are.
    pub enabled: bool,
    /// How strongly it acts: 0 none, 1 as designed, 2 twice as much.
    #[schemars(range(min = 0.0, max = 2.0))]
    pub strength: f64,
    /// Rule-specific numbers by name (a threshold, say). Each rule documents its own; a name no rule reads is kept.
    pub params: BTreeMap<Slug, f64>,
}

impl Default for RuleConfig {
    fn default() -> Self {
        DEFAULT_RULE.clone()
    }
}

impl Validate for RuleConfig {
    fn check(&self, c: &mut Check) {
        c.range("strength", self.strength, 0.0, 2.0);
        for (name, value) in &self.params {
            if !value.is_finite() {
                c.fail(&format!("params/{name}"), format!("{value} is not a finite number"));
            }
        }
    }
}

/// One character's rule switches: a sparse map from rule id to its settings. Stored as a map in `face.toml`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct GrammarRules(pub BTreeMap<RuleId, RuleConfig>);

impl GrammarRules {
    /// The settings of rule `id`: the character's entry if it has one, otherwise the default.
    pub fn rule(&self, id: &RuleId) -> &RuleConfig {
        self.0.get(id).unwrap_or(&DEFAULT_RULE)
    }
}

impl Validate for GrammarRules {
    fn check(&self, c: &mut Check) {
        for (id, rule) in &self.0 {
            c.nested(id.as_str(), rule);
        }
    }
}
