//! The effects' setting (particles.md D13; #814).
//!
//! | Key | Scope | Agent | Profile | Default |
//! |---|---|---|---|---|
//! | `effects.chat_emotes` | User | Confirm | Member | on |
//!
//! It lets the chat-emote pipeline fetch emote pictures from the chat platform's servers, so an agent asks first (ADR
//! 0026 decision 2: a setting that controls network fetches). The pipeline reads it on each new emote, never per frame;
//! off, nothing is fetched and every emote draws the placeholder heart.

use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::setting::Setting;

/// `effects.chat_emotes`: whether emote effects fetch and show the chat's emote pictures.
pub struct ChatEmotes;

impl Setting for ChatEmotes {
    type Value = bool;
    const META: SettingMeta = SettingMeta::new(
        "effects.chat_emotes",
        "Show chat emotes' pictures in emote effects (fetched from the chat platform's servers)",
        SettingScope::User,
        AgentWrite::Confirm,
        InProfile::Member,
        Apply::Live,
        1,
    );

    fn default() -> bool {
        true
    }
}

register_setting!(ChatEmotes);
