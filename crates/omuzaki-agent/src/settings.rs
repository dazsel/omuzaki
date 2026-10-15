//! The assistant's settings keys (D8). `llm.assistant.model` is not here: it belongs to the provider settings.
//!
//! Every key is a person's choice about money, limits or what is kept, so an agent may never write one: the model
//! cannot raise its own budget or its own caps.

use omuzaki_ops_api::Budget;
use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::setting::Setting;

use crate::config::Caps;

/// The longest a conversation may be kept: ten years.
pub const MAX_RETENTION_DAYS: u16 = 3650;
/// How long a conversation is kept after its last entry unless the person says otherwise.
pub const DEFAULT_RETENTION_DAYS: u16 = 90;

/// `assistant.caps`: the per-message limits.
pub struct AssistantCaps;

impl Setting for AssistantCaps {
    type Value = Caps;
    const META: SettingMeta = SettingMeta::new(
        "assistant.caps",
        "The most turns, tool calls, seconds and tokens the assistant uses for one message",
        SettingScope::User,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> Caps {
        Caps::default()
    }

    fn check(caps: &Caps) -> Result<(), SettingError> {
        let key = Self::META.key;
        let fields: [(&str, u64); 4] = [
            ("turns", u64::from(caps.turns)),
            ("tool_calls", u64::from(caps.tool_calls)),
            ("wall_s", u64::from(caps.wall_s)),
            ("tokens", caps.tokens),
        ];
        match fields.iter().find(|(_, value)| *value == 0) {
            Some((name, _)) => Err(SettingError::invalid(key, format!("`{name}` must be at least 1"))),
            None => Ok(()),
        }
    }
}
register_setting!(AssistantCaps);

/// `assistant.budget`: what each new assistant session opens with. Money defaults to 0 (ADR 0020): the first answer
/// with a hosted model asks the person.
pub struct AssistantBudget;

impl Setting for AssistantBudget {
    type Value = Budget;
    const META: SettingMeta = SettingMeta::new(
        "assistant.budget",
        "The budget each new assistant session opens with",
        SettingScope::User,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> Budget {
        Budget { usd_micros: 0, tokens: None, seconds: None }
    }
}
register_setting!(AssistantBudget);

/// `assistant.retention_days`: how long a conversation is kept after its last entry.
pub struct AssistantRetention;

impl Setting for AssistantRetention {
    type Value = u16;
    const META: SettingMeta = SettingMeta::new(
        "assistant.retention_days",
        "Days a conversation with the assistant is kept after its last message",
        SettingScope::User,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> u16 {
        DEFAULT_RETENTION_DAYS
    }

    fn check(days: &u16) -> Result<(), SettingError> {
        SettingError::range(Self::META.key, *days, 1, MAX_RETENTION_DAYS)
    }
}
register_setting!(AssistantRetention);
