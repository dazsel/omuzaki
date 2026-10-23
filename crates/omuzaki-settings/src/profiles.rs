//! Profile files (D7): `profiles/<id>.toml` in the config folder, a display `name` and a sparse `[values]` table of
//! `User` keys a profile may hold (`InProfile::Member`).
//!
//! A file is edited in place like the settings files (`document::apply_each`), so a key this build does not know, or
//! a comment, stays as it was. Reading is forgiving: a file that does not parse is skipped with a finding, a key a
//! profile may not hold (a `Machine` or `Excluded` key) is ignored with a finding, and a value that fails its rules
//! falls back with a finding. The base is never touched by any of it.

#![allow(
    clippy::disallowed_methods,
    reason = "the store's own profile files in the app's config folder, never a path an op was given"
)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use omuzaki_core::fsx::write_atomic;
use omuzaki_settings_api::error::{SettingError, SettingErrorKind, SettingFinding};
use omuzaki_settings_api::meta::{InProfile, SettingScope};
use omuzaki_settings_api::profile::{ProfileId, validate_name};
use omuzaki_settings_api::registry::SettingRegistration;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::backend::{LOCK_WAIT, lock_beside, storage};
use crate::document::{self, Edit};

/// The table that holds a profile's values.
const VALUES: &str = "values";

/// One profile as the store holds it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Profile {
    pub name: String,
    /// The values by key; only keys a profile may hold.
    pub values: BTreeMap<String, Value>,
}

/// Whether a profile may hold `reg`'s key.
pub(crate) fn member(reg: &SettingRegistration) -> bool {
    reg.meta.profile == InProfile::Member && reg.meta.scope == SettingScope::User
}

/// A display name, trimmed, checked and NFC-normalised.
pub(crate) fn clean_name(name: &str) -> Result<String, SettingError> {
    validate_name(&name.nfc().collect::<String>())
}

/// Four lowercase letters or digits, different on each call: the suffix of a new profile's id.
pub(crate) fn suffix(seed: u64) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    let mut n = u64::try_from(nanos % u128::from(u64::MAX)).unwrap_or(0) ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut out = String::with_capacity(ProfileId::SUFFIX_LEN);
    for _ in 0..ProfileId::SUFFIX_LEN {
        out.push(char::from(DIGITS[usize::try_from(n % 36).unwrap_or(0)]));
        n /= 36;
    }
    out
}

/// The profile files' folder and how to read and write them.
pub(crate) struct ProfileFiles {
    dir: PathBuf,
}

impl ProfileFiles {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, id: &ProfileId) -> PathBuf {
        self.dir.join(format!("{id}.toml"))
    }

    /// Every profile in the folder, and what was not used.
    pub(crate) fn load(
        &self,
        keys: &[&'static SettingRegistration],
    ) -> (BTreeMap<ProfileId, Profile>, Vec<SettingFinding>) {
        let mut profiles = BTreeMap::new();
        let mut findings = Vec::new();
        let Ok(entries) = fs::read_dir(&self.dir) else { return (profiles, findings) };
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let Some(id) = path
                .extension()
                .filter(|ext| *ext == "toml")
                .and_then(|_| path.file_stem())
                .and_then(|stem| ProfileId::new(stem.to_string_lossy().into_owned()).ok())
            else {
                continue;
            };
            let file = format!("profiles/{id}.toml");
            match fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|text| document::parse(&text)) {
                Ok(doc) => {
                    profiles.insert(id, read_profile(&doc, keys, &file, &mut findings));
                }
                Err(error) => {
                    let reason = format!("{file} does not read ({error}); the profile is skipped");
                    let error = SettingError::new("", SettingErrorKind::Storage { reason });
                    findings.push(SettingFinding::new(error, Value::Null, file));
                }
            }
        }
        (profiles, findings)
    }

    /// Writes `edits` (keys are `name` and `values.<key>`) into the profile's file, in place, under the file's lock.
    pub(crate) fn write(&self, id: &ProfileId, edits: &[Edit]) -> Result<(), SettingError> {
        let path = self.path(id);
        fs::create_dir_all(&self.dir).map_err(|e| storage(&self.dir, "create the folder", &e))?;
        let _lock = lock_beside(&path, LOCK_WAIT)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => None,
            Err(e) => return Err(storage(&path, "read", &e)),
        };
        let applied = document::apply_each(text.as_deref(), edits, &BTreeMap::new()).map_err(|e| {
            let reason = format!("profiles/{id}.toml can't take the change: {e:?}");
            SettingError::new("", SettingErrorKind::Storage { reason })
        })?;
        if let Some((key, reason)) = applied.refused.first() {
            let reason = format!("profiles/{id}.toml can't hold `{key}` in place: {reason}");
            return Err(SettingError::new("", SettingErrorKind::Storage { reason }));
        }
        if text.as_deref() == Some(applied.text.as_str()) {
            return Ok(());
        }
        write_atomic(&path, &applied.text).map_err(|e| storage(&path, "write", &e))
    }

    /// Removes the profile's file.
    pub(crate) fn remove(&self, id: &ProfileId) -> Result<(), SettingError> {
        let path = self.path(id);
        let _lock = lock_beside(&path, LOCK_WAIT)?;
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(storage(&path, "remove", &e)),
        }
    }
}

/// The edit that writes (or removes) `key` in a profile file.
pub(crate) fn value_edit(key: &str, value: Option<&Value>) -> Edit {
    Edit { key: format!("{VALUES}.{key}"), value: value.map(|v| (v.clone(), 1)) }
}

/// The edit that writes a profile's display name.
pub(crate) fn name_edit(name: &str) -> Edit {
    Edit { key: "name".to_owned(), value: Some((Value::String(name.to_owned()), 1)) }
}

/// The edits that make a new file hold exactly `profile`.
pub(crate) fn whole(profile: &Profile, before: Option<&Profile>) -> Vec<Edit> {
    let mut edits = vec![name_edit(&profile.name)];
    if let Some(before) = before {
        edits.extend(before.values.keys().filter(|k| !profile.values.contains_key(*k)).map(|k| value_edit(k, None)));
    }
    edits.extend(profile.values.iter().map(|(k, v)| value_edit(k, Some(v))));
    edits
}

fn read_profile(
    doc: &toml_edit::DocumentMut,
    keys: &[&'static SettingRegistration],
    file: &str,
    findings: &mut Vec<SettingFinding>,
) -> Profile {
    let name = document::lookup(doc, "name").and_then(|v| v.as_str().map(str::to_owned));
    let name = name.and_then(|n| clean_name(&n).ok()).unwrap_or_else(|| file.to_owned());
    let mut values = BTreeMap::new();
    for reg in keys {
        let key = reg.meta.key;
        let Some(value) = document::lookup(doc, &format!("{VALUES}.{key}")) else { continue };
        if !member(reg) {
            let reason = "a profile can't hold this key (a machine key, or one about privacy, language or startup); \
                          ignored";
            findings.push(SettingFinding::new(SettingError::invalid(key, reason), value, file));
            continue;
        }
        match reg.validate(&value) {
            Ok(()) => {
                values.insert(key.to_owned(), value);
            }
            Err(error) => findings.push(SettingFinding::new(error, value, file)),
        }
    }
    Profile { name, values }
}
