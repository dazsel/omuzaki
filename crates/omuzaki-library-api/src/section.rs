//! Sections, kinds and targets of items (design D1).

use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Which node an item applies to, and where its application record goes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// `characters/<id>`. The only target that applies in phase 1.
    Character,
    /// `scenes/<id>`. In the schema but refused until the ST-01 scene contract lands.
    Scene,
    /// `shows/<id>` (automation templates). Reserved until the show contract, phase 2.
    Show,
    /// `performances/<id>` (score templates). Reserved until the performance contract, phase 4.
    Performance,
}

impl Target {
    /// Why this target can't be used yet, or `None` if it can. `ItemManifest::check` reports it on `target`.
    pub const fn reserved(self) -> Option<&'static str> {
        match self {
            Self::Character => None,
            Self::Scene => Some("the scene target is reserved until the ST-01 scene contract"),
            Self::Show => Some("the show target is reserved until the show contract"),
            Self::Performance => Some("the performance target is reserved until the performance contract"),
        }
    }

    /// The top-level document collection holding nodes of this target (`characters`).
    pub const fn collection(self) -> &'static str {
        match self {
            Self::Character => "characters",
            Self::Scene => "scenes",
            Self::Show => "shows",
            Self::Performance => "performances",
        }
    }
}

/// What applying an item does (design D1). Validation holds the recipe to it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// Creates the parts it declares in `creates`: a hairstyle, a garment, an accessory.
    Part,
    /// Sets values on things that already exist and creates no part: a face profile, a colourway, a light rig.
    Setting,
    /// Adds one named member to a collection: an expression, a pose, an emote.
    Member,
    /// Applies other items (item steps) and may add steps of its own: an outfit, an emote pack.
    Bundle,
}

/// Declares [`Section`] with its slug and target in one table, so the three never drift apart.
macro_rules! sections {
    ($($variant:ident => $slug:literal, $target:ident;)+) => {
        /// The browsing group of an item. The list is fixed here, in the seed table's order, so a new section is an
        /// API change and a new item never is (design D1). The derived order is that table order.
        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
        pub enum Section {
            $(
                #[doc = concat!("`", $slug, "`, applies to a ", stringify!($target), ".")]
                #[serde(rename = $slug)]
                $variant,
            )+
        }

        impl Section {
            /// Every section, in the design's order.
            pub const ALL: &'static [Section] = &[$(Section::$variant),+];

            /// The section's slug, as written in manifests (`hair`, `body-shapes`).
            pub const fn name(self) -> &'static str {
                match self { $(Section::$variant => $slug),+ }
            }

            /// The target items of this section apply to.
            pub const fn target(self) -> Target {
                match self { $(Section::$variant => Target::$target),+ }
            }
        }
    };
}

sections! {
    BodyTemplates => "body-templates", Character;
    BodyShapes => "body-shapes", Character;
    SkinTones => "skin-tones", Character;
    Creatures => "creatures", Character;
    FaceProfiles => "face-profiles", Character;
    Eyes => "eyes", Character;
    Brows => "brows", Character;
    Mouths => "mouths", Character;
    Hair => "hair", Character;
    Bangs => "bangs", Character;
    Garments => "garments", Character;
    Outfits => "outfits", Character;
    Trims => "trims", Character;
    Accessories => "accessories", Character;
    Colourways => "colourways", Character;
    Looks => "looks", Character;
    LightRigs => "light-rigs", Scene;
    Poses => "poses", Character;
    Expressions => "expressions", Character;
    Emotes => "emotes", Character;
    Personalities => "personalities", Character;
    Stages => "stages", Scene;
    CameraMoves => "camera-moves", Scene;
    Automations => "automations", Show;
    Scores => "scores", Performance;
}

impl fmt::Display for Section {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// An unknown section name, with the nearest known one when there is one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown section {name:?}{}", .did_you_mean.map(|s| format!(" (did you mean {s:?}?)")).unwrap_or_default())]
pub struct UnknownSection {
    /// The name as written.
    pub name: String,
    /// The closest section name, if one is within a few edits.
    pub did_you_mean: Option<&'static str>,
}

impl FromStr for Section {
    type Err = UnknownSection;
    fn from_str(s: &str) -> Result<Self, UnknownSection> {
        Section::ALL.iter().copied().find(|sec| sec.name() == s).ok_or_else(|| UnknownSection {
            name: s.to_owned(),
            did_you_mean: omuzaki_core::text::nearest(s, Section::ALL.iter().map(|sec| sec.name())),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn there_are_25_sections_in_the_designs_order() {
        let names: Vec<&str> = Section::ALL.iter().map(|s| s.name()).collect();
        assert_eq!(names.len(), 25);
        assert_eq!(&names[..4], ["body-templates", "body-shapes", "skin-tones", "creatures"]);
        assert_eq!(&names[21..], ["stages", "camera-moves", "automations", "scores"]);
        assert!(Section::ALL.windows(2).all(|w| w[0] < w[1]), "derived order is table order");
    }

    #[test]
    fn names_round_trip_through_from_str_and_serde() {
        for &s in Section::ALL {
            assert_eq!(s.name().parse::<Section>().unwrap(), s);
            assert_eq!(serde_json::to_string(&s).unwrap(), format!("\"{}\"", s.name()));
        }
    }

    #[test]
    fn targets_follow_the_design_table() {
        assert_eq!(Section::Hair.target(), Target::Character);
        assert_eq!(Section::LightRigs.target(), Target::Scene);
        assert_eq!(Section::Stages.target(), Target::Scene);
        assert_eq!(Section::Automations.target(), Target::Show);
        assert_eq!(Section::Scores.target(), Target::Performance);
        assert_eq!(Section::ALL.iter().filter(|s| s.target() == Target::Character).count(), 20);
    }

    #[test]
    fn an_unknown_section_suggests_the_nearest() {
        let err = "hairs".parse::<Section>().unwrap_err();
        assert_eq!(err.did_you_mean, Some("hair"));
        assert!(err.to_string().contains("did you mean \"hair\""), "{err}");
    }

    #[test]
    fn only_the_character_target_is_open() {
        assert_eq!(Target::Character.reserved(), None);
        assert_eq!(Target::Scene.reserved(), Some("the scene target is reserved until the ST-01 scene contract"));
        assert!(Target::Show.reserved().is_some() && Target::Performance.reserved().is_some());
    }
}
