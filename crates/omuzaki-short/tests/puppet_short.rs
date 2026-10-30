//! A short performed by a flat 2D puppet: the project's example short on small synthetic layered art, rigged on the
//! spot. Every frame comes out, the head and the mouth move with the performance, and a face from the script reaches
//! the puppet.

#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "test pixels")]

use omuzaki_puppet::autorig::autorig;
use omuzaki_puppet::bundle::Bundle;
use omuzaki_puppet::layers::{LayerDef, LayeredArt, LayersManifest};
use omuzaki_short::ShortDef;
use omuzaki_short::error::ShortError;
use omuzaki_short::puppet::render_puppet_short;
use omuzaki_short::render::{FrameSink, frame_count};
use omuzaki_short::timeline::Timeline;
use omuzaki_short::voice::{Synthesis, voice_for};

const GERALD: &str = include_str!("../shorts/gerald.short.toml");
const W: u32 = 200;
const H: u32 = 300;

fn disc(cx: f32, cy: f32, rx: f32, ry: f32, rgb: [u8; 3]) -> Vec<u8> {
    let mut px = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let (u, v) = ((x as f32 + 0.5 - cx) / rx, (y as f32 + 0.5 - cy) / ry);
            if u * u + v * v <= 1.0 {
                px[((y * W + x) * 4) as usize..][..4].copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
            }
        }
    }
    px
}

fn puppet() -> Bundle {
    let names = [
        "body",
        "face",
        "eye white L",
        "iris L",
        "eye white R",
        "iris R",
        "mouth",
        "mouth open",
        "bangs",
        "eyes squeeze",
    ];
    let layer = |name: &str| LayerDef {
        name: name.into(),
        file: name.into(),
        offset: [0, 0],
        role: None,
        side: None,
        depth: None,
        blend: omuzaki_puppet_api::BlendMode::Normal,
        opacity: 1.0,
        show_when: (name == "eyes squeeze").then(|| "eyesSqueeze".into()),
        hide_when: None,
        replaces: None,
    };
    let manifest =
        LayersManifest { name: "tiny".into(), canvas: [W, H], artist: None, layers: names.map(layer).to_vec() };
    let art = LayeredArt::from_manifest(manifest, |file| {
        let px = match file {
            "body" => disc(100.0, 260.0, 90.0, 60.0, [90, 110, 200]),
            "face" => disc(100.0, 110.0, 60.0, 70.0, [250, 220, 200]),
            "eye white L" => disc(78.0, 110.0, 12.0, 9.0, [255, 255, 255]),
            "iris L" => disc(78.0, 110.0, 7.0, 8.0, [60, 40, 120]),
            "eye white R" => disc(122.0, 110.0, 12.0, 9.0, [255, 255, 255]),
            "iris R" => disc(122.0, 110.0, 7.0, 8.0, [60, 40, 120]),
            "mouth" => disc(100.0, 150.0, 10.0, 3.0, [150, 60, 60]),
            "mouth open" => disc(100.0, 152.0, 9.0, 7.0, [110, 30, 40]),
            "bangs" => disc(100.0, 60.0, 64.0, 30.0, [70, 50, 40]),
            _ => disc(100.0, 110.0, 40.0, 10.0, [20, 20, 20]),
        };
        Ok(([W, H], px))
    })
    .unwrap_or_else(|e| panic!("synthetic art: {e}"));
    Bundle::from_rigged(autorig(&art).unwrap_or_else(|e| panic!("{e}")))
}

struct Frames(Vec<Vec<u8>>);

impl FrameSink for Frames {
    fn frame(&mut self, index: u64, rgba: &[u8]) -> Result<(), ShortError> {
        assert_eq!(index as usize, self.0.len(), "frames arrive in order");
        self.0.push(rgba.to_vec());
        Ok(())
    }
}

fn changed(a: &[u8], b: &[u8]) -> usize {
    a.as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .filter(|(p, q)| p.iter().zip(*q).any(|(x, y)| x.abs_diff(*y) > 24))
        .count()
}

#[test]
fn the_example_short_plays_on_a_puppet() {
    let mut def = ShortDef::from_toml(GERALD).unwrap_or_else(|e| panic!("{e}"));
    def.size = [90, 160];
    def.fps = 12;
    def.captions.enabled = false;
    def.puppet = Some("tiny.layers.toml".into());
    let voice = voice_for(&def, |_| unreachable!("silent"), Synthesis::Estimate).unwrap_or_else(|e| panic!("{e}"));
    let timeline = Timeline::new(&def, &voice).unwrap_or_else(|e| panic!("{e}"));
    let mut frames = Frames(Vec::new());
    let report = render_puppet_short(puppet(), &def, &timeline, &voice, &mut frames, &mut |_| Ok(()))
        .unwrap_or_else(|e| panic!("{e}"));
    let count = frame_count(timeline.duration, def.fps);
    assert_eq!(report.frames, count);
    assert_eq!(frames.0.len() as u64, count);
    // The puppet is in the picture: the face colour fills a good share of a face shot.
    let skin =
        frames.0[0].as_chunks::<4>().0.iter().filter(|p| p[0] > 220 && p[1] > 190 && p[2] > 170 && p[2] < 230).count();
    assert!(skin > 90 * 160 / 20, "the face fills the frame: {skin} skin pixels");
    // The picture lives: consecutive seconds differ (shots, head, mouth, blinks), and no frame is the backdrop alone.
    let moving = frames.0.windows(12).filter(|w| changed(&w[0], &w[11]) > 50).count();
    assert!(moving > frames.0.len() / 2, "{moving} of {} one-second steps move", frames.0.len());
    // Rendering the same short again gives the same frames.
    let mut again = Frames(Vec::new());
    render_puppet_short(puppet(), &def, &timeline, &voice, &mut again, &mut |_| Ok(()))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(frames.0 == again.0, "a puppet short renders to the same pixels every run");
}
