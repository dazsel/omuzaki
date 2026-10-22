//! The app's shared cache folder: where every pinned file (models, runtime libraries, fonts) is kept.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use directories::BaseDirs;

/// The environment variable that replaces the OS cache folder (tests, portable installs, a bigger disk).
pub const CACHE_DIR_ENV: &str = "OMUZAKI_CACHE_DIR";

/// The folder below the OS cache folder that holds Omuzaki's files.
const SUBFOLDER: &str = "Omuzaki/cache";

/// There is nowhere to keep the cache: no override was given and the OS reports no cache folder (no home folder).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
#[error("no cache folder found (no home folder); set {CACHE_DIR_ENV} to a folder to use")]
pub struct CacheDirError;

/// The folder the app caches downloads in.
///
/// A non-empty `OMUZAKI_CACHE_DIR` wins and is used as given. Otherwise it is the OS cache folder plus
/// `Omuzaki/cache`: `%LOCALAPPDATA%` on Windows, `~/Library/Caches` on macOS, `$XDG_CACHE_HOME` or `~/.cache` on Linux.
/// The folder is not created here; the cache creates it on first use. Hand it to
/// `VerifiedCache::new(app_cache_dir()?, HttpFetcher::new())` so fonts and models share one folder.
///
/// # Errors
/// [`CacheDirError`] when neither the variable nor an OS cache folder is available.
pub fn app_cache_dir() -> Result<PathBuf, CacheDirError> {
    let env = std::env::var_os(CACHE_DIR_ENV);
    let base = BaseDirs::new();
    resolve(env.as_deref(), base.as_ref().map(directories::BaseDirs::cache_dir))
}

/// [`app_cache_dir`] with the environment passed in, so tests need not touch the process environment.
///
/// `env_value` is the value of `OMUZAKI_CACHE_DIR` (an empty one counts as unset) and `base` the OS cache folder.
///
/// # Errors
/// [`CacheDirError`] when `env_value` is unset or empty and `base` is `None`.
pub fn resolve(env_value: Option<&OsStr>, base: Option<&Path>) -> Result<PathBuf, CacheDirError> {
    match env_value {
        Some(value) if !value.is_empty() => Ok(PathBuf::from(value)),
        _ => base.map(|base| base.join(SUBFOLDER)).ok_or(CacheDirError),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "a panic is a clear test failure")]
mod tests {
    use super::*;

    #[test]
    fn the_environment_variable_wins() {
        let dir = resolve(Some(OsStr::new("/data/cache")), Some(Path::new("/home/x/.cache")));
        assert_eq!(dir, Ok(PathBuf::from("/data/cache")));
    }

    #[test]
    fn an_empty_variable_is_ignored() {
        let dir = resolve(Some(OsStr::new("")), Some(Path::new("/base")));
        assert_eq!(dir, Ok(Path::new("/base").join("Omuzaki/cache")));
    }

    #[test]
    fn no_variable_and_no_base_folder_is_an_error_naming_the_variable() {
        let error = resolve(None, None).expect_err("nowhere to cache");
        assert!(error.to_string().contains(CACHE_DIR_ENV), "{error}");
        assert_eq!(resolve(Some(OsStr::new("")), None), Err(CacheDirError));
    }

    #[test]
    fn the_base_folder_is_joined_with_omuzaki_cache() {
        let dir = resolve(None, Some(Path::new("/base"))).expect("resolves");
        assert!(dir.ends_with("Omuzaki/cache"));
        assert!(dir.starts_with("/base"));
    }

    #[test]
    fn the_real_function_returns_an_absolute_path_on_this_host() {
        // CI hosts have a home folder. A relative override is supported and returned as given, so absoluteness is only
        // asserted when the variable is not set.
        let dir = app_cache_dir().expect("a cache folder on this host");
        if std::env::var_os(CACHE_DIR_ENV).is_none_or(|v| v.is_empty()) {
            assert!(dir.is_absolute(), "{}", dir.display());
        }
    }
}
