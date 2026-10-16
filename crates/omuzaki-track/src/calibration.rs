//! Saved calibrations (PF-20): what a restart gives back to the retarget.
//!
//! A neutral-face calibration is worth keeping: it takes ten seconds to make and it is right only for one performer
//! on one kind of device, wearing one character's face. [`CalibrationStore`] holds the finished
//! [`Calibration`]s by character and [`DeviceProfile`], as plain data the settings store writes; it does not touch
//! the files itself. [`for_character`](CalibrationStore::for_character) is the restore: it hands back the calibration
//! saved for exactly that character and device class and nothing else, so a calibration made for another character
//! or for an iPhone never reaches a webcam performer.
//!
//! A stored value is checked before it is used ([`CalibrationStore::check`]): every range finite and wider than
//! nothing, the head rest pose finite. A calibration that fails is a finding for the settings store and the restore
//! falls back to the device's default, so a hand-edited file cannot freeze the face or fill it with NaN.

use std::collections::BTreeMap;

use omuzaki_anim_api::{Calibration, Channel, DeviceProfile};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How many characters a store keeps calibrations for: a bound on a file that grows with every character tried.
pub const MAX_CHARACTERS: usize = 64;

/// The most characters in a character id, the key of an entry.
pub const MAX_ID_CHARS: usize = 128;

/// One stored calibration: usable, or kept exactly as it was read because it could not be used.
#[derive(Clone, Debug, PartialEq)]
enum Entry {
    /// A calibration that read back and passed its checks.
    Good(Box<Calibration>),
    /// What the file held: a value that did not parse, failed a check or was filed under the wrong device class. It is
    /// never used and never rewritten, so one bad entry cannot cost the person the good ones, and a hand fix of the
    /// file is still there to make.
    Unreadable(serde_json::Value),
}

/// Calibrations by character and device class.
///
/// Entries are read one by one: a calibration that cannot be used is kept untouched and listed by
/// [`unreadable`](Self::unreadable), and every other entry still loads, is returned and is written back. Only a
/// whole value of the wrong shape (not a table of tables) cannot be read at all.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalibrationStore {
    entries: BTreeMap<String, BTreeMap<String, Entry>>,
}

impl JsonSchema for CalibrationStore {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "CalibrationStore".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        <BTreeMap<String, BTreeMap<String, Calibration>>>::json_schema(generator)
    }
}

impl Serialize for CalibrationStore {
    /// Writes the store without `null`s in the good entries (a head pose with no position leaves the field out, because
    /// the settings files are TOML, which has no null) and the unreadable ones as they were read.
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut out = serde_json::Map::new();
        for (character, by_profile) in &self.entries {
            let mut profiles = serde_json::Map::new();
            for (profile, entry) in by_profile {
                let value = match entry {
                    Entry::Good(calibration) => {
                        let mut value = serde_json::to_value(**calibration).map_err(serde::ser::Error::custom)?;
                        strip_nulls(&mut value);
                        value
                    }
                    Entry::Unreadable(raw) => raw.clone(),
                };
                profiles.insert(profile.clone(), value);
            }
            out.insert(character.clone(), serde_json::Value::Object(profiles));
        }
        serde_json::Value::Object(out).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for CalibrationStore {
    /// Reads every entry on its own. Only a value that is not a table of tables is an error.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = BTreeMap::<String, BTreeMap<String, serde_json::Value>>::deserialize(deserializer)?;
        let entries = raw
            .into_iter()
            .map(|(character, by_profile)| {
                let entries = by_profile
                    .into_iter()
                    .map(|(filed, raw)| {
                        let entry = match serde_json::from_value::<Calibration>(raw.clone()) {
                            Ok(calibration) if check_calibration(&character, &filed, &calibration).is_ok() => {
                                Entry::Good(Box::new(calibration))
                            }
                            _ => Entry::Unreadable(raw),
                        };
                        (filed, entry)
                    })
                    .collect();
                (character, entries)
            })
            .collect();
        Ok(Self { entries })
    }
}

/// Removes every object member that is `null`, at any depth.
fn strip_nulls(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            map.retain(|_, member| !member.is_null());
            map.values_mut().for_each(strip_nulls);
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(strip_nulls),
        _ => {}
    }
}

/// Why a store cannot be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CalibrationError {
    /// More than [`MAX_CHARACTERS`] characters.
    #[error("calibrations for {count} characters; at most {MAX_CHARACTERS} are kept")]
    TooMany {
        /// How many there were.
        count: usize,
    },
    /// A character id that is empty or longer than [`MAX_ID_CHARS`].
    #[error("a character id is 1 to {MAX_ID_CHARS} characters")]
    BadId,
    /// An entry whose calibration is for another device class than the one it is filed under.
    #[error("the calibration for character '{character}' filed under {filed} was made for {made}")]
    WrongProfile {
        /// The character.
        character: String,
        /// The class it is filed under.
        filed: String,
        /// The class it was made for.
        made: String,
    },
    /// A range or rest pose that is not a finite, usable number.
    #[error("the calibration for character '{character}' has {what} that is not finite or has no span")]
    Unusable {
        /// The character.
        character: String,
        /// What is wrong: a channel name or the head rest pose.
        what: String,
    },
}

impl CalibrationStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// The calibration saved for `character` on `profile`, if there is one. Another character's, or another device
    /// class's, is never returned.
    pub fn for_character(&self, character: &str, profile: DeviceProfile) -> Option<Calibration> {
        match self.entries.get(character).and_then(|by_profile| by_profile.get(&profile_key(profile))) {
            Some(Entry::Good(calibration)) => Some(**calibration),
            Some(Entry::Unreadable(_)) | None => None,
        }
    }

    /// The entries that could not be used, as `(character, device class)`: kept as they were read and never rewritten.
    /// A new calibration saved for the same character and device class replaces its entry, which is the person's own
    /// recalibration; every other unreadable entry stays.
    pub fn unreadable(&self) -> Vec<(String, String)> {
        self.entries
            .iter()
            .flat_map(|(character, by_profile)| {
                by_profile
                    .iter()
                    .filter(|(_, entry)| matches!(entry, Entry::Unreadable(_)))
                    .map(move |(profile, _)| (character.clone(), profile.clone()))
            })
            .collect()
    }

    /// Saves `calibration` for `character` (under the device class it was made for), replacing the one before.
    ///
    /// # Errors
    /// [`CalibrationError`] for an id that is not 1 to [`MAX_ID_CHARS`] characters, a calibration with a value that is
    /// not usable, or a new character past [`MAX_CHARACTERS`]; the store is unchanged then.
    pub fn save(&mut self, character: &str, calibration: Calibration) -> Result<(), CalibrationError> {
        check_id(character)?;
        check_calibration(character, &profile_key(calibration.profile), &calibration)?;
        if !self.entries.contains_key(character) && self.entries.len() >= MAX_CHARACTERS {
            return Err(CalibrationError::TooMany { count: self.entries.len() + 1 });
        }
        self.entries
            .entry(character.to_owned())
            .or_default()
            .insert(profile_key(calibration.profile), Entry::Good(Box::new(calibration)));
        Ok(())
    }

    /// Forgets everything saved for `character`. Returns whether there was anything.
    pub fn forget(&mut self, character: &str) -> bool {
        self.entries.remove(character).is_some()
    }

    /// How many characters have a calibration.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is saved.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Checks what the settings store runs on a stored value before it is used: how many characters and their ids. An
    /// entry that cannot be used does not fail the value: it is listed by [`unreadable`](Self::unreadable) instead.
    ///
    /// # Errors
    /// The first [`CalibrationError`] found.
    pub fn check(&self) -> Result<(), CalibrationError> {
        if self.entries.len() > MAX_CHARACTERS {
            return Err(CalibrationError::TooMany { count: self.entries.len() });
        }
        self.entries.keys().try_for_each(|character| check_id(character))
    }
}

fn check_id(character: &str) -> Result<(), CalibrationError> {
    let chars = character.chars().count();
    if (1..=MAX_ID_CHARS).contains(&chars) { Ok(()) } else { Err(CalibrationError::BadId) }
}

/// The name a device class is filed under: its serialised name (`webcam`, `arkit`).
fn profile_key(profile: DeviceProfile) -> String {
    serde_json::to_value(profile).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

fn check_calibration(character: &str, filed: &str, calibration: &Calibration) -> Result<(), CalibrationError> {
    let made = profile_key(calibration.profile);
    if made != filed {
        return Err(CalibrationError::WrongProfile { character: character.to_owned(), filed: filed.to_owned(), made });
    }
    let unusable = |what: String| CalibrationError::Unusable { character: character.to_owned(), what };
    for info in Channel::arkit() {
        let range = calibration.range(info.channel);
        if !(range.lo.is_finite() && range.hi.is_finite() && range.hi > range.lo) {
            return Err(unusable(format!("the range of {:?}", info.channel)));
        }
    }
    let head = calibration.head_rest;
    let position_ok = head.position.is_none_or(omuzaki_core::glam::Vec3::is_finite);
    if !(head.yaw.is_finite() && head.pitch.is_finite() && head.roll.is_finite() && position_ok) {
        return Err(unusable("the head rest pose".to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::{ChannelRange, HeadPose};

    use super::*;

    fn tuned(profile: DeviceProfile) -> Calibration {
        let mut calibration = Calibration::identity(profile);
        calibration.set_range(Channel::JAW_OPEN, ChannelRange { lo: 0.05, hi: 0.4 });
        calibration.head_rest = HeadPose { yaw: 0.1, pitch: -0.05, roll: 0.0, position: None };
        calibration
    }

    #[test]
    fn a_calibration_comes_back_for_its_character_and_device_only() {
        let mut store = CalibrationStore::new();
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        assert_eq!(store.for_character("kiri", DeviceProfile::Webcam), Some(tuned(DeviceProfile::Webcam)));
        assert_eq!(store.for_character("kiri", DeviceProfile::Arkit), None, "another device class");
        assert_eq!(store.for_character("mira", DeviceProfile::Webcam), None, "another character");
    }

    #[test]
    fn the_store_survives_a_round_trip_through_json() {
        let mut store = CalibrationStore::new();
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        store.save("kiri", tuned(DeviceProfile::Arkit)).unwrap();
        store.save("mira", tuned(DeviceProfile::Webcam)).unwrap();
        let json = serde_json::to_value(&store).unwrap();
        assert_eq!(serde_json::from_value::<CalibrationStore>(json).unwrap(), store);
    }

    #[test]
    fn the_written_form_has_no_null_and_reads_back_equal() {
        let mut store = CalibrationStore::new();
        store.save("kiri", Calibration::identity(DeviceProfile::Webcam)).unwrap(); // head rest with no position
        let json = serde_json::to_value(&store).unwrap();
        assert!(!json.to_string().contains("null"), "{json}");
        assert_eq!(serde_json::from_value::<CalibrationStore>(json).unwrap(), store);
    }

    #[test]
    fn a_saved_calibration_replaces_the_one_before() {
        let mut store = CalibrationStore::new();
        store.save("kiri", Calibration::identity(DeviceProfile::Webcam)).unwrap();
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.for_character("kiri", DeviceProfile::Webcam), Some(tuned(DeviceProfile::Webcam)));
        assert!(store.forget("kiri") && store.is_empty());
    }

    #[test]
    fn unusable_values_are_refused_and_leave_the_store_alone() {
        let mut store = CalibrationStore::new();
        let mut nan = tuned(DeviceProfile::Webcam);
        nan.set_range(Channel::JAW_OPEN, ChannelRange { lo: f32::NAN, hi: 1.0 });
        assert!(matches!(store.save("kiri", nan), Err(CalibrationError::Unusable { .. })));
        let mut flat = tuned(DeviceProfile::Webcam);
        flat.set_range(Channel::JAW_OPEN, ChannelRange { lo: 0.5, hi: 0.5 });
        assert!(matches!(store.save("kiri", flat), Err(CalibrationError::Unusable { .. })));
        let mut head = tuned(DeviceProfile::Webcam);
        head.head_rest.yaw = f32::INFINITY;
        assert!(matches!(store.save("kiri", head), Err(CalibrationError::Unusable { .. })));
        assert_eq!(store.save("", tuned(DeviceProfile::Webcam)), Err(CalibrationError::BadId));
        assert!(store.is_empty());
    }

    #[test]
    fn a_misfiled_entry_is_unreadable_and_kept_while_the_others_load() {
        let mut store = CalibrationStore::new();
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        store.save("mira", tuned(DeviceProfile::Webcam)).unwrap();
        let mut json = serde_json::to_value(&store).unwrap();
        // kiri's webcam calibration filed under `arkit`.
        let webcam = json["kiri"]["webcam"].take();
        json["kiri"] = serde_json::json!({ "arkit": webcam.clone() });
        let moved: CalibrationStore = serde_json::from_value(json).unwrap();
        assert_eq!(moved.for_character("kiri", DeviceProfile::Arkit), None);
        assert_eq!(
            moved.for_character("mira", DeviceProfile::Webcam),
            Some(tuned(DeviceProfile::Webcam)),
            "the good one loads"
        );
        assert_eq!(moved.unreadable(), [("kiri".to_owned(), "arkit".to_owned())]);
        // Written back as it was read.
        assert_eq!(serde_json::to_value(&moved).unwrap()["kiri"]["arkit"], webcam);
        assert_eq!(moved.check(), Ok(()), "an unreadable entry does not fail the value");
    }

    #[test]
    fn a_corrupt_entry_survives_a_save_for_another_character() {
        let mut store = CalibrationStore::new();
        store.save("mira", tuned(DeviceProfile::Webcam)).unwrap();
        let mut json = serde_json::to_value(&store).unwrap();
        json["kiri"] = serde_json::json!({ "webcam": { "profile": "webcam", "ranges": "garbage" } });
        let mut loaded: CalibrationStore = serde_json::from_value(json).unwrap();
        assert_eq!(loaded.unreadable(), [("kiri".to_owned(), "webcam".to_owned())]);
        loaded.save("noor", tuned(DeviceProfile::Webcam)).unwrap();
        let written = serde_json::to_value(&loaded).unwrap();
        assert_eq!(written["kiri"]["webcam"]["ranges"], "garbage", "the corrupt entry is untouched");
        assert!(written["mira"]["webcam"].is_object() && written["noor"]["webcam"].is_object());
        // Recalibrating that very character replaces its entry: the person's own fix.
        loaded.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        assert_eq!(loaded.unreadable(), Vec::<(String, String)>::new());
    }

    #[test]
    fn the_store_is_bounded() {
        let mut store = CalibrationStore::new();
        for i in 0..MAX_CHARACTERS {
            store.save(&format!("c{i}"), Calibration::identity(DeviceProfile::Webcam)).unwrap();
        }
        assert!(matches!(
            store.save("one-too-many", Calibration::identity(DeviceProfile::Webcam)),
            Err(CalibrationError::TooMany { .. })
        ));
        store.save("c0", tuned(DeviceProfile::Webcam)).unwrap();
    }
}
