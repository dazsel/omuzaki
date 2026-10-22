//! The replay buffer's settings (`docs/design/record.md` D4, D10).
//!
//! | Key | Scope | Agent | Why |
//! |---|---|---|---|
//! | `replay.enabled` | User | Confirm | turning it on starts a continuous capture of the person |
//! | `replay.length_s` | User | Free | taste |
//! | `replay.max_mb` | Machine | Never | this computer's memory or disk |
//! | `replay.backing` | Machine | Never | this computer's memory or disk |
//! | `replay.dir` | Machine | Never | a file root on this computer |

use std::path::PathBuf;

use omuzaki_core::time::Seconds;
use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::setting::Setting;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ring::{Caps, MAX_SPAN, MIN_SPAN, RingError};

/// The folder under the recordings folder that saved clips go into when `replay.dir` is unset.
pub const DEFAULT_SUBDIR: &str = "Replays";
/// The smallest cap a person may set, in MiB.
pub const MIN_MB: u64 = 64;
/// The default cap, in MiB: at the default codec it holds about 260 s, so the default length never reaches it.
pub const DEFAULT_MB: u64 = 1024;
/// The default length.
pub const DEFAULT_LENGTH: Seconds = Seconds(60.0);

/// Where the buffer keeps its chunks.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Backing {
    /// In memory (the default).
    #[default]
    Memory,
    /// In segment files in a cache folder, wiped when the buffer starts.
    Disk,
}

/// The replay buffer's limits, read from the settings when it starts.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ReplaySettings {
    /// How many seconds a save holds by default: 5 to 300.
    pub length: Seconds,
    /// The most bytes kept alive, at least 64 MiB.
    pub max_bytes: u64,
    /// Memory or disk.
    pub backing: Backing,
}

impl Default for ReplaySettings {
    fn default() -> Self {
        Self { length: DEFAULT_LENGTH, max_bytes: DEFAULT_MB << 20, backing: Backing::Memory }
    }
}

impl ReplaySettings {
    /// Settings with these values.
    pub fn new(length: Seconds, max_bytes: u64, backing: Backing) -> Self {
        Self { length, max_bytes, backing }
    }

    /// The ring's caps. The buffer keeps the asked length plus one keyframe interval, so a clip of the full length
    /// still has a keyframe at or before its start.
    pub fn caps(&self) -> Caps {
        Caps { max_span: Seconds((self.length.0 + 1.0).min(MAX_SPAN.0)), max_bytes: self.max_bytes }
    }

    /// Checks the length and the cap.
    ///
    /// # Errors
    /// [`RingError`] for a length outside 5 s to 300 s or a cap under 64 MiB.
    pub fn check(&self) -> Result<(), RingError> {
        if !(MIN_SPAN.0..=MAX_SPAN.0).contains(&self.length.0) {
            return Err(RingError::Span(self.length.0));
        }
        if self.max_bytes < MIN_MB << 20 {
            return Err(RingError::Bytes(self.max_bytes));
        }
        Ok(())
    }

    /// How many seconds the buffer holds at `bytes_per_second`: the length, or less when the cap binds.
    #[allow(clippy::cast_precision_loss)] // Bytes and rates far under 2^52.
    pub fn effective_span(&self, bytes_per_second: u64) -> Seconds {
        if bytes_per_second == 0 {
            return self.length;
        }
        Seconds(self.length.0.min(self.max_bytes as f64 / bytes_per_second as f64))
    }
}

/// `replay.enabled`: whether the replay buffer runs. Turning it on starts a continuous capture, so an agent asks first.
pub struct Enabled;

impl Setting for Enabled {
    type Value = bool;
    const META: SettingMeta = SettingMeta::new(
        "replay.enabled",
        "Keep the last seconds of the program ready to save as a clip",
        SettingScope::User,
        AgentWrite::Confirm,
        InProfile::Member,
        Apply::Live,
        1,
    );

    fn default() -> bool {
        false
    }
}

register_setting!(Enabled);

/// `replay.length_s`: how many seconds a saved clip holds by default.
pub struct LengthS;

impl Setting for LengthS {
    type Value = f64;
    const META: SettingMeta = SettingMeta::new(
        "replay.length_s",
        "How many seconds a saved replay clip holds, 5 to 300",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::Live,
        1,
    );

    fn default() -> f64 {
        DEFAULT_LENGTH.0
    }

    fn check(value: &f64) -> Result<(), SettingError> {
        if (MIN_SPAN.0..=MAX_SPAN.0).contains(value) {
            Ok(())
        } else {
            Err(SettingError::invalid(Self::META.key, "the replay length must be 5 to 300 seconds"))
        }
    }
}

register_setting!(LengthS);

/// `replay.max_mb`: the most memory or disk the buffer keeps alive, in MiB.
pub struct MaxMb;

impl Setting for MaxMb {
    type Value = u64;
    const META: SettingMeta = SettingMeta::new(
        "replay.max_mb",
        "The most memory or disk the replay buffer uses, in MiB (at least 64)",
        SettingScope::Machine,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> u64 {
        DEFAULT_MB
    }

    fn check(value: &u64) -> Result<(), SettingError> {
        if *value >= MIN_MB && *value <= 1 << 20 {
            Ok(())
        } else {
            Err(SettingError::invalid(Self::META.key, "the replay cap must be at least 64 MiB"))
        }
    }
}

register_setting!(MaxMb);

/// `replay.backing`: memory or disk.
pub struct BackingSetting;

impl Setting for BackingSetting {
    type Value = Backing;
    const META: SettingMeta = SettingMeta::new(
        "replay.backing",
        "Where the replay buffer keeps its last seconds: memory or disk",
        SettingScope::Machine,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> Backing {
        Backing::Memory
    }
}

register_setting!(BackingSetting);

/// `replay.dir`: the folder saved clips go into. Empty (the default) means `Replays` in the recordings folder.
pub struct Dir;

impl Setting for Dir {
    type Value = String;
    const META: SettingMeta = SettingMeta::new(
        "replay.dir",
        "The folder saved replay clips go into; unset means Replays in the recordings folder",
        SettingScope::Machine,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> String {
        String::new()
    }

    fn check(value: &String) -> Result<(), SettingError> {
        if value.is_empty() || std::path::Path::new(value).is_absolute() {
            Ok(())
        } else {
            Err(SettingError::invalid(Self::META.key, "the folder must be an absolute path, or empty for the default"))
        }
    }
}

register_setting!(Dir);

/// The folder clips go into: `replay.dir` when set, else `Replays` in the recordings folder.
pub fn resolve_dir(setting: &str, record_dir: &str) -> PathBuf {
    if setting.is_empty() {
        crate::record::settings::resolve_dir(record_dir).join(DEFAULT_SUBDIR)
    } else {
        PathBuf::from(setting)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_pass_and_the_ranges_hold() {
        assert!(ReplaySettings::default().check().is_ok());
        for (length, ok) in [(4.0, false), (5.0, true), (300.0, true), (301.0, false)] {
            let s = ReplaySettings { length: Seconds(length), ..ReplaySettings::default() };
            assert_eq!(s.check().is_ok(), ok, "{length}");
            assert_eq!(LengthS::check(&length).is_ok(), ok, "{length}");
        }
        assert!(ReplaySettings { max_bytes: (64 << 20) - 1, ..ReplaySettings::default() }.check().is_err());
        assert!(MaxMb::check(&63).is_err() && MaxMb::check(&64).is_ok());
        assert!(Dir::check(&"relative".into()).is_err());
    }

    #[test]
    fn the_span_is_the_length_unless_the_cap_binds() {
        let s = ReplaySettings::default();
        assert!((s.effective_span(33_000_000).0 - 32.537).abs() < 0.01);
        assert!((s.effective_span(2_500_000).0 - 60.0).abs() < f64::EPSILON);
        assert_eq!(s.caps().max_span, Seconds(61.0));
    }
}
