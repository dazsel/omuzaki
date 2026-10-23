//! The six effect presets on lavapipe (particles.md D9, D10; #1171): the kit's standard character in the default scene
//! at the bust camera, each preset at three moments, over the scene's backdrop and over transparent. Set
//! `OMUZAKI_EVIDENCE_DIR` to keep the contact sheet; without the `wgpu` feature the file is empty.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![cfg(feature = "wgpu")]

#[path = "render/support.rs"]
mod support;

use std::sync::Arc;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_char::ops::character::create::CharacterCreate;
use omuzaki_char_api::{BuildServices, CharId, CharacterBuilder};
use omuzaki_core::glam::UVec2;
use omuzaki_core::id::Slug;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Op, OpCtx};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{AlphaEncoding, Background, Image, MtoonSupport, Renderer, Scene, TargetDesc};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::body::{Body, BodyServices, load_body};
use omuzaki_stage::effects::log::EffectLimits;
use omuzaki_stage::effects::presets;
use omuzaki_stage::effects::service::StageEffects;
use omuzaki_stage::effects::weather::ResolvedWeather;
use omuzaki_stage::frame::face::FaceRig;
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::ops::render::job::RenderQuality;
use omuzaki_stage::scene::alone::{ShotChoice, StagedScene, cast_alone, stage_alone};
use omuzaki_stage::scene::default::scene_for;
use omuzaki_stage_api::{BurstRequest, ChatEmoteRef, EffectAt, EffectOrigin, EffectService, Framing, LiveWeather};
use serde_json::json;

use support::{Harness, TestDoc};

/// One cell of the sheet.
const CELL: UVec2 = UVec2::new(384, 216);
/// Ticks a second: particles are evaluated in closed form, so a coarse step shows the same moments.
const RATE: f64 = 10.0;

/// A preset, how to start it, and the seconds after its start the sheet shows.
struct Case {
    name: &'static str,
    weather: bool,
    at: Option<EffectAt>,
    moments: [f64; 3],
}

fn cases() -> [Case; 6] {
    // Hit stars come where something hits: here, beside the head and towards the camera.
    let beside_head = EffectAt { offset: [0.12, 0.05, 0.12], ..EffectAt::new(EffectOrigin::Subject, Some("head")) };
    [
        Case { name: "confetti", weather: false, at: None, moments: [0.2, 1.2, 2.2] },
        Case { name: "emote-rain", weather: false, at: None, moments: [0.4, 1.0, 1.6] },
        Case { name: "hit-stars", weather: false, at: Some(beside_head), moments: [0.1, 0.3, 0.5] },
        Case { name: "rain", weather: true, at: None, moments: [0.5, 1.0, 1.5] },
        Case { name: "snow", weather: true, at: None, moments: [0.5, 1.0, 1.5] },
        Case { name: "petals", weather: true, at: None, moments: [0.5, 1.0, 1.5] },
    ]
}

/// The kit's standard character, built for offline quality.
fn standard() -> omuzaki_char_api::BuiltCharacter {
    let mut doc = MemDoc::from_value(json!({}));
    let actor = Actor::Person { name: "test".to_owned() };
    let clock = FakeClock::new(Seconds(0.0));
    let op: CharacterCreate =
        serde_json::from_value(json!({ "id": "standard", "name": "standard", "template": "standard", "seed": 7 }))
            .unwrap();
    let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap();
    doc.apply(&plan.patch).unwrap();
    let harness = Harness::new(TestDoc(doc));
    let body = harness.with_job(|job| {
        job.read(|doc| {
            let services = BodyServices::new(
                job.service::<dyn CharacterBuilder>()?,
                job.service::<BuildServices>()?,
                job.service::<dyn omuzaki_store_api::Store>()?,
                MtoonSupport::NONE,
                &clock,
            );
            load_body(doc, &services, &CharId::new("standard").unwrap(), None, RenderQuality::Offline.detail())
        })
    });
    match body.unwrap() {
        Body::Kit(built) => *built,
        Body::Avatar(_) => panic!("a kit character builds as a kit body"),
    }
}

/// The default scene with `built` alone in it at the bust camera, for a cell's aspect.
fn bust_scene(built: &omuzaki_char_api::BuiltCharacter) -> StagedScene {
    let doc = MemDoc::from_value(json!({}));
    let (_, mut def) = scene_for(&doc, None).unwrap();
    let member = cast_alone(&mut def, &CharId::new("standard").unwrap(), None).unwrap();
    let mut choice = ShotChoice::default();
    choice.framing = Some(Framing::Bust);
    choice.apply(&mut def).unwrap();
    let landmarks = omuzaki_stage::scene::landmarks::Landmarks::measure(built).unwrap();
    #[allow(clippy::cast_precision_loss, reason = "a cell's sides are small")]
    let aspect = CELL.x as f32 / CELL.y as f32;
    stage_alone(&def, &member, landmarks, aspect).unwrap()
}

/// Gives the scene `weather` and opens it as a scene switch does.
fn set_weather(stage: &mut Stage, weather: Option<ResolvedWeather>) {
    let frame = stage.effects_mut().unwrap();
    let mut scene = frame.scene().clone();
    scene.weather = weather;
    frame.set_scene(&scene);
    frame.scene_switched();
}

/// What one background gave: every case's three moments, in case order, and each case's first frame, before its
/// particles.
struct Shots {
    moments: Vec<Image>,
    bare: Vec<Image>,
}

/// Every case over `background` (the scene's own when `None`).
fn shoot(
    renderer: &mut dyn Renderer,
    built: omuzaki_char_api::BuiltCharacter,
    background: Option<Background>,
) -> Shots {
    let mut staged = bust_scene(&built);
    if let Some(background) = background {
        staged.resolved.background = background;
    }
    let mut base = HumanoidPose::REST;
    seed_base_pose(&built.rig, &mut base);
    let target = renderer.create_target(&TargetDesc { size: CELL, alpha: AlphaEncoding::Straight }).unwrap();
    let camera = staged.camera().unwrap();
    let mut stage = Stage::new(
        renderer,
        built,
        UploadOptions::default(),
        Scene::new(camera),
        target,
        RenderQuality::Offline.quality(),
        // The frame's time comes from the input, so a coarse step still lands on each moment.
        StageClock::live(),
    )
    .unwrap();
    let service = Arc::new(StageEffects::new(5));
    stage.attach_effects(Arc::clone(&service), EffectLimits::live());
    stage.set_scene(&staged.resolved, staged.subject_root);
    stage.pay_scene(renderer).unwrap();
    stage.set_face(renderer, FaceRig::standard()).unwrap();
    // The last tick's time.
    let mut t = 0.0_f64;
    let tick = |stage: &mut Stage, renderer: &mut dyn Renderer, t: &mut f64, to: f64| {
        #[allow(clippy::cast_possible_truncation, reason = "a tenth of a second")]
        let dt = (to - *t) as f32;
        *t = to;
        let input = FrameInput { pose: base, dt, time: Some(Seconds(to)), ..FrameInput::default() };
        stage.tick(renderer, &input).unwrap();
    };
    tick(&mut stage, renderer, &mut t, 0.0);
    let wave = ChatEmoteRef { id: "1".into(), name: "Wave".into(), image: None };
    let heart = ChatEmoteRef { id: "2".into(), name: "Heart".into(), image: None };
    let mut shots = Shots { moments: Vec::new(), bare: Vec::new() };
    for case in cases() {
        // A clean stage for each case: no weather in the scene or live, and the last case's particles gone.
        set_weather(&mut stage, None);
        service.live_weather(&LiveWeather::Scene).unwrap();
        service.clear(0.0).unwrap();
        let start = t + 1.0 / RATE;
        tick(&mut stage, renderer, &mut t, start);
        shots.bare.push(renderer.read_target(target).unwrap());
        if case.weather {
            // As `stage.set_weather` leaves it: the scene's weather, pre-warmed as a scene's is when it opens (D10).
            let def = presets::find(case.name).unwrap().weather_def(1.0).unwrap();
            set_weather(
                &mut stage,
                Some(ResolvedWeather {
                    sprite: def.sprite,
                    params: def.particles,
                    density: def.density,
                    intensity: def.intensity,
                    height: def.height,
                    volume: None,
                }),
            );
        } else {
            let slug = Slug::new(case.name).unwrap();
            let emotes = if case.name == "emote-rain" { vec![wave.clone(), heart.clone()] } else { Vec::new() };
            service.burst(&BurstRequest { at: case.at.clone(), emotes, ..BurstRequest::new(slug) }).unwrap();
        }
        let mut next = start;
        for moment in case.moments {
            while next + 1.0 / RATE < start + moment {
                next += 1.0 / RATE;
                tick(&mut stage, renderer, &mut t, next);
            }
            next = start + moment;
            tick(&mut stage, renderer, &mut t, next);
            shots.moments.push(renderer.read_target(target).unwrap());
        }
    }
    stage.release(renderer).unwrap();
    renderer.release(target.into()).unwrap();
    shots
}

/// `image` over a grey checkerboard, so transparency shows.
fn over_checker(image: &Image) -> image::RgbaImage {
    image::RgbaImage::from_fn(image.size.x, image.size.y, |x, y| {
        let i = ((y * image.size.x + x) * 4) as usize;
        let p = &image.rgba8[i..i + 4];
        let back: u16 = if (x / 12 + y / 12) % 2 == 0 { 150 } else { 110 };
        let a = u16::from(p[3]);
        let mix = |c: u8| u8::try_from((u16::from(c) * a + back * (255 - a)) / 255).unwrap_or(255);
        image::Rgba([mix(p[0]), mix(p[1]), mix(p[2]), 255])
    })
}

/// How many pixels of a picture over transparent are drawn at all.
fn covered(image: &Image) -> usize {
    image.rgba8.as_chunks::<4>().0.iter().filter(|p| p[3] > 0).count()
}

#[test]
#[allow(clippy::disallowed_methods, reason = "the evidence sheet is written beside the test, not by an op")]
fn on_the_gpu_every_preset_shows_over_the_default_scene_and_over_transparent() {
    let mut renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let scene = shoot(&mut renderer, standard(), None).moments;
    let clear = shoot(&mut renderer, standard(), Some(Background::Transparent));
    assert_eq!(scene.len(), 18);

    if let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") {
        // Rows are the cases; the left three columns the default scene, the right three transparent on a checker.
        let mut sheet = image::RgbaImage::new(CELL.x * 6, CELL.y * 6);
        for (i, (s, c)) in scene.iter().zip(&clear.moments).enumerate() {
            let (row, col) = (u32::try_from(i / 3).unwrap(), u32::try_from(i % 3).unwrap());
            let s = image::RgbaImage::from_raw(s.size.x, s.size.y, s.rgba8.clone()).unwrap();
            image::imageops::overlay(&mut sheet, &s, i64::from(col * CELL.x), i64::from(row * CELL.y));
            image::imageops::overlay(
                &mut sheet,
                &over_checker(c),
                i64::from((col + 3) * CELL.x),
                i64::from(row * CELL.y),
            );
        }
        sheet.save(std::path::Path::new(&dir).join("effects-presets-sheet.png")).unwrap();
    }

    // Over transparent, each preset covers pixels the character and the ground left empty, at every moment.
    let mut hidden = Vec::new();
    for (i, case) in cases().iter().enumerate() {
        let bare = covered(&clear.bare[i]);
        let added: Vec<usize> =
            clear.moments[3 * i..3 * i + 3].iter().map(|m| covered(m).saturating_sub(bare)).collect();
        println!("{}: {added:?} pixels drawn over transparent beyond the character's {bare}", case.name);
        if added.iter().any(|&n| n <= 20) {
            hidden.push(case.name);
        }
    }
    assert!(hidden.is_empty(), "these presets don't show at every moment: {hidden:?}");
}
