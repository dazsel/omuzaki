//! The shipped effect presets (particles.md D9; #1171): every file parses, keeps its rules and is found by name, a typo
//! is refused with the nearest names, and no preset breaks the flash rule on the mock.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

use std::sync::Arc;

use omuzaki_anim_api::HumanoidPose;
use omuzaki_char_api::CharacterBuilder;
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Image, Light, Projection, Quality, Renderer, Scene, TargetDesc, Tonemap,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::effects::log::EffectLimits;
use omuzaki_stage::effects::presets;
use omuzaki_stage::effects::service::StageEffects;
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage_api::{
    BurstRequest, ChatEmoteRef, EffectAt, EffectError, EffectKind, EffectOrigin, EffectService, LiveWeather, WeatherDef,
};

#[test]
fn every_shipped_preset_parses_and_keeps_its_rules() {
    let all = presets::load_all().expect("every preset file loads");
    let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["confetti", "emote-rain", "hit-stars", "petals", "rain", "snow"]);
    assert_eq!(presets::presets().len(), all.len(), "the library holds every file");
    let weather = all.iter().filter(|p| p.kind == EffectKind::Weather).count();
    assert_eq!(weather, 3, "rain, snow and petals are weather");
    for preset in &all {
        if let Some(def) = preset.weather_def(1.0) {
            def.check().unwrap_or_else(|(path, rule)| panic!("{}: weather.{path} {rule}", preset.name));
        }
    }
}

#[test]
fn the_list_rows_carry_each_burst_count() {
    let rows = presets::infos();
    let confetti = rows.iter().find(|r| r.name.as_str() == "confetti").expect("confetti listed");
    assert_eq!(confetti.kind, EffectKind::Burst);
    assert!(confetti.default_count.is_some_and(|c| c > 0));
    let rain = rows.iter().find(|r| r.name.as_str() == "rain").expect("rain listed");
    assert_eq!(rain.default_count, None);
}

#[test]
fn a_typo_names_the_nearest_presets() {
    let Err(EffectError::UnknownPreset { name, nearest, .. }) = presets::find("confeti") else {
        panic!("a typo is an unknown preset")
    };
    assert_eq!(name, "confeti");
    assert_eq!(nearest.first().map(String::as_str), Some("confetti"));
    let Err(EffectError::UnknownPreset { nearest, .. }) = presets::find("xyzzyplugh") else { panic!("unknown") };
    assert!(nearest.is_empty(), "nothing is near a word that isn't close: {nearest:?}");
}

#[test]
fn a_preset_of_the_other_kind_says_which_op_takes_it() {
    let Err(EffectError::Invalid(why)) = presets::find_kind("confetti", EffectKind::Weather) else {
        panic!("confetti is not weather")
    };
    assert!(why.contains("stage.effect.burst"), "{why}");
    assert!(presets::find_kind("snow", EffectKind::Weather).is_ok());
}

#[test]
fn a_misspelt_key_or_a_wrong_name_is_refused_with_the_file() {
    let text = include_str!("../library/effects/snow.toml");
    let misspelt = text.replacen("density", "densty", 1);
    let Err(EffectError::Invalid(why)) = presets::parse("snow", &misspelt) else { panic!("misspelt key refused") };
    assert!(why.contains("library/effects/snow.toml") && why.contains("density"), "{why}");
    let Err(EffectError::Invalid(why)) = presets::parse("sleet", text) else { panic!("name must be the file's") };
    assert!(why.contains("file's name"), "{why}");
}

#[test]
fn weather_materialises_its_values_at_an_intensity() {
    let rain = presets::find("rain").expect("rain");
    let def: WeatherDef = rain.weather_def(0.5).expect("rain is weather");
    assert_eq!(def.intensity, 0.5);
    assert_eq!(def.preset.as_ref().map(omuzaki_core::id::Slug::as_str), Some("rain"));
    assert_eq!(def.particles, rain.particles);
    assert!(presets::find("confetti").expect("confetti").weather_def(1.0).is_none());
}

/// Frames a second in the flash check.
const FLASH_FPS: u32 = 30;

/// Rec. 709 relative luminance of each pixel of an opaque frame, `0..=1`.
fn luminance(image: &Image) -> Vec<f32> {
    fn linear(c: u8) -> f32 {
        let c = f32::from(c) / 255.0;
        if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }
    image
        .rgba8
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| 0.2126 * linear(p[0]) + 0.7152 * linear(p[1]) + 0.0722 * linear(p[2]))
        .collect()
}

/// `preset` over 120 frames at 30 fps on the mock with the lit mock character at a bust camera over a dark backdrop
/// (the hardest case for bright particles): weather from the first frame, a burst every second. Returns, per frame,
/// the share of pixels whose luminance moved by more than 0.1 since the frame before.
fn changes(preset: &str) -> Vec<f32> {
    let def = omuzaki_char_mock::mock_character().expect("the fixture parses");
    let built = omuzaki_char_api::RegistryBuilder
        .build(
            &omuzaki_char_api::CharId::new("mock").expect("a valid id"),
            &def,
            &omuzaki_char_api::BuildOptions::default(),
            &omuzaki_char_api::BuildServices::default(),
        )
        .expect("the mock character builds");
    let mut renderer = MockRenderer::new();
    let size = UVec2::new(256, 144);
    let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight }).expect("a target");
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.6, 0.9),
        Vec3::new(0.0, 0.52, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 },
    ));
    scene.background = Background::Solid { colour: Colour::rgb(24, 26, 34) };
    scene.post.tonemap = Tonemap::None;
    scene.lights.push(Light::Directional {
        to_light: Vec3::new(0.3, 0.8, 1.0),
        colour: Colour::WHITE,
        intensity: 2.5,
        casts_shadow: false,
    });
    let mut stage = Stage::new(
        &mut renderer,
        built,
        UploadOptions { face_px_per_m: 600.0 },
        scene,
        target,
        Quality::live(),
        StageClock::live(),
    )
    .expect("the stage starts");
    let service = Arc::new(StageEffects::new(11));
    stage.attach_effects(Arc::clone(&service), EffectLimits::live());
    let slug = Slug::new(preset).expect("a preset name");
    let info = presets::find(preset).expect("a shipped preset");
    if info.kind == EffectKind::Weather {
        service.live_weather(&LiveWeather::preset(slug.clone(), 1.0, None)).expect("weather starts");
    }
    let mut pose = HumanoidPose::REST;
    seed_base_pose(&stage.built().rig, &mut pose);
    let emote = ChatEmoteRef { id: "1".into(), name: "Wave".into(), image: None };
    let mut before: Option<Vec<f32>> = None;
    let mut shares = Vec::with_capacity(120);
    for n in 0..120_u32 {
        if info.kind == EffectKind::Burst && n % FLASH_FPS == 0 {
            let emotes = if preset == "emote-rain" { vec![emote.clone()] } else { Vec::new() };
            // Hit stars come where something hits; here, in front of the face.
            let at = (preset == "hit-stars")
                .then(|| EffectAt { offset: [0.0, 1.3, 0.2], ..EffectAt::new(EffectOrigin::World, None) });
            let request = BurstRequest { emotes, at, ..BurstRequest::new(slug.clone()) };
            service.burst(&request).expect("the burst queues");
        }
        let input = FrameInput {
            pose,
            dt: 1.0 / FLASH_FPS as f32,
            time: Some(Seconds(f64::from(n) / f64::from(FLASH_FPS))),
            ..FrameInput::default()
        };
        stage.tick(&mut renderer, &input).expect("the frame renders");
        let image = renderer.read_target(target).expect("the frame reads back");
        if n == 45
            && let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR")
        {
            let png = omuzaki_stage::ops::render::still::encode_png(&image).expect("encodes");
            #[allow(clippy::disallowed_methods, reason = "test evidence, written where the run asks")]
            std::fs::write(std::path::Path::new(&dir).join(format!("flash-{preset}.png")), png).expect("written");
        }
        let now = luminance(&image);
        if let Some(before) = &before {
            let moved = now.iter().zip(before).filter(|(a, b)| (*a - *b).abs() > 0.1).count();
            shares.push(moved as f32 / now.len() as f32);
        }
        before = Some(now);
    }
    stage.release(&mut renderer).expect("the stage releases");
    shares
}

#[test]
fn no_preset_flashes_more_than_three_times_a_second() {
    for preset in presets::presets() {
        let name = preset.name.as_str();
        let shares = changes(name);
        let flashes: Vec<bool> = shares.iter().map(|&s| s > 0.25).collect();
        let worst = flashes.windows(FLASH_FPS as usize).map(|w| w.iter().filter(|&&f| f).count()).max().unwrap_or(0);
        let peak = shares.iter().copied().fold(0.0_f32, f32::max);
        println!("{name}: peak change {:.1} % of pixels, at most {worst} flashes a second", peak * 100.0);
        assert!(worst <= 3, "{name} flashes {worst} times in a second (peak {:.1} %)", peak * 100.0);
        assert!(peak > 0.0, "{name} draws something");
    }
}
