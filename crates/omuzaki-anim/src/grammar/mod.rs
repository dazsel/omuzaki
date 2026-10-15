//! The anime grammar stage (spec FE-05): the rules of the drawn-face language that no tracker or score supplies, such as
//! a blink while smiling closing into a happy `^` instead of a flat lash line.
//!
//! [`GrammarStage`] is a `Bus -> Bus` stage that runs **after the life layer** (`docs/design/anim-api.md` section 3). The
//! rules it runs are [`GrammarRule`]s, each registered where it is written with
//! [`register_grammar_rule!`](crate::register_grammar_rule), so there is no central list to edit. Which of them run for
//! a character, and how strongly, is the face profile's `grammar` section ([`GrammarConfig::from_profile`]).
//!
//! ## What the stage guarantees
//!
//! - **Order.** Enabled rules run by `(priority, id)`, so the result never depends on link order.
//! - **Off means off.** A disabled rule is not called, so the bus is bit-for-bit what it would be without it, and its
//!   state is reset, so it starts clean when switched on again.
//! - **Strength.** `0` leaves the channels a rule would write exactly as they are, `1` is the rule as designed, `2` goes
//!   twice as far (up to each channel's range). The rule states its full-strength result; [`RuleCtx::put`] applies the
//!   dial.
//! - **Humans are never overridden.** A channel in `Ownership::human_mask` is not written. Rules read whatever they
//!   need, so their memory stays true while a person drives the output.
//! - **Ownership is not relabelled.** A rule decorates what its channel's owner shows; it does not take the channel.
//! - **No time, no problem.** Rules are functions of the bus, so a zero or odd `dt` still runs them (a paused frame
//!   keeps its grammar).
//! - **Real time.** A frame allocates and locks nothing, and is deterministic (decision 0009).
//!
//! ## Rules
//!
//! | Id | Module | What it does |
//! |---|---|---|
//! | `smile-blink-arc` | [`rules`] | a blink while smiling closes into a `^` arc |
//! | `laugh-squint` | [`rules`] | a big open-mouthed smile squints the eyes, lifts the cheeks and, when huge, closes into `^^` |
//! | `pout-cheek-puff` | [`rules`] | a pout (pucker with a frown, mouth shut) puffs the cheeks and lowers the brows a little |
//! | `grin-fangs` | [`rules`] | a big smile with an open mouth shows the fangs |
//!
//! The ids FE-05 reserves are in `omuzaki_face_api::RESERVED_RULES`; a switch for a rule this build does not know has no
//! effect. A rule's own numbers (its thresholds) are the profile's `params` for it, handed over through
//! [`GrammarRule::configure`]; each rule's page names the ones it reads.

#[macro_use]
mod framework;
pub mod rules;

use std::collections::BTreeSet;

use omuzaki_anim_api::{Bus, Tick};
use omuzaki_face_api::{FaceProfile, GrammarRules, RuleId};

pub use framework::{
    __inventory, __record_registration, GrammarError, GrammarRule, GrammarRuleRegistration, MAX_DT, RuleCtx, RuleParams,
};

/// Which grammar rules run for one character and how strongly: the face profile's switches, taken out of the profile
/// so the stage does not hold on to the whole face.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GrammarConfig {
    rules: GrammarRules,
}

impl GrammarConfig {
    /// The switches of `profile`. A profile without a `grammar` section runs every known rule at its defaults.
    pub fn from_profile(profile: &FaceProfile) -> Self {
        Self::from_rules(profile.grammar.clone())
    }

    /// The given switches.
    pub fn from_rules(rules: GrammarRules) -> Self {
        Self { rules }
    }

    /// The switches.
    pub fn rules(&self) -> &GrammarRules {
        &self.rules
    }

    /// Gives `slot` the switches for its rule: whether it runs, its strength as `0..=2` (a non-finite strength reads
    /// as the default `1`) and its own numbers.
    fn apply(&self, slot: &mut Slot) {
        let config = self.rules.rule(&slot.id);
        #[allow(clippy::cast_possible_truncation)] // a value clamped to 0..=2
        let strength = if config.strength.is_finite() { config.strength.clamp(0.0, 2.0) as f32 } else { 1.0 };
        slot.enabled = config.enabled;
        slot.strength = strength;
        slot.rule.configure(RuleParams::new(&config.params));
    }
}

/// One rule of a stage with its current settings.
#[derive(Debug)]
struct Slot {
    id: RuleId,
    rule: Box<dyn GrammarRule>,
    enabled: bool,
    strength: f32,
    /// True once the rule has stepped since it was last reset, so a rule that was never on is not reset again.
    dirty: bool,
}

/// The grammar stage of one character. Build it once, call [`step`](GrammarStage::step) every frame after the life
/// layer.
#[derive(Debug)]
pub struct GrammarStage {
    /// In run order.
    slots: Vec<Slot>,
}

impl GrammarStage {
    /// A stage with every registered rule, set as `config` says.
    ///
    /// # Errors
    ///
    /// [`GrammarError`] if a registered rule has an invalid or duplicate id.
    pub fn new(config: &GrammarConfig) -> Result<Self, GrammarError> {
        Self::with_rules(
            inventory::iter::<GrammarRuleRegistration>.into_iter().map(GrammarRuleRegistration::instantiate),
            config,
        )
    }

    /// A stage with exactly `rules` (tests and tools that bring their own), set as `config` says.
    ///
    /// # Errors
    ///
    /// [`GrammarError`] if a rule has an invalid or duplicate id.
    pub fn with_rules(
        rules: impl IntoIterator<Item = Box<dyn GrammarRule>>,
        config: &GrammarConfig,
    ) -> Result<Self, GrammarError> {
        let mut seen = BTreeSet::new();
        let mut slots = Vec::new();
        for rule in rules {
            let id = RuleId::new(rule.id()).map_err(|source| GrammarError::BadId { id: rule.id(), source })?;
            if !seen.insert(id.clone()) {
                return Err(GrammarError::Duplicate(id.to_string()));
            }
            let mut slot = Slot { id, rule, enabled: true, strength: 1.0, dirty: false };
            config.apply(&mut slot);
            slots.push(slot);
        }
        slots.sort_by(|a, b| a.rule.priority().cmp(&b.rule.priority()).then_with(|| a.id.cmp(&b.id)));
        Ok(Self { slots })
    }

    /// The ids of the rules this stage holds, in the order they run (enabled or not).
    pub fn rule_ids(&self) -> impl Iterator<Item = &str> {
        self.slots.iter().map(|slot| slot.id.as_str())
    }

    /// Applies new switches and numbers. Rules that are now off are reset on the next [`step`](GrammarStage::step); the
    /// rest keep their state, so a variant that changes one strength or threshold does not restart the others.
    pub fn set_config(&mut self, config: &GrammarConfig) {
        for slot in &mut self.slots {
            config.apply(slot);
        }
    }

    /// Forgets every rule's state.
    pub fn reset(&mut self) {
        for slot in &mut self.slots {
            slot.rule.reset();
            slot.dirty = false;
        }
    }

    /// Runs the enabled rules, in order, over `bus`. Hot path: no allocation, no locking.
    pub fn step(&mut self, tick: Tick, bus: &mut Bus) {
        // Rules never change who owns a channel, so this holds for the whole frame.
        let taken = bus.owners.human_mask();
        for slot in &mut self.slots {
            if !slot.enabled {
                if slot.dirty {
                    slot.rule.reset();
                    slot.dirty = false;
                }
                continue;
            }
            slot.dirty = true;
            slot.rule.step(&mut RuleCtx::new(&mut bus.values, taken, slot.strength, tick));
        }
    }
}
