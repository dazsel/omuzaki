//! The recorder's settings (`docs/design/record.md` D10): where takes go and in which format.
//!
//! | Key | Scope | Agent | Why |
//! |---|---|---|---|
//! | `record.format` | User | Free | taste and quality |
//! | `record.dir` | Machine | Never | a file root on this computer |

use std::path::PathBuf;

use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::setting::Setting;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The folder under the person's videos folder that takes go into when `record.dir` is unset.
pub const DEFAULT_SUBDIR: &str = "Omuzaki";

/// The file format of a take.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RecordFormat {
    /// The best format this machine can write: `WebM` VP9 with alpha when its encoder is present, else a PNG sequence.
    #[default]
    Auto,
    /// One PNG per frame with alpha, in a folder, plus WAV tracks: the safe fallback every editor reads.
    PngSequence,
    /// `WebM` with VP9 video and its alpha plane, plus WAV tracks: the default record format (D1).
    WebmVp9Alpha,
}

/// `record.format`: the format new takes are written in.
pub struct Format;

impl Setting for Format {
    type Value = RecordFormat;
    const META: SettingMeta = SettingMeta::new(
        "record.format",
        "The file format of new recordings: auto, png_sequence or webm_vp9_alpha",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::Live,
        1,
    );

    fn default() -> RecordFormat {
        RecordFormat::Auto
    }
}

register_setting!(Format);

/// `record.dir`: the folder takes are written into. Empty (the default) means `<videos>/Omuzaki`.
pub struct Dir;

impl Setting for Dir {
    type Value = String;
    const META: SettingMeta = SettingMeta::new(
        "record.dir",
        "The folder recordings are written into; unset means the Omuzaki folder in your videos folder",
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

/// The folder takes go into: `record.dir` when not empty, else `<videos>/Omuzaki`, else `<home>/Omuzaki`, else
/// `Omuzaki` in the current folder (a machine with no home folder at all).
pub fn resolve_dir(setting: &str) -> PathBuf {
    if !setting.is_empty() {
        return PathBuf::from(setting);
    }
    let dirs = directories::UserDirs::new();
    let base = dirs
        .as_ref()
        .and_then(|d| d.video_dir().map(std::path::Path::to_owned))
        .or_else(|| dirs.as_ref().map(|d| d.home_dir().to_owned()))
        .unwrap_or_default();
    base.join(DEFAULT_SUBDIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_use_the_design_names() -> Result<(), serde_json::Error> {
        assert_eq!(serde_json::to_string(&RecordFormat::WebmVp9Alpha)?, "\"webm_vp9_alpha\"");
        assert_eq!(serde_json::to_string(&RecordFormat::PngSequence)?, "\"png_sequence\"");
        assert_eq!(serde_json::to_string(&RecordFormat::Auto)?, "\"auto\"");
        Ok(())
    }

    #[test]
    fn the_folder_must_be_absolute() {
        assert!(Dir::check(&String::new()).is_ok());
        assert!(Dir::check(&"relative/takes".into()).is_err());
        assert!(Dir::check(&"  ".into()).is_err());
        let abs = std::env::temp_dir().join("takes").to_string_lossy().into_owned();
        assert!(Dir::check(&abs).is_ok());
        assert_eq!(resolve_dir(&abs), PathBuf::from(abs));
        assert!(resolve_dir("").ends_with(DEFAULT_SUBDIR));
    }
}
