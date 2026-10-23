//! Who drives a cast member (`docs/design/drivers.md`): for each group of the character's channels, which drivers may
//! move it and how strongly. Stored on the member, `cast/<member>.toml` under `[drive]`.
//!
//! A **driver** is anything that moves a character live. They come in four kinds, one per layer of the face's blend
//! (anim-api `LayerStack`), so a binding never invents a layer:
//!
//! | Kind | What feeds it |
//! |---|---|
//! | `score` | agents, scripts, automations and the AI performer, through the `act.*` ops; a recorded take |
//! | `tracker` | a webcam, a phone (`ARKit`), VMC, a replayed capture |
//! | `voice` | the microphone or a synthesised voice: the mouth's lip sync |
//! | `keys` | hotkeys, the trigger strip, redeems: `trigger.press` |
//!
//! A **group** is one of the channel regions the blend already asks ownership questions about. A [`DriveMap`] gives
//! each group a weight per kind, `0` (never drives it) to `1` (full strength, the default). A group or a kind the map
//! does not name drives at full strength, so an empty map is today's behaviour: every driver everywhere, the higher
//! layer winning (`keys` over `voice` over `tracker` over `score`).
//!
//! ```toml
//! [drive]
//! gaze = { tracker = 0.0 }            # the eyes look where an agent or a script says, never the webcam
//! head = { tracker = 0.5 }            # the webcam moves the head half as much
//! ```

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A group of channels a binding is about: the regions of anim-api's channel table, by the same names.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriveGroup {
    /// Brows, and the nose sneer that pulls them down.
    Brows,
    /// Lids: blink, squint, wide, and the eye-style extras.
    Eyes,
    /// Where the eyes look.
    Gaze,
    /// Jaw, lips, cheeks and tongue.
    Mouth,
    /// Head angles and shift.
    Head,
    /// Lean, body turn and breath.
    Body,
    /// Anime extras that are not eye styles: sparkle, tears, sweat, blush, fangs.
    Extras,
    /// The character's own custom channels.
    Custom,
}

impl DriveGroup {
    /// Every group, in order.
    pub const ALL: [Self; 8] =
        [Self::Brows, Self::Eyes, Self::Gaze, Self::Mouth, Self::Head, Self::Body, Self::Extras, Self::Custom];

    /// Its name, as the map spells it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Brows => "brows",
            Self::Eyes => "eyes",
            Self::Gaze => "gaze",
            Self::Mouth => "mouth",
            Self::Head => "head",
            Self::Body => "body",
            Self::Extras => "extras",
            Self::Custom => "custom",
        }
    }
}

/// A kind of driver: one per layer of the face's blend, lowest first.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DriverKind {
    /// Agents, scripts, automations, the AI performer and recorded takes (the `act.*` ops).
    Score,
    /// A face tracker: webcam, phone, VMC or a replayed capture.
    Tracker,
    /// Lip sync from a voice.
    Voice,
    /// Hotkeys and triggers.
    Keys,
}

impl DriverKind {
    /// Every kind, lowest layer first.
    pub const ALL: [Self; 4] = [Self::Score, Self::Tracker, Self::Voice, Self::Keys];

    /// Its name, as the map spells it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Score => "score",
            Self::Tracker => "tracker",
            Self::Voice => "voice",
            Self::Keys => "keys",
        }
    }
}

/// Which drivers move each group of a member's channels, and how strongly. See the module docs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct DriveMap(pub BTreeMap<DriveGroup, BTreeMap<DriverKind, f32>>);

impl DriveMap {
    /// Every driver at full strength everywhere.
    #[must_use]
    pub fn open() -> Self {
        Self::default()
    }

    /// The weight `driver` has on `group`: what the map says, or `1`.
    pub fn weight(&self, group: DriveGroup, driver: DriverKind) -> f32 {
        self.0.get(&group).and_then(|kinds| kinds.get(&driver)).copied().unwrap_or(1.0)
    }

    /// Sets the weight of `driver` on `group`, clamped to `0..=1` (a non-finite weight counts as `1`). A weight of `1`
    /// is the default, so it is removed from the map rather than written.
    pub fn set(&mut self, group: DriveGroup, driver: DriverKind, weight: f32) {
        let weight = if weight.is_finite() { weight.clamp(0.0, 1.0) } else { 1.0 };
        if weight >= 1.0 {
            if let Some(kinds) = self.0.get_mut(&group) {
                kinds.remove(&driver);
                if kinds.is_empty() {
                    self.0.remove(&group);
                }
            }
        } else {
            self.0.entry(group).or_default().insert(driver, weight);
        }
    }

    /// Whether every driver drives everything at full strength (nothing to store).
    pub fn is_open(&self) -> bool {
        self.0.values().all(|kinds| kinds.values().all(|w| *w >= 1.0))
    }

    /// The first weight that is not a number in `0..=1`, as `(group, driver, weight)`.
    pub fn invalid(&self) -> Option<(DriveGroup, DriverKind, f32)> {
        self.0.iter().find_map(|(group, kinds)| {
            kinds.iter().find(|(_, w)| !(w.is_finite() && (0.0..=1.0).contains(*w))).map(|(k, w)| (*group, *k, *w))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_map_drives_everything_and_reads_from_the_names_people_write() {
        let open = DriveMap::open();
        assert!(open.is_open());
        assert!((open.weight(DriveGroup::Gaze, DriverKind::Tracker) - 1.0).abs() < f32::EPSILON);
        let map: DriveMap = toml::from_str("gaze = { tracker = 0.0 }\nhead = { tracker = 0.5, score = 1.0 }").unwrap();
        assert!(map.weight(DriveGroup::Gaze, DriverKind::Tracker).abs() < f32::EPSILON);
        assert!((map.weight(DriveGroup::Head, DriverKind::Tracker) - 0.5).abs() < f32::EPSILON);
        assert!((map.weight(DriveGroup::Mouth, DriverKind::Voice) - 1.0).abs() < f32::EPSILON);
        assert!(!map.is_open());
        assert!(toml::from_str::<DriveMap>("nose = { tracker = 0.0 }").is_err(), "an unknown group is refused");
        assert!(toml::from_str::<DriveMap>("gaze = { webcam = 0.0 }").is_err(), "an unknown driver is refused");
    }

    #[test]
    fn setting_full_strength_removes_the_entry_and_weights_are_clamped() {
        let mut map = DriveMap::open();
        map.set(DriveGroup::Gaze, DriverKind::Tracker, -2.0);
        assert!(map.weight(DriveGroup::Gaze, DriverKind::Tracker).abs() < f32::EPSILON);
        map.set(DriveGroup::Gaze, DriverKind::Tracker, 1.0);
        assert_eq!(map, DriveMap::open(), "back to the default leaves nothing behind");
        map.set(DriveGroup::Head, DriverKind::Score, f32::NAN);
        assert_eq!(map, DriveMap::open());
        let bad = DriveMap(BTreeMap::from([(DriveGroup::Head, BTreeMap::from([(DriverKind::Score, 1.5)]))]));
        assert_eq!(bad.invalid(), Some((DriveGroup::Head, DriverKind::Score, 1.5)));
    }
}
