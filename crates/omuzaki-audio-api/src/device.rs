//! Devices, roles and bindings (design doc section 2).
//!
//! A project names **roles** ([`DeviceRole`]), never devices. A person-level machine profile ([`ProfileStore`]) binds
//! each role to a [`Binding`]. A device is always identified by its stable [`DeviceId`], never by an index, because
//! Windows reorders devices.

use std::collections::BTreeMap;

use omuzaki_core::time::Seconds;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::DeviceRole;

/// A device's stable id: cpal's `(HostId, id)`, as strings. Never an index.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct DeviceId {
    /// The audio host (`wasapi`, `alsa`, `coreaudio`, ...).
    pub host: String,
    /// The host's own stable id: the `WASAPI` endpoint id, the `CoreAudio` device UID, the `ALSA` or `PipeWire` name.
    pub id: String,
}

/// Which way a device's audio flows.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    /// A capture device (microphone, line in, a virtual cable's output side).
    Input,
    /// A playback device (speakers, headphones, a virtual cable's input side).
    Output,
}

/// Which device channels feed (or are fed by) a graph source or output, zero-based, in graph-channel order.
///
/// `[1]` is the second channel of a USB interface as a mono source; `[0, 1]` a stereo pair.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ChannelMap(pub Vec<u16>);

impl ChannelMap {
    /// One device channel as a mono source.
    #[must_use]
    pub fn mono(channel: u16) -> Self {
        Self(vec![channel])
    }

    /// Two device channels as a stereo pair, left then right.
    #[must_use]
    pub fn stereo(left: u16, right: u16) -> Self {
        Self(vec![left, right])
    }

    /// The device channels, one per graph channel.
    #[must_use]
    pub fn channels(&self) -> &[u16] {
        &self.0
    }
}

impl Default for ChannelMap {
    /// The first device channel, as mono.
    fn default() -> Self {
        Self::mono(0)
    }
}

/// How a role is bound in the machine profile.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Binding {
    /// A specific device. The default. It never falls back to another device, the OS default included.
    Fixed {
        /// The device.
        id: DeviceId,
        /// The device's name when it was bound, to show if the device is absent.
        label: String,
        /// Which device channels the role uses.
        channels: ChannelMap,
        /// For an output: the person says it plays through speakers, so `AecMode::Auto` turns the echo canceller on.
        /// Set by the person, default false; ignored on inputs.
        #[serde(default)]
        speakers: bool,
    },
    /// An explicit opt-in to follow whatever the OS default is.
    SystemDefault {
        /// Which device channels the role uses.
        channels: ChannelMap,
        /// As for `Fixed`: an output that plays through speakers.
        #[serde(default)]
        speakers: bool,
    },
}

impl Binding {
    /// The device channels this binding uses.
    #[must_use]
    pub fn channels(&self) -> &ChannelMap {
        match self {
            Self::Fixed { channels, .. } | Self::SystemDefault { channels, .. } => channels,
        }
    }

    /// Whether the person says this output plays through speakers.
    #[must_use]
    pub fn speakers(&self) -> bool {
        match self {
            Self::Fixed { speakers, .. } | Self::SystemDefault { speakers, .. } => *speakers,
        }
    }
}

/// What the backend knows about a device it enumerated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeviceInfo {
    /// The stable id.
    pub id: DeviceId,
    /// The name to show.
    pub label: String,
    /// Capture or playback.
    pub direction: Direction,
    /// Whether the OS names it the default for its direction.
    pub is_default: bool,
    /// Sample rates the device supports, in Hz.
    pub rates: Vec<u32>,
    /// Channel count.
    pub channels: u16,
    /// The latency the driver reports, if it does.
    pub latency_hint: Option<Seconds>,
}

/// Where a binding is in its life (design doc section 2.3).
///
/// ```text
/// Unbound -bind-> Opening -ok-> Running -error / vanished-> Lost -same id reappears-> Opening
///                    \-fail-> Failed(reason)   (retried on the next watch tick, backing off to 5 s)
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DeviceState {
    /// The role has no binding.
    Unbound,
    /// The stream is being opened.
    Opening,
    /// The stream is running.
    Running,
    /// The device vanished. Inputs feed silence and flag a gap; outputs discard; the graph keeps running.
    Lost,
    /// Opening failed.
    Failed {
        /// Why, in words a person can read.
        reason: String,
    },
}

/// Why a profile operation failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ProfileError {
    /// The store could not be read or written.
    #[error("the machine profile is unavailable: {0}")]
    Unavailable(String),
}

/// The person-level machine profile: which device serves each role on this computer. It lives outside the project, in
/// the same store as ML-13's per-hash acknowledgements, so a project opened elsewhere asks for its devices again.
///
/// This trait stands in until that store lands. Binding is a person's act: callers check the actor first
/// (design doc section 7.2).
pub trait ProfileStore: Send + Sync {
    /// Every binding in the profile.
    ///
    /// # Errors
    /// [`ProfileError::Unavailable`] when the profile cannot be read.
    fn bindings(&self) -> Result<BTreeMap<DeviceRole, Binding>, ProfileError>;

    /// Binds `role`, replacing any earlier binding.
    ///
    /// # Errors
    /// [`ProfileError::Unavailable`] when the profile cannot be written.
    fn bind(&self, role: &DeviceRole, binding: Binding) -> Result<(), ProfileError>;

    /// Removes the binding of `role`. Unbinding an unbound role succeeds.
    ///
    /// # Errors
    /// [`ProfileError::Unavailable`] when the profile cannot be written.
    fn unbind(&self, role: &DeviceRole) -> Result<(), ProfileError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_binding_round_trips_through_toml() {
        let fixed = Binding::Fixed {
            id: DeviceId { host: "wasapi".into(), id: "{0.0.1.00000000}.{abc}".into() },
            label: "USB Mic".into(),
            channels: ChannelMap::mono(1),
            speakers: true,
        };
        let default = Binding::SystemDefault { channels: ChannelMap::stereo(0, 1), speakers: false };
        for b in [fixed, default] {
            let text = toml::to_string(&b).unwrap_or_default();
            assert_eq!(toml::from_str::<Binding>(&text).ok(), Some(b), "{text}");
        }
    }

    #[test]
    fn device_states_serialize_with_a_tag() {
        let text = serde_json::to_string(&DeviceState::Failed { reason: "busy".into() }).unwrap_or_default();
        assert_eq!(text, r#"{"state":"failed","reason":"busy"}"#);
    }
}
