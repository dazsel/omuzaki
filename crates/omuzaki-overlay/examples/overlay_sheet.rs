//! Draws the overlay's evidence: every widget kind on a 1920x1080 program frame, and a strip of each transition at
//! its halfway point. `cargo run -p omuzaki-overlay --example overlay_sheet -- <out dir> [backdrop.png] [second.png]`
//! The backdrops stand in for two scenes' rendered frames; without them two gradients are used.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout, clippy::disallowed_methods)]
#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "an evidence tool: pixel maths on a fixed 1080p frame"
)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use glam::UVec2;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocWrite, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_live_api::event::{LiveEvent, Source};
use omuzaki_live_api::ids::EventKind;
use omuzaki_overlay::ProgramService;
use omuzaki_overlay::transition::{Blender, Direction, TransitionKind, TransitionSpec};
use omuzaki_render_api::{AlphaEncoding, Image};
use serde_json::{Value, json};

const W: u32 = 1920;
const H: u32 = 1080;

fn backdrop(path: Option<&PathBuf>, top: [u8; 3], bottom: [u8; 3]) -> Image {
    if let Some(path) = path {
        let img = image::open(path).unwrap().resize_to_fill(W, H, image::imageops::FilterType::Lanczos3).to_rgba8();
        return Image::new(UVec2::new(W, H), AlphaEncoding::Straight, img.into_raw()).unwrap();
    }
    let mut px = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        let t = y as f32 / H as f32;
        let mix = |a: u8, b: u8| (f32::from(a) * (1.0 - t) + f32::from(b) * t) as u8;
        for _ in 0..W {
            px.extend_from_slice(&[mix(top[0], bottom[0]), mix(top[1], bottom[1]), mix(top[2], bottom[2]), 255]);
        }
    }
    Image::new(UVec2::new(W, H), AlphaEncoding::Straight, px).unwrap()
}

fn save(image: &Image, path: &Path) {
    image::save_buffer(path, &image.rgba8, image.size.x, image.size.y, image::ColorType::Rgba8).unwrap();
    println!("{}", path.display());
}

fn event(kind: &str, payload: Value) -> LiveEvent {
    LiveEvent::new(EventKind::new(kind).unwrap(), Source::Input, Seconds(0.0), payload)
}

fn user(name: &str, colour: &str) -> Value {
    json!({ "id": name, "login": name.to_lowercase(), "name": name, "colour": colour })
}

fn main() {
    let args: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    let out = args.first().cloned().unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&out).unwrap();
    let scene_a = backdrop(args.get(1), [42, 30, 70], [210, 120, 160]);
    let scene_b = backdrop(args.get(2), [20, 60, 80], [120, 200, 190]);

    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let program = ProgramService::new(clock.clone()).unwrap();
    let mut doc = MemDoc::new();
    let widgets = json!({
        "alerts": { "kind": "alerts" },
        "chat": { "kind": "chat", "lines": 6, "layout": { "anchor": "bottom_left", "width": 0.27 } },
        "goal": { "kind": "goal", "title": "Road to 100 subs", "target": 100, "start": 61 },
        "subathon": { "kind": "timer", "mode": "subathon", "seconds": 7265, "label": "Subathon" },
        "title": { "kind": "text", "text": "Cosy rainy night stream", "subtitle": "drawing my new outfit together",
                   "layout": { "anchor": "bottom_right", "width": 0.34 }, "style": { "accent": "#7ad7ff" } }
    });
    doc.apply(&Patch::new().with(DocPath::root().join("overlay"), None, Some(json!({ "widgets": widgets })))).unwrap();
    program.follow(&doc);
    for (name, colour, text) in [
        ("Mochi", "#ff7aa8", "the rain sounds are so cosy tonight"),
        ("Kaz", "#1e3a8a", "hiii"),
        ("lumen", "#7ad7ff", "the new hair looks amazing, did you make it with the strand tool?"),
        ("Tako", "#ffd166", "!bonk"),
        ("Pip", "#9bff9b", "first time here, love the vibe"),
    ] {
        program
            .observe(&event("chat.message", json!({ "user": user(name, colour), "text": text, "message_id": name })));
    }
    program.observe(&event(
        "tip",
        json!({ "user": user("Mochi", "#ff7aa8"), "amount": { "micros": 5_000_000, "currency": "USD" },
                "message": "for the new outfit fund!" }),
    ));
    let mut frame = scene_a.clone();
    program.compose(&mut frame);
    clock.advance(Seconds(1.0));
    let mut frame = scene_a.clone();
    program.compose(&mut frame);
    save(&frame, &out.join("overlay-widgets-1080p.png"));

    // The same overlay over a transparent program, as OBS receives it, shown over a checkerboard.
    let mut clear = Image::new(UVec2::new(W, H), AlphaEncoding::Straight, vec![0; (W * H * 4) as usize]).unwrap();
    program.compose(&mut clear);
    let mut checker = clear.clone();
    for (i, px) in checker.rgba8.chunks_mut(4).enumerate() {
        let (x, y) = ((i as u32 % W) / 24, (i as u32 / W) / 24);
        let bg = if (x + y) % 2 == 0 { 200.0 } else { 150.0 };
        let a = f32::from(px[3]) / 255.0;
        for c in &mut px[..3] {
            *c = (f32::from(*c) * a + bg * (1.0 - a)) as u8;
        }
        px[3] = 255;
    }
    save(&checker, &out.join("overlay-widgets-alpha.png"));

    // Each transition at 25%, 50% and 75%, scaled down into one sheet.
    let kinds = [
        TransitionSpec { kind: TransitionKind::Fade, ..TransitionSpec::default() },
        TransitionSpec { kind: TransitionKind::Dip, ..TransitionSpec::default() },
        TransitionSpec { kind: TransitionKind::Wipe, direction: Direction::Right, ..TransitionSpec::default() },
        TransitionSpec { kind: TransitionKind::Slide, ..TransitionSpec::default() },
        TransitionSpec { kind: TransitionKind::Iris, ..TransitionSpec::default() },
    ];
    let (tw, th) = (W / 4, H / 4);
    let mut sheet = image::RgbaImage::new(tw * 3, th * kinds.len() as u32);
    let mut blender = Blender::default();
    for (row, spec) in kinds.iter().enumerate() {
        for (col, t) in [0.25_f32, 0.5, 0.75].into_iter().enumerate() {
            let mut to = scene_b.clone();
            blender.blend(spec, &scene_a, &mut to, t);
            let img = image::RgbaImage::from_raw(W, H, to.rgba8).unwrap();
            let small = image::imageops::resize(&img, tw, th, image::imageops::FilterType::Triangle);
            image::imageops::overlay(&mut sheet, &small, i64::from(col as u32 * tw), i64::from(row as u32 * th));
        }
    }
    let path = out.join("transitions-sheet.png");
    sheet.save(&path).unwrap();
    println!("{} (rows: fade, dip, wipe, slide, iris; columns: 25%, 50%, 75%)", path.display());

    // What composing costs at 1080p, with every widget up (pictures cached after the first frame).
    let mut worst = 0.0_f64;
    for _ in 0..60 {
        let mut f = scene_a.clone();
        let t = std::time::Instant::now();
        program.compose(&mut f);
        worst = worst.max(t.elapsed().as_secs_f64() * 1000.0);
        clock.advance(Seconds(1.0 / 60.0));
    }
    println!(
        "compose at 1080p, five widgets: worst of 60 frames {worst:.2} ms; status {}",
        program.status()["compose"]
    );
}
