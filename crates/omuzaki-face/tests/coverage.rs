//! FE-01 and FE-02 on the real painter: every ARKit-52 channel and every anime extra visibly changes the painted face,
//! except the three the contract leaves undrawn; a sabotaged face is caught with its dead controls named; a sided
//! channel changes the mirror image of what its other side changes; and head, body and custom channels change nothing.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{CHANNELS, Channel, ChannelBuf, Region};
use omuzaki_char_api::HeadParams;
use omuzaki_core::colour::Colour;
use omuzaki_face::preview::sweep::{
    ChannelStatus, SweepOptions, UNMAPPED, default_min_changed_px, face_channels, sweep,
};
use omuzaki_face::preview::{FaceSubject, Preview};
use omuzaki_face_api::{FaceLayerId, FaceProfile, FangKind, Palette, Raster};

/// A quarter of the reference width: fast in a debug build, and the threshold scales with it.
const WIDTH: u32 = 387;

fn subject(profile: FaceProfile) -> FaceSubject {
    FaceSubject {
        profile: profile
            .resolve_colours(&Palette::default())
            .expect("the default face uses fixed colours or fallbacks"),
        projection: HeadParams::default().face_projection(),
        skin: Colour::rgb(0xf9, 0xd3, 0xc5),
    }
}

fn options(channels: Vec<Channel>) -> SweepOptions {
    SweepOptions { channels, width_px: WIDTH, min_changed_px: default_min_changed_px(WIDTH), keep_pictures: false }
}

#[test]
fn every_face_channel_moves_the_default_face_except_the_three_unmapped() {
    let report = sweep(&subject(FaceProfile::default()), &options(face_channels()), &|| false).unwrap().unwrap();
    assert_eq!(report.rows.len(), 65, "52 ARKit channels and 13 anime extras");
    assert_eq!(report.with_status(ChannelStatus::Dead), Vec::<&str>::new());
    assert_eq!(report.with_status(ChannelStatus::OffInDesign), Vec::<&str>::new(), "the default face has fangs");
    let unmapped: Vec<&str> = UNMAPPED.iter().map(|c| c.name()).collect();
    assert_eq!(report.with_status(ChannelStatus::Unmapped), unmapped);
    for row in &report.rows {
        if row.status == ChannelStatus::Unmapped {
            assert_eq!(row.most_changed_px(), 0, "{} is unmapped, so it must not draw", row.channel);
        }
    }
}

#[test]
fn a_sabotaged_face_is_caught_with_its_dead_controls_named() {
    let mut broken = FaceProfile::default();
    broken.mapping.eyes.wide = 0.0;
    broken.mapping.cheeks.blush = 0.0;
    let channels = vec![Channel::EYE_WIDE_LEFT, Channel::EYE_WIDE_RIGHT, Channel::BLUSH, Channel::JAW_OPEN];
    let report = sweep(&subject(broken), &options(channels), &|| false).unwrap().unwrap();
    assert_eq!(report.with_status(ChannelStatus::Dead), ["eyeWideLeft", "eyeWideRight", "blush"]);
    assert_eq!(report.with_status(ChannelStatus::Moves), ["jawOpen"]);
}

#[test]
fn fangs_on_a_face_without_them_are_off_in_the_design_not_dead() {
    let mut fangless = FaceProfile::default();
    fangless.mouth.fang.kind = FangKind::None;
    let report = sweep(&subject(fangless), &options(vec![Channel::FANGS]), &|| false).unwrap().unwrap();
    assert_eq!(report.with_status(ChannelStatus::OffInDesign), ["fangs"]);
}

#[test]
fn a_cancelled_sweep_stops() {
    let report = sweep(&subject(FaceProfile::default()), &options(face_channels()), &|| true).unwrap();
    assert!(report.is_none());
}

/// Which pixels of the Base layer differ from `rest` by more than the sweep's tolerance.
fn changed_mask(rest: &Raster, pushed: &Raster) -> Vec<bool> {
    rest.pixels
        .as_chunks::<4>()
        .0
        .iter()
        .zip(pushed.pixels.as_chunks::<4>().0)
        .map(|(a, b)| a.iter().zip(b).any(|(x, y)| x.abs_diff(*y) > 8))
        .collect()
}

#[test]
fn a_sided_channel_changes_the_mirror_image_of_its_other_side() {
    let face = subject(FaceProfile::default());
    let mut preview = Preview::new(face.clone(), WIDTH);
    preview.paint(&ChannelBuf::NEUTRAL).unwrap();
    let rest = preview.layer(FaceLayerId::Base).clone();
    let (w, h) = (rest.width as usize, rest.height as usize);
    for channel in [
        Channel::EYE_BLINK_LEFT,
        Channel::EYE_ARC_LEFT,
        Channel::EYE_WIDE_LEFT,
        Channel::BROW_OUTER_UP_LEFT,
        Channel::BROW_DOWN_LEFT,
        Channel::CHEEK_SQUINT_LEFT,
        Channel::MOUTH_SMILE_LEFT,
        Channel::NOSE_SNEER_LEFT,
    ] {
        let mut bus = ChannelBuf::NEUTRAL;
        bus.set(channel, 1.0);
        preview.paint(&bus).unwrap();
        let one = changed_mask(&rest, preview.layer(FaceLayerId::Base));
        bus.mirror();
        preview.paint(&bus).unwrap();
        let other = changed_mask(&rest, preview.layer(FaceLayerId::Base));
        // Mirror `other` left to right and measure how well it covers `one` (intersection over union).
        let (mut both, mut either) = (0u32, 0u32);
        for y in 0..h {
            for x in 0..w {
                let (a, b) = (one[y * w + x], other[y * w + (w - 1 - x)]);
                both += u32::from(a && b);
                either += u32::from(a || b);
            }
        }
        assert!(either > 0, "{channel} changed nothing");
        let iou = f64::from(both) / f64::from(either);
        // Not 1: the eye highlights are the same on both eyes (one light), so they cut the lids a little differently.
        assert!(iou > 0.8, "{channel}: the mirrored change overlaps by only {iou:.2}");
        // And the change leans to the character's own side: the left is +x, the right half of the picture. A brow
        // also tilts both eyes a little (the eyes share one tilt), so the test is "more on its side", not "only".
        let half = |range: std::ops::Range<usize>| -> u32 {
            (0..h).map(|y| range.clone().map(|x| u32::from(one[y * w + x])).sum::<u32>()).sum()
        };
        let (left_half, right_half) = (half(0..w / 2), half(w / 2..w));
        assert!(right_half > left_half, "{channel} drew more on the wrong side ({right_half} against {left_half})");
    }
}

#[test]
fn head_body_and_custom_channels_change_nothing_in_the_face() {
    let face = subject(FaceProfile::default());
    let mut preview = Preview::new(face, WIDTH);
    preview.paint(&ChannelBuf::NEUTRAL).unwrap();
    let rest: Vec<Raster> = FaceLayerId::ALL.iter().map(|&l| preview.layer(l).clone()).collect();
    for info in CHANNELS.iter().filter(|c| matches!(c.region, Region::Head | Region::Body | Region::Custom)) {
        let mut bus = ChannelBuf::NEUTRAL;
        bus.set(info.channel, info.max);
        preview.paint(&bus).unwrap();
        for (layer, before) in FaceLayerId::ALL.iter().zip(&rest) {
            assert_eq!(preview.layer(*layer), before, "{} changed the {layer:?} layer", info.name);
        }
    }
}
