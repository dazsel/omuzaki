//! The `input.phone` setting: where the phone is and which ports the receivers listen on.

use std::net::IpAddr;

use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::setting::Setting;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::protocol::Protocol;

/// The phone link, as the Machine setting `input.phone`. A device binding, like `input.camera`: only the person sets
/// it (it opens a port on the local network and names a machine on it), and it never roams.
pub struct InputPhone;

/// The value of [`InputPhone`]. A table whose fields all have defaults, because the settings files are TOML and TOML
/// has no null: an unset phone address is an absent key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct PhoneSettings {
    /// The phone's address on the local network. iFacialMocap and VTube Studio need it (this computer asks the phone
    /// for data); for every protocol, once it is set, datagrams from any other address are ignored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<IpAddr>,
    /// The port this computer listens on for iFacialMocap.
    #[serde(default = "ifacialmocap_port")]
    pub ifacialmocap_port: u16,
    /// The port this computer listens on for VTube Studio's iPhone link.
    #[serde(default = "vtube_studio_port")]
    pub vtube_studio_port: u16,
    /// The port this computer listens on for Live Link Face.
    #[serde(default = "live_link_port")]
    pub live_link_face_port: u16,
    /// The port this computer listens on for VMC.
    #[serde(default = "vmc_port")]
    pub vmc_port: u16,
}

const fn ifacialmocap_port() -> u16 {
    Protocol::Ifacialmocap.default_port()
}
const fn vtube_studio_port() -> u16 {
    Protocol::VtubeStudio.default_port()
}
const fn live_link_port() -> u16 {
    Protocol::LiveLinkFace.default_port()
}
const fn vmc_port() -> u16 {
    Protocol::Vmc.default_port()
}

impl Default for PhoneSettings {
    fn default() -> Self {
        Self {
            address: None,
            ifacialmocap_port: ifacialmocap_port(),
            vtube_studio_port: vtube_studio_port(),
            live_link_face_port: live_link_port(),
            vmc_port: vmc_port(),
        }
    }
}

impl PhoneSettings {
    /// With the phone at `address`.
    #[must_use]
    pub fn with_address(mut self, address: Option<IpAddr>) -> Self {
        self.address = address;
        self
    }

    /// With `protocol` listening on `port` (0 lets the OS pick one, for tests).
    #[must_use]
    pub fn with_port(mut self, protocol: Protocol, port: u16) -> Self {
        *self.port_mut(protocol) = port;
        self
    }

    /// The port `protocol` listens on.
    pub fn port(&self, protocol: Protocol) -> u16 {
        match protocol {
            Protocol::Ifacialmocap => self.ifacialmocap_port,
            Protocol::VtubeStudio => self.vtube_studio_port,
            Protocol::LiveLinkFace => self.live_link_face_port,
            Protocol::Vmc => self.vmc_port,
        }
    }

    fn port_mut(&mut self, protocol: Protocol) -> &mut u16 {
        match protocol {
            Protocol::Ifacialmocap => &mut self.ifacialmocap_port,
            Protocol::VtubeStudio => &mut self.vtube_studio_port,
            Protocol::LiveLinkFace => &mut self.live_link_face_port,
            Protocol::Vmc => &mut self.vmc_port,
        }
    }
}

impl Setting for InputPhone {
    type Value = PhoneSettings;
    const META: SettingMeta = SettingMeta::new(
        "input.phone",
        "The phone or mocap app that drives the face over the network: its address and the ports to listen on",
        SettingScope::Machine,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> PhoneSettings {
        PhoneSettings::default()
    }
}

register_setting!(InputPhone);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_table_reads_as_the_defaults_and_ports_round_trip() {
        let empty: PhoneSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, PhoneSettings::default());
        assert_eq!(empty.port(Protocol::Vmc), 39539);
        let set = PhoneSettings::default().with_address(Some([192, 168, 1, 20].into())).with_port(Protocol::Vmc, 40000);
        let json = serde_json::to_value(&set).unwrap();
        assert_eq!(json["address"], "192.168.1.20");
        assert_eq!(serde_json::from_value::<PhoneSettings>(json).unwrap(), set);
        assert!(serde_json::to_value(PhoneSettings::default()).unwrap().get("address").is_none());
    }
}
