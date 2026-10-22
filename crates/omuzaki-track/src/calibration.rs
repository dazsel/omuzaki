//! Saved calibrations (PF-20): what a restart gives back to the retarget.
//!
//! A neutral-face calibration is worth keeping: it takes ten seconds to make and it is right only for one performer
//! on one kind of device, wearing one character's face. ADR 0026 classes it as *state*, not as a setting: it is
//! written by the app as a side effect of a calibration, has no op to set it and no default, so it has its own file,
//! `perform/calibration.toml` in the local data folder ([`calibration_file`]; it describes a face on this computer's
//! camera, so it never roams). [`CalibrationStore`] reads that file and writes it back.
//!
//! # The file
//!
//! ```toml
//! version = 1
//!
//! [[calibration]]
//! character = "kiri"
//! profile = "webcam"
//! [calibration.calibration]
//! # profile, ranges and head_rest, as `omuzaki-anim-api` writes a `Calibration`
//! ```
//!
//! TOML has no null, so a head rest pose without a position leaves the field out. A top-level key this build does not
//! know is kept and written back.
//!
//! # Invariants
//!
//! 1. **One bad part never costs the others.** Every `[[calibration]]` element is read on its own, and the character
//!    and device class an element is filed under decide nothing about its neighbours. An element that cannot be used
//!    (not a table, a missing or mistyped field, a calibration that does not read, a value out of range, one filed
//!    under the wrong device class, a repeat of a key already read) is listed in [`CalibrationStore::findings`] and
//!    kept exactly as it was read.
//! 2. **What was read is what is written.** A kept element is written back untouched, so a later save for another
//!    character does not lose it and a hand fix of the file is still possible. The one element a save replaces is
//!    the one filed under the same character and device class: the person's own recalibration. [`forget`](CalibrationStore::forget)
//!    drops one device class of a character on purpose, and [`forget_all_devices`](CalibrationStore::forget_all_devices)
//!    every class of it.
//! 3. **A file that cannot be read at all is never written.** A file that does not parse, has a version this build does
//!    not know, or has `calibration` of the wrong shape leaves the store *locked*: it reads as empty, reports the
//!    reason, and [`save`](CalibrationStore::save), [`forget`](CalibrationStore::forget) and
//!    [`forget_all_devices`](CalibrationStore::forget_all_devices) refuse
//!    ([`CalibrationError::Locked`]), because writing would replace a file whose content is unknown.
//! 4. **A calibration is checked before it is used** ([`CalibrationStore::for_character`] only hands back one that
//!    passed): every range finite and wider than nothing, the head rest pose finite. A hand-edited file cannot freeze
//!    the face or fill it with NaN.
//! 5. **Never another character's or device's.** [`for_character`](CalibrationStore::for_character) returns the
//!    calibration filed for exactly that character and device class.
//! 6. **A failed write changes nothing.** The file is replaced atomically and the store keeps its previous state when
//!    the write fails.
//!
//! No finding text carries a calibration value, and the store logs nothing: a calibration and a character id describe
//! a face.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omuzaki_anim_api::{Calibration, Channel, DeviceProfile};
use omuzaki_core::fsx::write_atomic;

/// The file format version this build reads and writes.
pub const FILE_VERSION: i64 = 1;

/// How many characters a store keeps calibrations for: a bound on a file that grows with every character tried.
pub const MAX_CHARACTERS: usize = 64;

/// The most characters in a character id, the key of an entry.
pub const MAX_ID_CHARS: usize = 128;

/// Where the calibrations live, under the local data folder: `perform/calibration.toml`.
pub fn calibration_file(data_dir: &Path) -> PathBuf {
    data_dir.join("perform").join("calibration.toml")
}

/// A character and the device class a calibration is filed under.
type Key = (String, String);

/// Something in the file that could not be used. Carries no calibration value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CalibrationFinding {
    /// The character the entry is filed under, when it could be read.
    pub character: Option<String>,
    /// The device class the entry is filed under, when it could be read.
    pub profile: Option<String>,
    /// What is wrong, in words. `None` for `character` means a whole-file problem or an entry with no readable id.
    pub reason: String,
}

/// An element of the file that could not be used, kept as it was read.
#[derive(Clone, Debug, PartialEq)]
struct Kept {
    key: Option<Key>,
    raw: toml::Value,
}

/// Calibrations by character and device class, with the file they come from.
///
/// See the module docs for the invariants. A store from [`new`](Self::new) has no file and lives in memory.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CalibrationStore {
    path: Option<PathBuf>,
    good: BTreeMap<Key, Box<Calibration>>,
    kept: Vec<Kept>,
    /// Top-level keys of the file this build does not know.
    extra: toml::Table,
    /// Why the file could not be read at all; the store refuses to write while this is set.
    locked: Option<String>,
    findings: Vec<CalibrationFinding>,
}

/// Why a calibration cannot be kept or a store cannot be changed.
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
    #[error("a calibration filed under {filed} was made for {made}")]
    WrongProfile {
        /// The class it is filed under.
        filed: String,
        /// The class it was made for.
        made: String,
    },
    /// A range or rest pose that is not a finite, usable number.
    #[error("a calibration has {what} that is not finite or has no span")]
    Unusable {
        /// What is wrong: a channel name or the head rest pose.
        what: String,
    },
    /// The file could not be read at all, so nothing is written over it.
    #[error("the saved calibrations could not be read ({reason}); fix or remove the file before saving another")]
    Locked {
        /// Why the file could not be read.
        reason: String,
    },
    /// The file could not be written. The store is as it was.
    #[error("the calibrations could not be written: {reason}")]
    Write {
        /// The operating system's reason.
        reason: String,
    },
}

impl CalibrationError {
    /// A short reason without any id, for a finding.
    fn reason(&self) -> String {
        match self {
            Self::WrongProfile { .. } => "filed under a device class it was not made for".to_owned(),
            Self::Unusable { what } => format!("{what} is not finite or has no span"),
            Self::BadId => format!("the character id is not 1 to {MAX_ID_CHARS} characters"),
            other => other.to_string(),
        }
    }
}

impl CalibrationStore {
    /// An empty store with no file: it lives in memory (the tests, the CLI).
    pub fn new() -> Self {
        Self::default()
    }

    /// Opens the store on `path` (see [`calibration_file`]). A missing file is an empty store. Never fails: whatever
    /// cannot be read is listed by [`findings`](Self::findings), and a file that cannot be read at all locks the store
    /// against writing (invariant 3).
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut store = match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::new(),
            Err(e) => Self::locked_with(format!("the file cannot be read: {}", e.kind())),
        };
        store.path = Some(path);
        store
    }

    fn locked_with(reason: String) -> Self {
        let finding = CalibrationFinding { character: None, profile: None, reason: reason.clone() };
        Self { locked: Some(reason), findings: vec![finding], ..Self::default() }
    }

    /// Reads the text of a file. Each element is read on its own (invariant 1).
    fn parse(text: &str) -> Self {
        let mut table: toml::Table = match text.parse() {
            Ok(table) => table,
            Err(e) => {
                // Not `Display`: that quotes the offending line, which may hold a calibration.
                let line = e.span().map_or(0, |span| text[..span.start.min(text.len())].matches('\n').count() + 1);
                return Self::locked_with(format!("the file does not parse ({}, line {line})", e.message()));
            }
        };
        match table.remove("version") {
            None | Some(toml::Value::Integer(FILE_VERSION)) => {}
            Some(_) => return Self::locked_with("the file has a version this build does not know".to_owned()),
        }
        let items = match table.remove("calibration") {
            None => Vec::new(),
            Some(toml::Value::Array(items)) => items,
            Some(_) => return Self::locked_with("`calibration` is not a list of tables".to_owned()),
        };
        let mut store = Self { extra: table, ..Self::default() };
        for item in items {
            store.read_element(item);
        }
        store
    }

    fn read_element(&mut self, item: toml::Value) {
        let (key, outcome) = decode(&item);
        let problem = match (&key, outcome) {
            (Some(key), Ok(calibration)) => {
                let characters = self.good.keys().map(|(c, _)| c).collect::<std::collections::BTreeSet<_>>();
                if self.good.contains_key(key) {
                    Some("the same character and device class appears twice; the first is used".to_owned())
                } else if !characters.contains(&key.0) && characters.len() >= MAX_CHARACTERS {
                    Some(CalibrationError::TooMany { count: characters.len() + 1 }.to_string())
                } else {
                    self.good.insert(key.clone(), Box::new(calibration));
                    return;
                }
            }
            (_, Ok(_)) => Some("the entry has no readable character and device class".to_owned()),
            (_, Err(reason)) => Some(reason),
        };
        let (character, profile) = key.clone().map_or((None, None), |(c, p)| (Some(c), Some(p)));
        self.findings.push(CalibrationFinding { character, profile, reason: problem.unwrap_or_default() });
        self.kept.push(Kept { key, raw: item });
    }

    /// What in the file could not be used, each kept as it was read. Empty for a clean file. See invariant 1 and 3.
    pub fn findings(&self) -> &[CalibrationFinding] {
        &self.findings
    }

    /// Why the whole file could not be read, if it could not: the store is then empty and refuses to write.
    pub fn locked(&self) -> Option<&str> {
        self.locked.as_deref()
    }

    /// The calibration saved for `character` on `profile`, if there is one. Another character's, or another device
    /// class's, is never returned (invariant 5).
    pub fn for_character(&self, character: &str, profile: DeviceProfile) -> Option<Calibration> {
        self.good.get(&(character.to_owned(), profile_key(profile))).map(|calibration| **calibration)
    }

    /// Saves `calibration` for `character` (under the device class it was made for), replacing the one before, and
    /// writes the file when the store has one.
    ///
    /// # Errors
    /// [`CalibrationError`] for an id that is not 1 to [`MAX_ID_CHARS`] characters, a calibration with a value that is
    /// not usable, a new character past [`MAX_CHARACTERS`], a locked store or a failed write; the store is unchanged
    /// then.
    pub fn save(&mut self, character: &str, calibration: Calibration) -> Result<(), CalibrationError> {
        self.refuse_if_locked()?;
        check_id(character)?;
        let profile = profile_key(calibration.profile);
        check_calibration(&profile, &calibration)?;
        if !self.good.keys().any(|(c, _)| c == character) && self.characters() >= MAX_CHARACTERS {
            return Err(CalibrationError::TooMany { count: self.characters() + 1 });
        }
        let key = (character.to_owned(), profile);
        self.changed(|store| {
            // The person's own recalibration replaces an unreadable entry filed under the same key.
            store.kept.retain(|kept| kept.key.as_ref() != Some(&key));
            store.findings.retain(|f| (f.character.as_ref(), f.profile.as_ref()) != (Some(&key.0), Some(&key.1)));
            store.good.insert(key, Box::new(calibration));
        })
    }

    /// Forgets what is saved for `character` on `profile` only, an unreadable entry filed under that key included,
    /// and writes the file. Another device class of the same character, and every other character, stay. Returns
    /// whether there was anything.
    ///
    /// # Errors
    /// [`CalibrationError::Locked`] or [`CalibrationError::Write`]; the store is unchanged then.
    pub fn forget(&mut self, character: &str, profile: DeviceProfile) -> Result<bool, CalibrationError> {
        let key = (character.to_owned(), profile_key(profile));
        self.forget_where(|(c, p)| (c, p) == (&key.0, &key.1))
    }

    /// Forgets everything saved for `character` on every device class, unreadable entries included, and writes the
    /// file. This is the deliberate "forget all for this character"; [`forget`](Self::forget) takes one device.
    /// Returns whether there was anything.
    ///
    /// # Errors
    /// [`CalibrationError::Locked`] or [`CalibrationError::Write`]; the store is unchanged then.
    pub fn forget_all_devices(&mut self, character: &str) -> Result<bool, CalibrationError> {
        self.forget_where(|(c, _)| c == character)
    }

    /// Drops every usable and kept entry whose key `matches`, and writes. Findings go with their entries.
    fn forget_where(&mut self, matches: impl Fn((&String, &String)) -> bool) -> Result<bool, CalibrationError> {
        self.refuse_if_locked()?;
        let had = self.good.keys().any(|(c, p)| matches((c, p)))
            || self.kept.iter().any(|kept| kept.key.as_ref().is_some_and(|(c, p)| matches((c, p))));
        if had {
            self.changed(|store| {
                store.good.retain(|(c, p), _| !matches((c, p)));
                store.kept.retain(|kept| kept.key.as_ref().is_none_or(|(c, p)| !matches((c, p))));
                store.findings.retain(|f| match (&f.character, &f.profile) {
                    (Some(c), Some(p)) => !matches((c, p)),
                    _ => true,
                });
            })?;
        }
        Ok(had)
    }

    /// How many characters have a calibration that can be used.
    pub fn len(&self) -> usize {
        self.characters()
    }

    /// Whether no calibration can be used.
    pub fn is_empty(&self) -> bool {
        self.good.is_empty()
    }

    fn characters(&self) -> usize {
        self.good.keys().map(|(character, _)| character).collect::<std::collections::BTreeSet<_>>().len()
    }

    fn refuse_if_locked(&self) -> Result<(), CalibrationError> {
        self.locked.clone().map_or(Ok(()), |reason| Err(CalibrationError::Locked { reason }))
    }

    /// Applies `edit`, then writes; when the write fails the store goes back to what it was (invariant 6).
    fn changed(&mut self, edit: impl FnOnce(&mut Self)) -> Result<(), CalibrationError> {
        let before = self.clone();
        edit(self);
        match self.write() {
            Ok(()) => Ok(()),
            Err(error) => {
                *self = before;
                Err(error)
            }
        }
    }

    /// Writes the file: the usable entries in order, then the kept ones as they were read.
    fn write(&self) -> Result<(), CalibrationError> {
        let Some(path) = &self.path else { return Ok(()) };
        let write_error = |reason: String| CalibrationError::Write { reason };
        let mut items: Vec<toml::Value> = Vec::with_capacity(self.good.len() + self.kept.len());
        for ((character, profile), calibration) in &self.good {
            let mut value = serde_json::to_value(**calibration).map_err(|e| write_error(e.to_string()))?;
            strip_nulls(&mut value);
            let body = toml::Value::try_from(value).map_err(|e| write_error(e.to_string()))?;
            let mut element = toml::Table::new();
            element.insert("character".to_owned(), toml::Value::String(character.clone()));
            element.insert("profile".to_owned(), toml::Value::String(profile.clone()));
            element.insert("calibration".to_owned(), body);
            items.push(toml::Value::Table(element));
        }
        items.extend(self.kept.iter().map(|kept| kept.raw.clone()));
        let mut root = toml::Table::new();
        root.insert("version".to_owned(), toml::Value::Integer(FILE_VERSION));
        root.extend(self.extra.clone());
        if !items.is_empty() {
            root.insert("calibration".to_owned(), toml::Value::Array(items));
        }
        let text = toml::to_string(&root).map_err(|e| write_error(e.to_string()))?;
        write_atomic(path, &text).map_err(|e| write_error(e.kind().to_string()))
    }
}

/// Reads one element: its key when it has one, and the calibration or the reason it cannot be used.
fn decode(item: &toml::Value) -> (Option<Key>, Result<Calibration, String>) {
    let Some(table) = item.as_table() else {
        return (None, Err("the entry is not a table".to_owned()));
    };
    let text = |name: &str| table.get(name).and_then(toml::Value::as_str).map(str::to_owned);
    let key = text("character").zip(text("profile"));
    let Some((character, profile)) = key.clone() else {
        return (key, Err("the entry needs a `character` and a `profile`, both text".to_owned()));
    };
    if let Err(error) = check_id(&character) {
        return (key, Err(error.reason()));
    }
    let Some(body) = table.get("calibration") else {
        return (key, Err("the entry has no `calibration`".to_owned()));
    };
    // `try_into` reports a serde message that names the field but never quotes the value.
    let Ok(calibration) = body.clone().try_into::<Calibration>() else {
        return (key, Err("the calibration does not read".to_owned()));
    };
    match check_calibration(&profile, &calibration) {
        Ok(()) => (key, Ok(calibration)),
        Err(error) => (key, Err(error.reason())),
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

fn check_id(character: &str) -> Result<(), CalibrationError> {
    let chars = character.chars().count();
    if (1..=MAX_ID_CHARS).contains(&chars) { Ok(()) } else { Err(CalibrationError::BadId) }
}

/// The name a device class is filed under: its serialised name (`webcam`, `arkit`).
fn profile_key(profile: DeviceProfile) -> String {
    serde_json::to_value(profile).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

fn check_calibration(filed: &str, calibration: &Calibration) -> Result<(), CalibrationError> {
    let made = profile_key(calibration.profile);
    if made != filed {
        return Err(CalibrationError::WrongProfile { filed: filed.to_owned(), made });
    }
    let unusable = |what: String| CalibrationError::Unusable { what };
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

    /// A store on a file in a fresh folder, and the file's path.
    fn on_disk() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = calibration_file(dir.path());
        (dir, path)
    }

    /// The file's text with one more element (given as TOML) appended to its `[[calibration]]` list.
    fn with_element(path: &Path, element: &str) {
        let mut text = std::fs::read_to_string(path).unwrap_or_else(|_| "version = 1\n".to_owned());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        text.push_str("\n[[calibration]]\n");
        text.push_str(element);
        std::fs::write(path, text).unwrap();
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
    fn the_file_round_trips_bit_for_bit_and_holds_no_null() {
        let (_dir, path) = on_disk();
        let mut store = CalibrationStore::open(&path);
        assert!(store.is_empty() && store.findings().is_empty(), "a missing file is an empty store");
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        store.save("kiri", Calibration::identity(DeviceProfile::Arkit)).unwrap(); // no head position: nothing to write
        store.save("mira", tuned(DeviceProfile::Webcam)).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("version = 1"), "{text}");
        assert!(!text.contains("null"), "{text}");
        let reopened = CalibrationStore::open(&path);
        assert_eq!(reopened, store);
        assert_eq!(reopened.for_character("kiri", DeviceProfile::Webcam), Some(tuned(DeviceProfile::Webcam)));
        assert_eq!(reopened.findings(), []);
    }

    #[test]
    fn a_saved_calibration_replaces_the_one_before() {
        let mut store = CalibrationStore::new();
        store.save("kiri", Calibration::identity(DeviceProfile::Webcam)).unwrap();
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.for_character("kiri", DeviceProfile::Webcam), Some(tuned(DeviceProfile::Webcam)));
        assert_eq!(store.forget("kiri", DeviceProfile::Webcam), Ok(true));
        assert!(store.is_empty());
        assert_eq!(store.forget("kiri", DeviceProfile::Webcam), Ok(false));
    }

    #[test]
    fn forgetting_one_device_keeps_the_other_and_other_characters_across_a_restart() {
        let (_dir, path) = on_disk();
        let mut store = CalibrationStore::open(&path);
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        store.save("kiri", tuned(DeviceProfile::Arkit)).unwrap();
        store.save("mira", tuned(DeviceProfile::Webcam)).unwrap();
        assert_eq!(store.forget("kiri", DeviceProfile::Webcam), Ok(true));
        assert_eq!(store.forget("kiri", DeviceProfile::Webcam), Ok(false), "already gone");
        let again = CalibrationStore::open(&path);
        assert_eq!(again.for_character("kiri", DeviceProfile::Webcam), None);
        assert_eq!(again.for_character("kiri", DeviceProfile::Arkit), Some(tuned(DeviceProfile::Arkit)));
        assert_eq!(again.for_character("mira", DeviceProfile::Webcam), Some(tuned(DeviceProfile::Webcam)));
        let mut again = again;
        assert_eq!(again.forget_all_devices("kiri"), Ok(true));
        let last = CalibrationStore::open(&path);
        assert_eq!(last.for_character("kiri", DeviceProfile::Arkit), None);
        assert_eq!(last.for_character("mira", DeviceProfile::Webcam), Some(tuned(DeviceProfile::Webcam)));
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

    #[test]
    fn bad_elements_of_every_shape_cost_the_good_ones_nothing_and_are_kept() {
        let (_dir, path) = on_disk();
        let mut store = CalibrationStore::open(&path);
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        // An entry whose calibration is the wrong type, one with garbage inside, one with no profile, and an element
        // with nothing in it.
        with_element(&path, "character = \"mira\"\nprofile = \"webcam\"\ncalibration = \"garbage\"\n");
        with_element(
            &path,
            "character = \"noor\"\nprofile = \"webcam\"\n[calibration.calibration]\nprofile = \"webcam\"\nranges = \"garbage\"\n",
        );
        with_element(&path, "character = \"ola\"\n");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("\n[[calibration]]\n");
        std::fs::write(&path, text).unwrap();

        let loaded = CalibrationStore::open(&path);
        assert_eq!(loaded.for_character("kiri", DeviceProfile::Webcam), Some(tuned(DeviceProfile::Webcam)));
        assert_eq!(loaded.locked(), None, "bad elements do not lock the file");
        let named: Vec<_> = loaded.findings().iter().map(|f| f.character.as_deref()).collect();
        assert_eq!(named, [Some("mira"), Some("noor"), None, None]);
        let all = format!("{:?}", loaded.findings());
        assert!(!all.contains("garbage"), "a finding never quotes a stored value: {all}");

        // A save for another character keeps every bad element as it was.
        let mut loaded = loaded;
        loaded.save("pia", tuned(DeviceProfile::Webcam)).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written.matches("garbage").count(), 2, "{written}");
        let again = CalibrationStore::open(&path);
        assert_eq!(again.findings().len(), 4);
        assert!(again.for_character("kiri", DeviceProfile::Webcam).is_some());
        assert!(again.for_character("pia", DeviceProfile::Webcam).is_some());
    }

    #[test]
    fn recalibrating_a_character_replaces_its_unreadable_entry_and_forgetting_drops_it() {
        let (_dir, path) = on_disk();
        with_element(&path, "character = \"mira\"\nprofile = \"webcam\"\ncalibration = \"garbage\"\n");
        with_element(&path, "character = \"noor\"\nprofile = \"webcam\"\ncalibration = \"garbage\"\n");
        let mut store = CalibrationStore::open(&path);
        assert_eq!(store.findings().len(), 2);
        store.save("mira", tuned(DeviceProfile::Webcam)).unwrap();
        assert_eq!(store.findings().len(), 1, "the person's own fix clears it");
        assert_eq!(store.forget("noor", DeviceProfile::Webcam), Ok(true), "forgetting drops a bad entry too");
        let again = CalibrationStore::open(&path);
        assert_eq!(again.findings(), []);
        assert_eq!(again.len(), 1);
    }

    #[test]
    fn a_file_that_cannot_be_read_at_all_is_never_written_over() {
        for (what, text) in [
            ("does not parse", "this is [not toml"),
            ("a version this build does not know", "version = 2\n[[calibration]]\ncharacter = \"kiri\"\n"),
            ("is not a list of tables", "version = 1\ncalibration = \"garbage\"\n"),
        ] {
            let (_dir, path) = on_disk();
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, text).unwrap();
            let mut store = CalibrationStore::open(&path);
            assert!(store.is_empty());
            assert!(store.locked().is_some(), "{what}");
            assert_eq!(store.findings().len(), 1, "{what}");
            assert!(store.findings()[0].character.is_none());
            assert!(!format!("{:?}", store.findings()).contains("garbage"), "{what}");
            assert!(
                matches!(store.save("kiri", tuned(DeviceProfile::Webcam)), Err(CalibrationError::Locked { .. })),
                "{what}"
            );
            assert!(
                matches!(store.forget("kiri", DeviceProfile::Webcam), Err(CalibrationError::Locked { .. })),
                "{what}"
            );
            assert!(matches!(store.forget_all_devices("kiri"), Err(CalibrationError::Locked { .. })), "{what}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "{what}: the file is as it was");
        }
    }

    #[test]
    fn keys_this_build_does_not_know_are_written_back() {
        let (_dir, path) = on_disk();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "version = 1\nnote = \"from a newer build\"\n").unwrap();
        let mut store = CalibrationStore::open(&path);
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("from a newer build"));
    }

    #[test]
    fn a_failed_write_leaves_the_store_and_the_file_as_they_were() {
        let (dir, path) = on_disk();
        let mut store = CalibrationStore::open(&path);
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        let before = store.clone();
        // The folder the file lives in is replaced by a file, so the write cannot succeed.
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(dir.path().join("perform"), "in the way").unwrap();
        assert!(matches!(store.save("mira", tuned(DeviceProfile::Webcam)), Err(CalibrationError::Write { .. })));
        assert_eq!(store, before, "memory still matches what was last written");
        assert_eq!(store.for_character("mira", DeviceProfile::Webcam), None);
    }

    #[test]
    fn an_entry_filed_under_the_wrong_device_class_is_kept_and_not_used() {
        let (_dir, path) = on_disk();
        let mut store = CalibrationStore::open(&path);
        store.save("kiri", tuned(DeviceProfile::Webcam)).unwrap();
        let text =
            std::fs::read_to_string(&path).unwrap().replacen("profile = \"webcam\"\n", "profile = \"arkit\"\n", 1);
        std::fs::write(&path, text).unwrap();
        let moved = CalibrationStore::open(&path);
        assert_eq!(moved.for_character("kiri", DeviceProfile::Arkit), None);
        assert_eq!(moved.for_character("kiri", DeviceProfile::Webcam), None);
        assert_eq!(moved.findings().len(), 1);
    }
}
