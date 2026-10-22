//! The sweep's direction check (#229): every face channel has an expected effect, the default face moves every channel
//! the expected way on the expected side, and planted faults (a mirrored smile, an inverted blink, a change painted on
//! the wrong half) are flagged by name.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_char_api::HeadParams;
use omuzaki_core::colour::Colour;
use omuzaki_face::preview::FaceSubject;
use omuzaki_face::preview::direction::{Direction, SideChange, check, entry, expected, parse};
use omuzaki_face::preview::sweep::{SweepOptions, default_min_changed_px, face_channels, sweep};
use omuzaki_face_api::{FaceMapping, FaceProfile, Palette};

fn subject() -> FaceSubject {
    FaceSubject {
        profile: FaceProfile::default().resolve_colours(&Palette::default()).unwrap(),
        projection: HeadParams::default().face_projection(),
        skin: Colour::rgb(0xf9, 0xd3, 0xc5),
    }
}

#[test]
fn every_face_channel_has_an_expected_effect() {
    let table = expected().unwrap();
    let missing: Vec<&str> =
        face_channels().into_iter().filter(|c| entry(table, *c).is_none()).map(Channel::name).collect();
    assert!(missing.is_empty(), "no expected effect for {missing:?}");
    assert_eq!(table.len(), face_channels().len(), "the table names only face channels");
}

#[test]
fn the_default_face_moves_every_channel_the_right_way_on_the_right_side() {
    let width_px = 387;
    let options = SweepOptions {
        channels: face_channels(),
        width_px,
        min_changed_px: default_min_changed_px(width_px),
        keep_pictures: false,
    };
    let report = sweep(&subject(), &options, &|| false).unwrap().unwrap();
    for direction in [Direction::WrongWay, Direction::WrongSide, Direction::Missing, Direction::Unchecked] {
        let flagged = report.with_direction(direction);
        let detail: Vec<_> =
            report.rows.iter().filter(|r| r.direction == direction).map(|r| (r.channel, r.sides)).collect();
        assert!(flagged.is_empty(), "{direction:?}: {detail:?}");
    }
    assert_eq!(report.with_direction(Direction::Inert), ["jawForward", "mouthShrugLower", "mouthShrugUpper"]);
    // The run is deterministic.
    assert_eq!(report, sweep(&subject(), &options, &|| false).unwrap().unwrap());
}

/// The paint parameters of the default face at rest and with `channel` at 1.
fn push(channel: Channel) -> (omuzaki_face_api::PaintParams, omuzaki_face_api::PaintParams) {
    let mapping = FaceMapping::default();
    let mut buf = ChannelBuf::NEUTRAL;
    let rest = mapping.apply(&buf);
    buf.set(channel, 1.0);
    (rest, mapping.apply(&buf))
}

/// A painted change that falls on the character's `left` half.
const ON_THE_LEFT: SideChange = SideChange { left: 900, right: 40 };

#[test]
fn a_mirrored_smile_and_an_inverted_blink_are_flagged() {
    let table = expected().unwrap();
    let verdict = |channel: Channel, pushed: &dyn Fn(&mut omuzaki_face_api::PaintParams), sides: SideChange| {
        let (rest, mut after) = push(channel);
        pushed(&mut after);
        check(entry(table, channel), &rest, &after, true, sides)
    };
    // As the default mapping paints them, both are right.
    assert_eq!(verdict(Channel::MOUTH_SMILE_LEFT, &|_| {}, ON_THE_LEFT), Direction::Right);
    assert_eq!(verdict(Channel::EYE_BLINK_LEFT, &|_| {}, ON_THE_LEFT), Direction::Right);
    // A face whose smile corners are mirrored lifts the right corner for mouthSmileLeft: the left corner never moves.
    let mirrored = |p: &mut omuzaki_face_api::PaintParams| *p = p.mirrored();
    assert_eq!(verdict(Channel::MOUTH_SMILE_LEFT, &mirrored, ON_THE_LEFT), Direction::WrongWay);
    // An inverted blink opens the eye wider instead of closing it.
    let inverted = |p: &mut omuzaki_face_api::PaintParams| {
        p.left_eye.close = 0.0;
        p.left_eye.open = 1.6;
    };
    assert_eq!(verdict(Channel::EYE_BLINK_LEFT, &inverted, ON_THE_LEFT), Direction::WrongWay);
    // The right move painted on the wrong half of the face.
    let on_the_right = SideChange { left: 40, right: 900 };
    assert_eq!(verdict(Channel::EYE_BLINK_LEFT, &|_| {}, on_the_right), Direction::WrongSide);
    // A channel swept down from its maximum expects the opposite move.
    let (rest, after) = push(Channel::EYE_BLINK_LEFT);
    assert_eq!(check(entry(table, Channel::EYE_BLINK_LEFT), &after, &rest, false, ON_THE_LEFT), Direction::Right);
}

#[test]
fn a_broken_table_is_refused_by_name() {
    assert!(parse("[eyeBlinkLeft]\nmeasure = \"eye_size\"\nsign = 1\n").unwrap_err().contains("unknown measure"));
    assert!(parse("[eyeBlinkLeft]\nmeasure = \"eye_open.left\"\nsign = 2\n").unwrap_err().contains("sign"));
    assert!(parse("[eyeBlinkLeft]\nsign = 1\n").unwrap_err().contains("no measure"));
}
