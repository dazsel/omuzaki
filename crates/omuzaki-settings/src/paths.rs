//! Where the settings files live (D3).

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use omuzaki_settings_api::error::{SettingError, SettingErrorKind};

/// Overrides the config folder (tests, portable installs).
pub const CONFIG_DIR_VAR: &str = "OMUZAKI_CONFIG_DIR";

/// Overrides the local data folder (tests, portable installs).
pub const DATA_DIR_VAR: &str = "OMUZAKI_DATA_DIR";

/// The two folders the store writes to.
///
/// `config` may roam between computers and holds `settings.toml`; `data` is local to this computer and holds
/// `machine.toml`. A `Machine` value cannot roam because it is never written under `config`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paths {
    config: PathBuf,
    data: PathBuf,
}

impl Paths {
    /// Explicit folders: what tests pass, so they never touch the real ones.
    #[must_use]
    pub fn new(config: impl Into<PathBuf>, data: impl Into<PathBuf>) -> Self {
        Self { config: config.into(), data: data.into() }
    }

    /// The OS folders, each overridable by [`CONFIG_DIR_VAR`] and [`DATA_DIR_VAR`].
    ///
    /// # Errors
    /// [`SettingErrorKind::Storage`] when the OS reports no home folder and an override is missing.
    pub fn from_env() -> Result<Self, SettingError> {
        Self::resolve(
            std::env::var_os(CONFIG_DIR_VAR).map(PathBuf::from),
            std::env::var_os(DATA_DIR_VAR).map(PathBuf::from),
        )
    }

    fn resolve(config: Option<PathBuf>, data: Option<PathBuf>) -> Result<Self, SettingError> {
        if let (Some(config), Some(data)) = (&config, &data) {
            return Ok(Self::new(config, data));
        }
        let dirs = ProjectDirs::from("", "", "Omuzaki").ok_or_else(|| {
            let reason = format!("no home folder: set {CONFIG_DIR_VAR} and {DATA_DIR_VAR}");
            SettingError::new("", SettingErrorKind::Storage { reason })
        })?;
        Ok(Self::new(
            config.unwrap_or_else(|| dirs.config_dir().to_path_buf()),
            data.unwrap_or_else(|| dirs.data_local_dir().to_path_buf()),
        ))
    }

    /// The config folder (`settings.toml`).
    #[must_use]
    pub fn config(&self) -> &Path {
        &self.config
    }

    /// The local data folder (`machine.toml`).
    #[must_use]
    pub fn data(&self) -> &Path {
        &self.data
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_overrides_win_without_asking_the_os() {
        let p = Paths::resolve(Some("/c".into()), Some("/d".into())).unwrap();
        assert_eq!((p.config(), p.data()), (Path::new("/c"), Path::new("/d")));
    }

    #[test]
    fn one_override_replaces_only_its_folder() {
        let Ok(p) = Paths::resolve(Some("/c".into()), None) else { return };
        assert_eq!(p.config(), Path::new("/c"));
        assert_ne!(p.data(), Path::new("/c"));
    }
}
