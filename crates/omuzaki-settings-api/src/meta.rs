//! Static facts about a setting: scope, who may write it, whether a profile may hold it, when it applies.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Where a setting's value lives and whom it follows (D3).
///
/// Closed: adding a scope must fail every `match` that decides where a value is stored.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingScope {
    /// The person's taste: follows them to another computer (the config dir, which may roam).
    User,
    /// Bound to this computer's hardware or paths: audio devices, GPU choices, file roots, the update channel.
    Machine,
}

/// What an agent may do to this key. Every actor may read every key (scope `read`).
///
/// Closed: the op gate matches it exhaustively, so a new variant cannot be allowed by accident.
/// Rule for choosing: anything touching privacy, network, file roots, credentials, updates or capture is `Confirm`
/// or `Never`. `Free` is for taste only.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentWrite {
    /// Any agent holding `edit:settings` (harmless taste: theme, density).
    Free,
    /// The write is queued for the person (`OpCtx::require_person`): outputs, anything that can start capture or spend.
    Confirm,
    /// Never (`OpError::person_only`): privacy, consent, devices, file roots, update channel.
    Never,
}

/// Whether a profile (D7) may hold this key.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InProfile {
    /// A profile may override this key. Only valid for [`SettingScope::User`] keys.
    Member,
    /// A profile never holds this key (privacy, consent, language, startup, and every `Machine` key).
    Excluded,
}

/// When a change takes effect.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    /// Subscribers react to the event now.
    Live,
    /// Saved now; the UI says "applies after restart".
    NextStart,
}

/// Static facts about a setting. Build it with [`SettingMeta::new`]: every argument is required, so there is no
/// default for the author to forget (the three safety choices are exactly what a default would get wrong silently).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct SettingMeta {
    /// `<area>.<name>[.<name>…]`, lowercase snake segments, at least two. No key is a prefix of another.
    pub key: &'static str,
    /// English, one line (message id derived, as l10n-ops D3.1).
    pub summary: &'static str,
    /// Where the value lives.
    pub scope: SettingScope,
    /// What an agent may do.
    pub agent: AgentWrite,
    /// Whether a profile may hold it.
    pub profile: InProfile,
    /// When a change takes effect.
    pub apply: Apply,
    /// Bumped when the stored shape changes (D5). Starts at 1.
    pub version: u32,
}

impl SettingMeta {
    /// All seven facts, in the order of the fields. A `const fn`, so `Setting::META` can use it.
    #[must_use]
    pub const fn new(
        key: &'static str,
        summary: &'static str,
        scope: SettingScope,
        agent: AgentWrite,
        profile: InProfile,
        apply: Apply,
        version: u32,
    ) -> Self {
        Self { key, summary, scope, agent, profile, apply, version }
    }

    /// The first segment of the key: the Settings page section (`ui` for `ui.scale`).
    #[must_use]
    pub fn area(&self) -> &'static str {
        self.key.split('.').next().unwrap_or(self.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn area_is_the_first_segment() {
        let m = SettingMeta::new(
            "audio.bindings.voice",
            "x",
            SettingScope::Machine,
            AgentWrite::Never,
            InProfile::Excluded,
            Apply::Live,
            1,
        );
        assert_eq!(m.area(), "audio");
    }

    #[test]
    fn enums_use_snake_case_on_the_wire() {
        assert_eq!(serde_json::to_string(&AgentWrite::Confirm).ok().as_deref(), Some("\"confirm\""));
        assert_eq!(serde_json::to_string(&Apply::NextStart).ok().as_deref(), Some("\"next_start\""));
    }
}
