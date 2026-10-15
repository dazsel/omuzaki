//! `AvatarDef`: the D5 node reads from TOML, round-trips, refuses unknown keys everywhere, and checks its schema.

use omuzaki_anim_api::{Channel, HumanoidBone};
use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, DriverChoice, MaterialEdit, NodeChoice, SCHEMA, SourceFormat};
use omuzaki_avatar_api::error::AvatarError;
use omuzaki_char_api::{MaterialName, PartId};
use omuzaki_core::glam::Vec4;
use omuzaki_io_api::ColorBindKind;
use omuzaki_store_api::ContentHash;

const HASH: &str = "0707070707070707070707070707070707070707070707070707070707070707";

/// The node of design D5, with an attachment.
fn stored() -> String {
    format!(
        r#"
schema = 1
source = {{ hash = "{HASH}", format = "vrm1", file = "Ava.vrm" }}
humanoid = {{ leftHand = {{ node = 41, name = "J_Bip_L_Hand" }} }}
channels = {{ cheekPuff = "Puff", custom00 = "happy", tongueOut = false }}
materials = {{ body_00 = {{ shade = [0.8, 0.6, 0.6] }} }}
acknowledged = {{ perform = "2026-10-13", by = "owner" }}

[parts.hat]
kind = "hat"
attach = {{ to = "anchor", part = "avatar", anchor = "head-top" }}
"#
    )
}

#[test]
fn the_design_node_reads_as_written() {
    let def: AvatarDef = toml::from_str(&stored()).unwrap();
    assert_eq!(def.check_schema(), Ok(()));
    assert_eq!(def.source, AvatarSource::new(ContentHash([7; 32]), SourceFormat::Vrm1, "Ava.vrm"));
    assert_eq!(def.humanoid[&HumanoidBone::LeftHand], NodeChoice::new(41, "J_Bip_L_Hand"));
    assert_eq!(def.channels[&Channel::CHEEK_PUFF], DriverChoice::Expression("Puff".into()));
    assert_eq!(def.channels[&Channel::TONGUE_OUT], DriverChoice::Nothing);
    let edit = &def.materials[&MaterialName::new("body_00").unwrap()];
    assert_eq!(edit.colour(ColorBindKind::ShadeColor), Some(Vec4::new(0.8, 0.6, 0.6, 1.0)));
    assert_eq!(edit.colour(ColorBindKind::Color), None, "an unedited field keeps the file's value");
    assert_eq!(def.acknowledged.as_ref().map(|a| a.by.as_str()), Some("owner"));
    assert!(def.parts.contains_key(&PartId::new("hat").unwrap()));
}

#[test]
fn the_node_round_trips_through_toml() {
    let def: AvatarDef = toml::from_str(&stored()).unwrap();
    let written = toml::to_string(&def).unwrap();
    let again: AvatarDef = toml::from_str(&written).unwrap();
    assert_eq!(again, def);
    assert_eq!(toml::to_string(&again).unwrap(), written, "writing is stable");
}

#[test]
fn a_new_node_writes_only_what_it_holds() {
    let def = AvatarDef::new(AvatarSource::new(ContentHash([7; 32]), SourceFormat::Gltf, "a.glb"));
    let written = toml::to_string(&def).unwrap();
    assert_eq!(written.lines().count(), 6, "schema and the source table only:\n{written}");
    assert_eq!(toml::from_str::<AvatarDef>(&written).unwrap(), def);
}

#[test]
fn unknown_keys_are_refused_at_every_level() {
    let base = stored();
    for (from, to) in [
        ("schema = 1", "schema = 1\nscheme = 1"),
        ("file = \"Ava.vrm\"", "file = \"Ava.vrm\", size = 3"),
        ("name = \"J_Bip_L_Hand\"", "name = \"J_Bip_L_Hand\", nmae = \"x\""),
        ("leftHand", "leftHnad"),
        ("cheekPuff", "cheekPuf"),
        ("shade = [0.8, 0.6, 0.6]", "shadow = [0.8, 0.6, 0.6]"),
        ("by = \"owner\"", "by = \"owner\", when = 1"),
        ("kind = \"hat\"", "kind = \"hat\"\nknd = 1"),
    ] {
        let text = base.replacen(from, to, 1);
        assert_ne!(text, base, "{from}");
        assert!(toml::from_str::<AvatarDef>(&text).is_err(), "{to} must be refused");
    }
}

#[test]
fn a_driver_choice_is_a_name_or_false() {
    let base = stored();
    for bad in ["tongueOut = true", "tongueOut = \"\"", "tongueOut = 3"] {
        let text = base.replacen("tongueOut = false", bad, 1);
        assert!(toml::from_str::<AvatarDef>(&text).is_err(), "{bad} must be refused");
    }
}

#[test]
fn another_schema_is_refused_by_name() {
    let def: AvatarDef = toml::from_str(&stored().replacen("schema = 1", "schema = 2", 1)).unwrap();
    assert_eq!(def.check_schema(), Err(AvatarError::unknown_schema(2)));
    assert_eq!(SCHEMA, 1);
}

#[test]
fn a_material_edit_names_each_colour_a_bind_can_move() {
    let mut edit = MaterialEdit::new();
    edit.base = Some([0.5, 0.123, 0.9999, 1.0]);
    edit.outline = Some([0.0, 0.0, 0.1]);
    assert_eq!(edit.colour(ColorBindKind::Color), Some(Vec4::new(0.5, 0.123, 0.9999, 1.0)));
    assert_eq!(edit.colour(ColorBindKind::OutlineColor), Some(Vec4::new(0.0, 0.0, 0.1, 1.0)));
    for kind in
        [ColorBindKind::EmissionColor, ColorBindKind::ShadeColor, ColorBindKind::MatcapColor, ColorBindKind::RimColor]
    {
        assert_eq!(edit.colour(kind), None);
    }
    assert!(!edit.is_empty() && MaterialEdit::new().is_empty());
}

#[test]
fn the_schema_describes_the_node() {
    let schema = schemars::schema_for!(AvatarDef);
    let text = serde_json::to_string(&schema).unwrap();
    for key in ["humanoid", "channels", "materials", "acknowledged", "parts", "DriverChoice"] {
        assert!(text.contains(key), "{key}");
    }
}
