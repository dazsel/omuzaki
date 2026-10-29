//! A track plays on a puppet: views move the picture, parameters hold until set again, and a bad track is refused
//! before anything draws.

use std::collections::BTreeMap;

use omuzaki_puppet::track::{PuppetTrack, TrackFrame, TrackValue};
use omuzaki_puppet::{CpuRenderer, Evaluator, PuppetBuilder, Texture, grid_mesh};
use omuzaki_puppet_api::{BindTarget, Interp};

fn puppet() -> (Evaluator, Vec<Texture>) {
    let mut b = PuppetBuilder::new("t", [8.0, 8.0]);
    let tex = b.texture("white.png", [2, 2]);
    let root = b.root();
    let part = b.part("square", root, tex, grid_mesh([-2.0, -2.0, 4.0, 4.0], 1, 1, [0.0, 0.0, 1.0, 1.0]));
    let fade = b.param("fade", 0.0, 1.0, 1.0, &[0.0, 1.0]);
    b.bind(fade, part, BindTarget::Opacity, Interp::Linear, vec![0.0, 1.0]);
    let eval = Evaluator::new(b.finish().unwrap_or_default()).unwrap_or_else(|e| panic!("{e}"));
    let white = Texture::from_straight(2, 2, &[255; 16]).unwrap_or_else(|| panic!("2x2"));
    (eval, vec![white])
}

fn frame(view: Option<[f32; 6]>, params: &[(&str, f32)]) -> TrackFrame {
    TrackFrame {
        view,
        params: params.iter().map(|(n, v)| ((*n).to_owned(), TrackValue::One(*v))).collect::<BTreeMap<_, _>>(),
    }
}

#[test]
fn views_move_the_picture_and_parameters_hold() {
    let (mut eval, textures) = puppet();
    let track = PuppetTrack {
        size: [16, 16],
        fps: 30,
        background: [0, 0, 0, 255],
        physics: false,
        frames: vec![
            frame(Some([1.0, 0.0, 0.0, 1.0, 4.0, 4.0]), &[]),
            frame(Some([1.0, 0.0, 0.0, 1.0, 12.0, 12.0]), &[("fade", 0.5)]),
            frame(None, &[]),
        ],
    };
    let mut frames = Vec::new();
    track
        .play(&mut eval, &textures, &mut CpuRenderer::new(2), |i, rgba| {
            assert_eq!(i, frames.len());
            frames.push(rgba.to_vec());
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{e}"));
    let px = |f: &[u8], x: usize, y: usize| f[(y * 16 + x) * 4];
    assert_eq!(px(&frames[0], 4, 4), 255, "the square around (4, 4)");
    assert_eq!(px(&frames[0], 12, 12), 0);
    assert!((126..=129).contains(&px(&frames[1], 12, 12)), "moved and half faded: {}", px(&frames[1], 12, 12));
    assert_eq!(frames[2], frames[1], "a frame that sets nothing keeps the view and the fade");
}

#[test]
fn a_track_naming_a_missing_parameter_is_refused() {
    let (mut eval, textures) = puppet();
    let track = PuppetTrack {
        size: [4, 4],
        fps: 30,
        background: [0; 4],
        physics: true,
        frames: vec![frame(None, &[("nope", 1.0)])],
    };
    let drawn = track.play(&mut eval, &textures, &mut CpuRenderer::new(1), |_, _| panic!("nothing draws"));
    assert!(drawn.is_err_and(|e| e.to_string().contains("nope")));
}
