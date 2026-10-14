//! The painter and the rasteriser together: channels become pixels, and the pixels change the way the channels say.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{
    FaceDrawing, FaceInput, FaceLayerId, FaceMapping, FacePainter, FaceRasterizer, FaceView, Raster,
};
use omuzaki_face_mock::{MockPainter, MockRasterizer, mock_profile};

const PX_PER_M: f32 = 1000.0;

fn render(buf: &ChannelBuf, layer: FaceLayerId) -> Raster {
    let profile = mock_profile();
    let params = FaceMapping::default().apply(buf);
    let mut painter = MockPainter::default();
    let input = FaceInput { profile: &profile, params: &params, time: Seconds(0.0), view: FaceView::default() };
    let mut drawing = FaceDrawing::new();
    painter.paint(&input, &mut drawing).unwrap();
    let spec = *painter.layers().iter().find(|l| l.id == layer).unwrap();
    let mut out = Raster::new(1, 1);
    MockRasterizer.rasterize(&spec, &drawing[layer], PX_PER_M, &mut out).unwrap();
    out
}

fn covered(r: &Raster) -> usize {
    r.pixels.chunks(4).filter(|p| p[3] > 0).count()
}

fn with(pairs: &[(Channel, f32)]) -> ChannelBuf {
    let mut b = ChannelBuf::NEUTRAL;
    for (c, v) in pairs {
        b.set(*c, *v);
    }
    b
}

#[test]
fn an_open_mouth_draws_more_pixels_than_a_closed_one() {
    let closed = render(&ChannelBuf::NEUTRAL, FaceLayerId::Base);
    let open = render(&with(&[(Channel::JAW_OPEN, 0.7)]), FaceLayerId::Base);
    assert!(covered(&open) > covered(&closed) + 50, "{} vs {}", covered(&open), covered(&closed));
}

#[test]
fn a_blink_removes_the_eye_whites() {
    let open = render(&ChannelBuf::NEUTRAL, FaceLayerId::Base);
    let shut = render(&with(&[(Channel::EYE_BLINK_LEFT, 1.0), (Channel::EYE_BLINK_RIGHT, 1.0)]), FaceLayerId::Base);
    assert!(covered(&shut) + 1000 < covered(&open), "{} vs {}", covered(&shut), covered(&open));
}

#[test]
fn brows_alone_make_a_sparse_layer_that_follows_the_brow_channels() {
    let neutral = render(&ChannelBuf::NEUTRAL, FaceLayerId::Brows);
    let raised =
        render(&with(&[(Channel::BROW_OUTER_UP_LEFT, 1.0), (Channel::BROW_OUTER_UP_RIGHT, 1.0)]), FaceLayerId::Brows);
    let total = neutral.pixels.len() / 4;
    assert!(covered(&neutral) > 100 && covered(&neutral) * 20 < total, "thin strokes only");
    assert_ne!(neutral, raised, "raising moves the strokes");
}

#[test]
fn the_same_channels_rasterise_to_the_same_bytes() {
    let buf = with(&[(Channel::MOUTH_SMILE_LEFT, 0.7), (Channel::EYE_LOOK_OUT_LEFT, 0.5)]);
    assert_eq!(render(&buf, FaceLayerId::Base), render(&buf, FaceLayerId::Base));
}
