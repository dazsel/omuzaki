//! The assistant's settings keys (#1300): registration, defaults, ranges and who may write them.

use omuzaki_agent::settings::{AssistantBudget, AssistantCaps, AssistantRetention};
use omuzaki_agent::Caps;
use omuzaki_ops_api::Budget;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingScope};
use omuzaki_settings_api::setting::Setting;

omuzaki_settings_api::check_crate!(omuzaki_agent);

#[test]
fn no_assistant_key_lets_an_agent_write_and_none_roams_into_a_profile() {
    let metas = [AssistantCaps::META, AssistantBudget::META, AssistantRetention::META];
    for meta in metas {
        assert_eq!(meta.agent, AgentWrite::Never, "{}: an agent write is PersonOnly", meta.key);
        assert_eq!(meta.profile, InProfile::Excluded, "{}", meta.key);
        assert_eq!(meta.scope, SettingScope::User, "{}", meta.key);
        assert_eq!(meta.apply, Apply::Live, "{}", meta.key);
        assert_eq!(meta.version, 1, "{}", meta.key);
    }
    let keys = metas.map(|m| m.key);
    assert_eq!(keys, ["assistant.caps", "assistant.budget", "assistant.retention_days"]);
}

#[test]
fn the_defaults_are_exactly_the_designs() {
    assert_eq!(AssistantCaps::default(), Caps { turns: 30, tool_calls: 60, wall_s: 600, tokens: 400_000 });
    assert_eq!(AssistantBudget::default(), Budget { usd_micros: 0, tokens: None, seconds: None });
    assert_eq!(AssistantRetention::default(), 90);
}

#[test]
fn the_defaults_round_trip_through_toml() {
    #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
    struct Doc {
        caps: Caps,
        budget: Budget,
        days: u16,
    }
    let doc = Doc { caps: AssistantCaps::default(), budget: AssistantBudget::default(), days: AssistantRetention::default() };
    let text = toml::to_string(&doc).unwrap();
    assert_eq!(toml::from_str::<Doc>(&text).unwrap(), doc);
}

#[test]
fn out_of_range_values_are_rejected_with_the_key_named() {
    let zero_turns = Caps { turns: 0, ..Caps::default() };
    let error = AssistantCaps::check(&zero_turns).unwrap_err().to_string();
    assert!(error.contains("assistant.caps") && error.contains("turns"), "{error}");
    for bad in [0u16, 3651] {
        let error = AssistantRetention::check(&bad).unwrap_err().to_string();
        assert!(error.contains("assistant.retention_days"), "{error}");
    }
    assert!(AssistantRetention::check(&1).is_ok() && AssistantRetention::check(&3650).is_ok());
    for each in [
        Caps { tool_calls: 0, ..Caps::default() },
        Caps { wall_s: 0, ..Caps::default() },
        Caps { tokens: 0, ..Caps::default() },
    ] {
        assert!(AssistantCaps::check(&each).is_err());
    }
}
