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

/// Calibrations by character and device class.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct CalibrationStore {
    entries: BTreeMap<String, BTreeMap<String, Calibration>>,
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
        self.entries.get(character).and_then(|by_profile| by_profile.get(&profile_key(profile))).copied()
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
        self.entries.entry(character.to_owned()).or_default().insert(profile_key(calibration.profile), calibration);
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

    /// Checks every entry: what the settings store runs on a stored value before it is used.
    ///
    /// # Errors
    /// The first [`CalibrationError`] found.
    pub fn check(&self) -> Result<(), CalibrationError> {
        if self.entries.len() > MAX_CHARACTERS {
            return Err(CalibrationError::TooMany { count: self.entries.len() });
        }
        for (character, by_profile) in &self.entries {
            check_id(character)?;
            for (filed, calibration) in by_profile {
                check_calibration(character, filed, calibration)?;
            }
        }
        Ok(())
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
        return Err(CalibrationError::WrongProfile {
            character: character.to_owned(),
            filed: filed.to_owned(),
            made,
        });
    }
    let unusable = |what: String| CalibrationError::Unusable { character: character.to_owned(), what };
    for info in Channel::arkit() {
        let range = calibration.range(info.channel);
        if !(range.lo.is_finite() && range.hi.is_finite() && range.hi > range.lo) {
            return Err(unusable(format!("the range of {:?}", info.channel)));
        }
    }
    let head = calibration.head_rest;
    let position_ok = head.position.is_none_or(|p| p.is_finite());
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
    fn a_hand_edited_store_is_checked_before_it_is_used() {
        let mut store = CalibrationStore::new();
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        let mut json = serde_json::to_value(&store).unwrap();
        // The webcam calibration filed under `arkit`.
        let webcam = json["kiri"]["webcam"].take();
        json["kiri"] = serde_json::json!({ "arkit": webcam });
        let moved: CalibrationStore = serde_json::from_value(json).unwrap();
        assert!(matches!(moved.check(), Err(CalibrationError::WrongProfile { .. })));
        assert_eq!(store.check(), Ok(()));
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
