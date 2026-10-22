//! The `obs.*` settings (all `Machine` scope, and never written by an agent: they say where this computer's OBS is and
//! whether a link may leave the machine).
//!
//! | Key | Default | Why |
//! |---|---|---|
//! | `obs.host` | `127.0.0.1` | OBS on this computer, or another one when remote links are allowed |
//! | `obs.port` | 4455 | OBS's default WebSocket port |
//! | `obs.allow_remote` | false | a person's choice to reach OBS on another PC, unencrypted |
//! | `obs.autoconnect` | false | connect when the studio starts |

use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::setting::Setting;

use crate::limits::DEFAULT_PORT;
use crate::service::DEFAULT_HOST;

const fn machine(key: &'static str, summary: &'static str) -> SettingMeta {
    SettingMeta::new(key, summary, SettingScope::Machine, AgentWrite::Never, InProfile::Excluded, Apply::Live, 1)
}

/// `obs.host`: the machine OBS runs on.
pub struct Host;

impl Setting for Host {
    type Value = String;
    const META: SettingMeta = machine("obs.host", "The computer OBS runs on: 127.0.0.1 for this one");

    fn default() -> String {
        DEFAULT_HOST.to_owned()
    }

    fn check(value: &String) -> Result<(), SettingError> {
        if value.trim().is_empty() || value.contains(['/', ' ', '@']) {
            return Err(SettingError::invalid(Self::META.key, "a host name or an IP address, like 127.0.0.1"));
        }
        Ok(())
    }
}

register_setting!(Host);

/// `obs.port`: OBS's WebSocket server port.
pub struct Port;

impl Setting for Port {
    type Value = u16;
    const META: SettingMeta = machine("obs.port", "The port of OBS's WebSocket server (Tools, WebSocket Server Settings)");

    fn default() -> u16 {
        DEFAULT_PORT
    }

    fn check(value: &u16) -> Result<(), SettingError> {
        if *value == 0 {
            return Err(SettingError::invalid(Self::META.key, "a port from 1 to 65535"));
        }
        Ok(())
    }
}

register_setting!(Port);

/// `obs.allow_remote`: whether OBS may be on another computer.
pub struct AllowRemote;

impl Setting for AllowRemote {
    type Value = bool;
    const META: SettingMeta =
        machine("obs.allow_remote", "Allow connecting to OBS on another computer (the link is not encrypted)");

    fn default() -> bool {
        false
    }
}

register_setting!(AllowRemote);

/// `obs.autoconnect`: connect to OBS when the studio starts.
pub struct Autoconnect;

impl Setting for Autoconnect {
    type Value = bool;
    const META: SettingMeta = machine("obs.autoconnect", "Connect to OBS when the studio starts");

    fn default() -> bool {
        false
    }
}

register_setting!(Autoconnect);
