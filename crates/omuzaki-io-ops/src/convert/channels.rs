//! Animation channels and the expression names a file carries them under.
//!
//! The runtime drives a face through [`Channel`]s (`omuzaki-anim-api`); a VRM carries them as custom expressions
//! (`docs/design/io-api.md`, D6):
//!
//! - **The 52 `ARKit` blendshapes** keep Apple's camelCase name (`jawOpen`), so a face tracker drives an exported model
//!   directly. The channel table already uses those names, which makes this half an identity on names; what needs a
//!   table is the *order*, because the file side lists them in [`ARKIT_52`] order (eyes, jaw, mouth, brows, ...) and
//!   the runtime indexes them alphabetically. The tests pin the two lists to each other.
//! - **The anime extras** (FE-02: `^^` eyes, heart eyes, sweat drop, ...) export as `omuzaki_<id>` so they cannot collide
//!   with a tracker's names ([`EXTRAS`]). The id is the channel name in kebab case.
//!
//! Head and body channels (`headYaw`, `breath`, ...) and the `customNN` slots are not expressions and have no name here.
//! The VRM presets (`happy`, `aa`, ...) are not channels either; the face baker derives them from the channels.
//!
//! A character's own named expressions (`smug`, a second `happy`) export under [`named_expression`]: the VRM preset of
//! the same name, else `omuzaki_<id>`. The VRM and VRMA exporters both name expressions through it, so a VRMA track
//! for an expression targets the expression the VRM carries.

use omuzaki_anim_api::{Channel, ExpressionId};
use omuzaki_io_api::{ARKIT_52, EXTRA_PREFIX, ExpressionName, PresetExpression, extra_expression};

/// The anime extras and the id each is exported under (`omuzaki_<id>`). Together with the 52 `ARKit` channels these are
/// every channel that becomes an expression.
pub const EXTRAS: [(Channel, &str); 13] = [
    (Channel::EYE_ARC_LEFT, "eye-arc-left"),
    (Channel::EYE_ARC_RIGHT, "eye-arc-right"),
    (Channel::EYES_HEART, "eyes-heart"),
    (Channel::EYES_STAR, "eyes-star"),
    (Channel::EYES_GLOOM, "eyes-gloom"),
    (Channel::EYES_SHOCK, "eyes-shock"),
    (Channel::EYES_SQUEEZE, "eyes-squeeze"),
    (Channel::EYES_SPIRAL, "eyes-spiral"),
    (Channel::SPARKLE, "sparkle"),
    (Channel::TEARS, "tears"),
    (Channel::SWEAT_DROP, "sweat-drop"),
    (Channel::BLUSH, "blush"),
    (Channel::FANGS, "fangs"),
];

/// The expression `channel` is exported as, or `None` for channels that are not expressions (head and body motion, the
/// `customNN` slots).
pub fn expression_name(channel: Channel) -> Option<ExpressionName> {
    if channel.is_arkit() {
        let name = ARKIT_52.iter().copied().find(|name| *name == channel.name())?;
        return ExpressionName::parse(name).ok();
    }
    let (_, id) = EXTRAS.iter().find(|(extra, _)| *extra == channel)?;
    extra_expression(id).ok()
}

/// The name a character's expression `id` is exported under: the VRM preset when `id` is a preset's name in an id's
/// kebab case (`happy`, `blink-left` for `blinkLeft`), else `omuzaki_<id>` (`omuzaki_smug`). `None` only for an id a
/// file can't carry as a name.
pub fn named_expression(id: &ExpressionId) -> Option<ExpressionName> {
    match PresetExpression::ALL.iter().find(|preset| kebab_eq(preset.as_str(), id.as_str())) {
        Some(preset) => Some(ExpressionName::Preset(*preset)),
        None => extra_expression(id.as_str()).ok(),
    }
}

/// Whether the camelCase `camel` is `kebab` in kebab case.
fn kebab_eq(camel: &str, kebab: &str) -> bool {
    let mut rest = kebab.chars();
    camel.chars().all(|c| {
        if c.is_ascii_uppercase() {
            rest.next() == Some('-') && rest.next() == Some(c.to_ascii_lowercase())
        } else {
            rest.next() == Some(c)
        }
    }) && rest.next().is_none()
}

/// The channel an expression named `name` carries, or `None` if it is a preset, a model's own expression or an
/// unknown `omuzaki_` extra. Names are case-sensitive.
pub fn channel_for(name: &str) -> Option<Channel> {
    if let Some(id) = name.strip_prefix(EXTRA_PREFIX) {
        return EXTRAS.iter().find(|(_, extra)| *extra == id).map(|(channel, _)| *channel);
    }
    // `Channel::from_name` also knows `headYaw` or `blush`; only the `ARKit` names are exported unprefixed.
    Channel::from_name(name).filter(|channel| channel.is_arkit() && ARKIT_52.contains(&channel.name()))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use omuzaki_anim_api::CHANNELS;
    use omuzaki_core::id::Slug;
    use omuzaki_io_api::PresetExpression;

    use super::*;

    fn kebab(camel: &str) -> String {
        camel.chars().fold(String::new(), |mut out, c| {
            if c.is_ascii_uppercase() {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            out
        })
    }

    #[test]
    fn the_arkit_table_covers_the_file_list_exactly() {
        let runtime: BTreeSet<&str> = Channel::arkit().iter().map(|info| info.name).collect();
        let file: BTreeSet<&str> = ARKIT_52.iter().copied().collect();
        assert_eq!(runtime.len(), 52);
        assert_eq!(runtime, file, "the runtime's 52 ARKit channels and the file's 52 names are the same set");
    }

    #[test]
    fn the_arkit_names_map_one_to_one_onto_the_arkit_channels() {
        let mut channels = BTreeSet::new();
        for name in ARKIT_52 {
            let channel = channel_for(name).unwrap_or_else(|| panic!("{name} has no channel"));
            assert!(channel.is_arkit(), "{name}");
            assert_eq!(expression_name(channel).unwrap().as_str(), name);
            assert!(channels.insert(channel), "{name} shares a channel with another name");
        }
        assert_eq!(channels.len(), Channel::arkit().len(), "no ARKit channel is left without a name");
    }

    #[test]
    fn the_extras_cover_the_anime_channels_exactly() {
        let table: BTreeSet<Channel> = EXTRAS.iter().map(|(channel, _)| *channel).collect();
        let first = Channel::EYE_ARC_LEFT.index();
        let last = Channel::FANGS.index();
        let assigned: BTreeSet<Channel> = (first..=last).filter_map(Channel::from_index).collect();
        assert_eq!(assigned.len(), 13, "channels 60 to 72 are all assigned");
        assert_eq!(table, assigned);
    }

    #[test]
    fn extras_are_prefixed_kebab_case_ids_that_cannot_meet_a_tracker_name() {
        let mut ids = BTreeSet::new();
        for (channel, id) in EXTRAS {
            assert_eq!(id, kebab(channel.name()), "{channel}");
            assert!(Slug::new(id).is_ok(), "{id} is a slug, as extra_expression expects");
            assert!(ids.insert(id), "{id} is used twice");
            let name = expression_name(channel).unwrap();
            assert_eq!(name.as_str(), format!("omuzaki_{id}"));
            assert!(!ARKIT_52.contains(&name.as_str()));
            assert!(matches!(name, ExpressionName::Custom(_)), "{name} must not be a preset");
            assert_eq!(channel_for(name.as_str()), Some(channel));
        }
    }

    #[test]
    fn only_expression_channels_have_names_and_names_never_repeat() {
        let mut names = BTreeSet::new();
        for info in CHANNELS {
            match expression_name(info.channel) {
                Some(name) => assert!(names.insert(name.to_string()), "{name} is used twice"),
                None => assert!(
                    !info.channel.is_arkit() && !EXTRAS.iter().any(|(extra, _)| *extra == info.channel),
                    "{} should have a name",
                    info.name
                ),
            }
        }
        assert_eq!(names.len(), 52 + 13);
        for not_expressions in [Channel::HEAD_YAW, Channel::BREATH, Channel::CUSTOM_00, Channel::CUSTOM_15] {
            assert_eq!(expression_name(not_expressions), None, "{not_expressions}");
        }
    }

    #[test]
    fn names_that_are_not_exported_channels_are_refused() {
        for name in [
            "happy",
            "blink",
            "headYaw",
            "breath",
            "custom00",
            "blush",
            "JawOpen",
            "jawopen",
            "omuzaki_",
            "omuzaki_unknown",
            "omuzaki_jawOpen",
            "omuzaki_Blush",
            "",
        ] {
            assert_eq!(channel_for(name), None, "{name:?}");
        }
        assert!(matches!(ExpressionName::parse("happy"), Ok(ExpressionName::Preset(PresetExpression::Happy))));
    }

    #[test]
    fn a_named_expression_is_its_preset_or_omuzaki_prefixed() {
        let id = |s: &str| omuzaki_anim_api::ExpressionId::new(s).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(named_expression(&id("happy")), Some(ExpressionName::Preset(PresetExpression::Happy)));
        assert_eq!(named_expression(&id("aa")), Some(ExpressionName::Preset(PresetExpression::Aa)));
        assert_eq!(named_expression(&id("smug")).map(|n| n.to_string()), Some("omuzaki_smug".to_owned()));
        assert_eq!(named_expression(&id("blink-left")), Some(ExpressionName::Preset(PresetExpression::BlinkLeft)));
        assert_eq!(named_expression(&id("blinkleft")).map(|n| n.to_string()), Some("omuzaki_blinkleft".to_owned()));
        // Every preset whose name an expression can have maps back to that preset.
        for preset in PresetExpression::ALL {
            if let Ok(own) = omuzaki_anim_api::ExpressionId::new(preset.as_str()) {
                assert_eq!(named_expression(&own), Some(ExpressionName::Preset(preset)), "{preset:?}");
            }
        }
    }
}
