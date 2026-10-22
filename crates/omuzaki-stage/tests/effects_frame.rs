//! The effects' frame step on the stage (particles.md D7, D10, D12, D14; #1170): bursts spawn on the next frame at
//! this frame's bone, weather runs from the scene and the live override, a weather change cross-fades on immutable
//! systems, the frame allocates nothing, and checkpoints restore exactly, into the same stage or a fresh one.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
// The counting allocator is the one unsafe impl: it forwards to `System` unchanged (the other hot-loop tests do the same).
#![allow(
    unsafe_code,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

#[path = "frame/double.rs"]
mod double;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_anim_api::{HumanoidBone, HumanoidPose};
use omuzaki_core::glam::{Quat, UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{
    AlphaEncoding, Camera, ParticleSystemDesc, ParticleSystemId, Projection, Quality, Renderer, Scene, SpawnBatch,
    Sprite, TargetDesc, particles,
};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::effects::checkpoint::{EffectCheckpoint, SystemRecord};
use omuzaki_stage::effects::frame::{Live, palette_colours, systems_live};
use omuzaki_stage::effects::log::EffectLimits;
use omuzaki_stage::effects::presets;
use omuzaki_stage::effects::service::StageEffects;
use omuzaki_stage::effects::weather::{ResolvedWeather, SceneEffects};
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage_api::effects::{BurstShape, EffectEventKind, EffectService, WeatherSource};
use omuzaki_stage_api::{BurstRequest, ChatEmoteRef, EffectAt, EffectEvent, EffectOrigin, LiveWeather, SpriteDef};
use sha2::{Digest, Sha256};

use double::{Recorder, built};

struct Counting;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    let _ = ACTIVE.try_with(|active| {
        if active.get() {
            let _ = COUNT.try_with(|count| count.set(count.get() + 1));
        }
    });
}

// SAFETY: every call forwards unchanged to the system allocator; counting touches only const-initialised cells.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

fn allocations_in<R>(f: impl FnOnce() -> R) -> (R, u64) {
    COUNT.with(|count| count.set(0));
    ACTIVE.with(|active| active.set(true));
    let result = f();
    ACTIVE.with(|active| active.set(false));
    (result, COUNT.with(Cell::get))
}

/// Frames a second.
const RATE: u64 = 30;
const FPS: f64 = RATE as f64;

fn camera() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 1.0, 2.6),
        Vec3::new(0.0, 0.9, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 },
    )
}

/// A stage on `renderer` with the mock character and effects attached to a fresh service.
fn stage(renderer: &mut Recorder) -> (Stage, Arc<StageEffects>) {
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let mut stage = Stage::new(
        renderer,
        built(),
        UploadOptions { face_px_per_m: 600.0 },
        Scene::new(camera()),
        target,
        Quality::offline(),
        StageClock::live(),
    )
    .expect("the stage starts");
    let service = Arc::new(StageEffects::new(7));
    stage.attach_effects(Arc::clone(&service), EffectLimits::live());
    (stage, service)
}

/// Frame `n` of the script: the right arm swings, the spine sways.
fn input(base: &HumanoidPose, n: u64) -> FrameInput {
    let t = n as f32 / FPS as f32;
    let mut pose = *base;
    pose.set(HumanoidBone::Spine, base.get(HumanoidBone::Spine) * Quat::from_rotation_z(0.2 * (t * 1.7).sin()));
    pose.set(
        HumanoidBone::RightUpperArm,
        base.get(HumanoidBone::RightUpperArm) * Quat::from_rotation_x(0.9 * (t * 2.3).sin()),
    );
    FrameInput { pose, dt: 1.0 / FPS as f32, time: Some(Seconds(n as f64 / FPS)), ..FrameInput::default() }
}

fn base(stage: &Stage) -> HumanoidPose {
    let mut base = HumanoidPose::REST;
    seed_base_pose(&stage.built().rig, &mut base);
    base
}

fn slug(name: &str) -> Slug {
    Slug::new(name).unwrap()
}

/// The scene effects with `preset`'s weather, falling over the default floor bounds.
fn weather(preset: &str) -> SceneEffects {
    let p = presets::find(preset).unwrap();
    let w = p.weather.as_ref().unwrap();
    SceneEffects {
        weather: Some(ResolvedWeather {
            sprite: p.sprite,
            params: p.particles.clone(),
            density: w.density,
            intensity: 1.0,
            height: w.height,
            volume: None,
        }),
        ..SceneEffects::default()
    }
}

/// A bone the mock character has, preferring the right hand.
fn hand_bone(stage: &Stage) -> HumanoidBone {
    [HumanoidBone::RightHand, HumanoidBone::RightLowerArm, HumanoidBone::Chest, HumanoidBone::Head]
        .into_iter()
        .find(|b| stage.bone_world(*b).is_some())
        .expect("the mock character has an arm, a chest or a head")
}

fn burst_at(preset: &str, from: EffectOrigin, point: Option<&str>) -> BurstRequest {
    BurstRequest { at: Some(EffectAt::new(from, point)), ..BurstRequest::new(slug(preset)) }
}

/// The batches' system ids as stable keys, so two stages on two renderers compare.
fn keys(stage: &Stage) -> BTreeMap<ParticleSystemId, u64> {
    let effects = stage.effects().unwrap();
    effects.systems().iter().map(|s| (s.id, SystemRecord::new(s.sprite, &s.params, s.weather).key)).collect()
}

fn particle_hash(stage: &Stage) -> [u8; 32] {
    let keys = keys(stage);
    let mut hash = Sha256::new();
    for b in &stage.scene().particles {
        hash.update(keys[&b.system].to_le_bytes());
        for v in b.emitter.to_cols_array() {
            hash.update(v.to_le_bytes());
        }
        hash.update(format!(
            "{:?}{:?}{}{}{}{}{}{:?}{:?}{}",
            b.shape, b.velocity, b.start.0, b.interval, b.first, b.count, b.seed, b.cells, b.colours, b.scale
        ));
    }
    hash.finalize().into()
}

// ---- Bursts and anchors ------------------------------------------------------------------------------------------

#[test]
fn a_burst_requested_between_frames_spawns_next_frame_at_this_frames_bone() {
    let mut renderer = Recorder::new(true);
    let (mut stage, service) = stage(&mut renderer);
    let base = base(&stage);
    let bone = hand_bone(&stage);
    for n in 0..10 {
        stage.tick(&mut renderer, &input(&base, n)).unwrap();
    }
    let request = burst_at("confetti", EffectOrigin::Subject, Some(bone.name()));
    let accepted = service.burst(&request).unwrap();
    assert_eq!(accepted.count, presets::find("confetti").unwrap().burst.as_ref().unwrap().count);
    assert!(stage.scene().particles.is_empty(), "nothing spawns before the next frame");
    stage.tick(&mut renderer, &input(&base, 10)).unwrap();
    let particles = &stage.scene().particles;
    assert_eq!(particles.len(), 1, "one batch: {particles:?}");
    let batch = particles[0];
    let hand = stage.bone_world(bone).unwrap().w_axis.truncate();
    assert!(
        batch.emitter.w_axis.truncate().abs_diff_eq(hand, 1e-5),
        "the emitter is at this frame's {bone:?}: {:?} vs {hand:?}",
        batch.emitter.w_axis
    );
    assert_eq!(batch.seed, accepted.seed);
    assert_eq!(batch.start, Seconds(10.0 / FPS));
    assert!(batch.velocity.length() > 0.0, "the swinging arm gives the burst its motion");
    assert_eq!(
        batch.colours,
        palette_colours(&stage.built().palette),
        "confetti takes the subject's palette plus white"
    );
    let events = service.take_events();
    assert_eq!(events.len(), 1);
    let EffectEventKind::Burst(event) = &events[0].kind else { panic!("a burst event") };
    assert_eq!(event.emitter, batch.emitter.to_cols_array());
    assert_eq!(event.count, batch.count);
    stage.release(&mut renderer).unwrap();
}

#[test]
fn a_camera_top_burst_is_a_strip_above_the_frame_and_a_missing_member_falls_back_with_a_finding() {
    let mut renderer = Recorder::new(true);
    let (mut stage, service) = stage(&mut renderer);
    let base = base(&stage);
    stage.tick(&mut renderer, &input(&base, 0)).unwrap();
    let emote = ChatEmoteRef { id: "1".into(), name: "Kappa".into(), image: None };
    let rain = BurstRequest {
        emotes: vec![emote.clone(), ChatEmoteRef { id: "2".into(), ..emote }],
        ..BurstRequest::new(slug("emote-rain"))
    };
    let accepted = service.burst(&rain).unwrap();
    assert_eq!(accepted.count, 24, "12 per emote");
    service.burst(&burst_at("hit-stars", EffectOrigin::Member("ghost".parse().unwrap()), None)).unwrap();
    stage.tick(&mut renderer, &input(&base, 1)).unwrap();
    let particles = &stage.scene().particles;
    assert_eq!(particles.len(), 2);
    let strip = particles[0];
    let omuzaki_render_api::EmitterShape::Rect { half, .. } = strip.shape else { panic!("a strip: {:?}", strip.shape) };
    assert!(half.x > half.y, "wide and shallow: {half:?}");
    assert!(strip.emitter.w_axis.y > 1.0, "above the frame's top edge at the subject's distance");
    assert_eq!(strip.cells.as_slice(), [0], "every emote draws the placeholder until images are fetched");
    let status = service.status();
    assert!(status.findings.iter().any(|f| f.contains("not on this stage")), "{:?}", status.findings);
    assert!(status.findings.iter().any(|f| f.contains("placeholder")), "{:?}", status.findings);
    let EffectEventKind::Burst(event) = &service.take_events()[0].kind else { panic!("a burst") };
    assert!(matches!(event.shape, BurstShape::Rect { .. }));
    assert_eq!(event.emotes.len(), 2, "the event keeps the emote refs for the take");
    stage.release(&mut renderer).unwrap();
}

#[test]
fn a_renderer_without_particles_accepts_effects_and_draws_none() {
    let mut renderer = Recorder::new(true);
    renderer.particles = false;
    let (mut stage, service) = stage(&mut renderer);
    stage.effects_mut().unwrap().set_scene(&weather("snow"));
    let base = base(&stage);
    service.burst(&BurstRequest::new(slug("confetti"))).unwrap();
    for n in 0..3 {
        stage.tick(&mut renderer, &input(&base, n)).expect("frames render");
    }
    assert_eq!(stage.scene().particles.len(), 0);
    let status = service.status();
    assert!(!status.drawn, "the status says nothing is drawn");
    assert_eq!(status.slots, 0);
    stage.release(&mut renderer).unwrap();
}

// ---- Weather -----------------------------------------------------------------------------------------------------

#[test]
fn weather_is_prewarmed_held_to_its_share_and_the_override_ends_on_time() {
    let mut renderer = Recorder::new(false);
    let (mut stage, service) = stage(&mut renderer);
    stage.effects_mut().unwrap().set_scene(&weather("rain"));
    let base = base(&stage);
    stage.tick(&mut renderer, &input(&base, 0)).unwrap();
    let first = service.status();
    assert_eq!(first.weather, WeatherSource::Scene);
    assert!(first.slots > 100, "the first frame shows rain in full flow: {} slots", first.slots);
    assert!(first.slots <= EffectLimits::live().weather_slots(), "held to the weather share");
    service.live_weather(&LiveWeather::preset(slug("snow"), 1.0, Some(1.0))).unwrap();
    stage.tick(&mut renderer, &input(&base, 1)).unwrap();
    assert_eq!(service.status().weather, WeatherSource::Override);
    for n in 2..40 {
        stage.tick(&mut renderer, &input(&base, n)).unwrap();
    }
    assert_eq!(stage.effects().unwrap().live(), Live::Scene, "the override's second ran out");
    assert_eq!(service.status().weather, WeatherSource::Scene);
    service.live_weather(&LiveWeather::None).unwrap();
    stage.tick(&mut renderer, &input(&base, 40)).unwrap();
    assert_eq!(service.status().weather, WeatherSource::None);
    stage.release(&mut renderer).unwrap();
}

#[test]
fn switching_snow_to_rain_leaves_the_snow_alone_and_releases_its_system_when_it_lands() {
    let mut renderer = Recorder::new(false);
    let (mut stage, service) = stage(&mut renderer);
    stage.effects_mut().unwrap().set_scene(&weather("snow"));
    let base = base(&stage);
    for n in 0..30 {
        stage.tick(&mut renderer, &input(&base, n)).unwrap();
    }
    let snow_system = stage.effects().unwrap().systems().iter().find(|s| s.weather).unwrap().clone();
    let desc = |s: &omuzaki_stage::effects::frame::System| ParticleSystemDesc {
        sprite: match s.sprite {
            SpriteDef::Shape(shape) => Sprite::Shape(shape),
            SpriteDef::Atlas(_) => unreachable!("weather draws shapes"),
        },
        params: s.params.clone(),
    };
    let at = Seconds(1.2);
    let snow_desc = desc(&snow_system);
    let find = |batches: &[SpawnBatch], g: u32| -> Option<SpawnBatch> {
        batches.iter().copied().find(|b| b.system == snow_system.id && b.first <= g && g - b.first < b.count)
    };
    let before: Vec<(u32, _)> = stage
        .scene()
        .particles
        .iter()
        .filter(|b| b.system == snow_system.id)
        .flat_map(|b| (b.first..b.first + b.count).step_by(7))
        .filter_map(|g| Some((g, particles::evaluate(&snow_desc, &find(&stage.scene().particles, g)?, g, at, 1.0)?)))
        .collect();
    assert!(before.len() > 10, "snow is falling: {}", before.len());
    service.live_weather(&LiveWeather::preset(slug("rain"), 1.0, None)).unwrap();
    stage.tick(&mut renderer, &input(&base, 30)).unwrap();
    let mut compared = 0;
    for (g, state) in &before {
        if let Some(b) = find(&stage.scene().particles, *g) {
            // Prune moves a batch's `start` and `first` together, so the age comes out within an f32 rounding.
            let now = particles::evaluate(&snow_desc, &b, *g, at, 1.0).expect("still alive at the same time");
            assert!(now.position.abs_diff_eq(state.position, 1e-5), "flake {g}: {now:?} vs {state:?}");
            assert!(now.velocity.abs_diff_eq(state.velocity, 1e-5), "flake {g}");
            assert!((now.size - state.size).abs() < 1e-6, "flake {g}");
            assert_eq!((now.colour, now.lifetime), (state.colour, state.lifetime), "flake {g}");
            compared += 1;
        }
    }
    assert!(compared > 10, "the snow already born moves exactly as before ({compared} flakes)");
    let rain = presets::find("rain").unwrap();
    let systems = stage.effects().unwrap().systems();
    let rain_system = systems.iter().find(|s| s.weather && s.params == rain.particles).expect("a rain system");
    assert!(stage.scene().particles.iter().any(|b| b.system == rain_system.id), "rain falls with rain's values");
    assert!(systems.iter().any(|s| s.id == snow_system.id), "snow's system lives while its flakes do");
    // Snow lives at most 8 s.
    for n in 31..(31 + 9 * RATE) {
        stage.tick(&mut renderer, &input(&base, n)).unwrap();
    }
    let systems = stage.effects().unwrap().systems();
    assert!(!systems.iter().any(|s| s.id == snow_system.id), "the snow system is released once its last flake landed");
    assert!(systems_live(stage.effects().unwrap(), &stage.scene().particles));
    stage.release(&mut renderer).unwrap();
}

#[test]
fn resolve_puts_auto_weather_over_the_cast_and_the_camera_and_a_box_where_it_says() {
    use omuzaki_stage_api::{SceneDef, SceneSettings, WeatherArea};
    let rain = presets::find("rain").unwrap();
    let mut def = SceneDef::new(SceneSettings::named("rainy"));
    def.weather = rain.weather_def(0.5);
    let cam = camera();
    let fx = SceneEffects::resolve(&def, &BTreeMap::new(), None, Some(&cam), 1.5);
    assert!(fx.auto.max.y >= 2.6 + 1.0 - 1e-4, "the camera's position, grown by a unit: {:?}", fx.auto);
    let w = fx.weather.as_ref().unwrap();
    assert_eq!(w.intensity, 0.5);
    assert!(w.volume.is_none(), "auto is computed per frame from the bounds");
    def.weather.as_mut().unwrap().area =
        WeatherArea::boxed(omuzaki_stage_api::Anchor::Stage, [-2.0, 0.0, -1.0], [2.0, 3.0, 1.0]);
    let fx = SceneEffects::resolve(&def, &BTreeMap::new(), None, Some(&cam), 1.5);
    let volume = fx.weather.unwrap().volume.unwrap();
    assert!(volume.emitter.w_axis.truncate().abs_diff_eq(Vec3::new(0.0, 3.0, 0.0), 1e-6));
    assert_eq!(volume.half.to_array(), [2.0, 1.0]);
    assert_eq!(volume.area_m2(), 8.0);
}

// ---- Allocation --------------------------------------------------------------------------------------------------

#[test]
fn the_frame_allocates_nothing_with_weather_and_bursts_on() {
    let mut renderer = Recorder::new(false);
    let (mut stage, service) = stage(&mut renderer);
    stage.effects_mut().unwrap().set_scene(&weather("snow"));
    let base = base(&stage);
    let bone = hand_bone(&stage);
    let requests = [
        burst_at("confetti", EffectOrigin::Subject, Some(bone.name())),
        burst_at("hit-stars", EffectOrigin::World, None),
        burst_at("emote-rain", EffectOrigin::Camera, Some("top")),
    ];
    // Warm-up: each preset's first use makes its system.
    for (n, request) in requests.iter().enumerate() {
        service.burst(request).unwrap();
        stage.tick(&mut renderer, &input(&base, n as u64)).unwrap();
    }
    let inputs: Vec<FrameInput> = (3..240).map(|n| input(&base, n)).collect();
    let mut total = 0;
    for (i, frame) in inputs.iter().enumerate() {
        if i % 20 == 0 {
            service.burst(&requests[i / 20 % 3]).unwrap();
        }
        let (result, count) = allocations_in(|| stage.tick(&mut renderer, frame));
        result.unwrap();
        total += count;
    }
    assert!(stage.scene().particles.len() > 1);
    assert_eq!(total, 0, "the frame step allocates nothing in steady state");
    service.take_events();
    stage.release(&mut renderer).unwrap();
}

// ---- The scripted minute, checkpoints and seeking ------------------------------------------------------------------

const MINUTE: u64 = 60 * RATE;

/// What the script asks for before frame `n`.
fn script(service: &StageEffects, bone: HumanoidBone, n: u64) {
    if n >= 45 && (n - 45).is_multiple_of(85) && (n - 45) / 85 < 20 {
        let request = match (n - 45) / 85 % 3 {
            0 => burst_at("confetti", EffectOrigin::Subject, Some(bone.name())),
            1 => BurstRequest {
                at: Some(EffectAt { offset: [0.3, 1.2, 0.2], ..EffectAt::new(EffectOrigin::World, None) }),
                seed: Some(u32::try_from(n).unwrap()),
                ..BurstRequest::new(slug("hit-stars"))
            },
            _ => BurstRequest {
                emotes: vec![ChatEmoteRef { id: "7".into(), name: "Wave".into(), image: None }],
                ..BurstRequest::new(slug("emote-rain"))
            },
        };
        service.burst(&request).unwrap();
    }
    if n == 150 {
        service.clear(0.5).unwrap();
    }
    if n == 300 {
        service.live_weather(&LiveWeather::preset(slug("rain"), 0.8, Some(60.0))).unwrap();
    }
}

struct Run {
    images: Vec<[u8; 32]>,
    particles: Vec<[u8; 32]>,
    checkpoints: Vec<(u64, EffectCheckpoint)>,
    events: Vec<EffectEvent>,
}

/// The scripted minute: snow, 20 bursts, a clear at 5 s and a minute of rain from 10 s, with a checkpoint every
/// second (a take writes one every 10 s; the extra ones are for seeking).
fn run_minute(draw: bool) -> Run {
    let mut renderer = Recorder::new(draw);
    let (mut stage, service) = stage(&mut renderer);
    stage.effects_mut().unwrap().set_scene(&weather("snow"));
    let base = base(&stage);
    let bone = hand_bone(&stage);
    let target = stage.target();
    let mut run = Run { images: Vec::new(), particles: Vec::new(), checkpoints: Vec::new(), events: Vec::new() };
    for n in 0..MINUTE {
        script(&service, bone, n);
        stage.tick(&mut renderer, &input(&base, n)).unwrap();
        if draw {
            let image = renderer.read_target(target).unwrap();
            run.images.push(Sha256::digest(&image.rgba8).into());
        }
        run.particles.push(particle_hash(&stage));
        if n % RATE == 0 {
            run.checkpoints.push((n, stage.effects().unwrap().checkpoint(Seconds(n as f64 / FPS))));
        }
    }
    run.events = service.take_events();
    stage.release(&mut renderer).unwrap();
    run
}

#[test]
fn a_scripted_minute_renders_the_same_twice_and_again_from_a_checkpoint_in_a_fresh_stage() {
    let (first, second) = (run_minute(true), run_minute(true));
    assert_eq!(first.images, second.images, "two runs give the same frames");
    assert_eq!(first.particles, second.particles, "and the same batches");
    let bursts = first.events.iter().filter(|e| matches!(e.kind, EffectEventKind::Burst(_))).count();
    assert_eq!(bursts, 20);
    assert_eq!(first.events.len(), 22, "20 bursts, a clear and the override");
    let distinct: std::collections::BTreeSet<_> = first.particles.iter().collect();
    assert!(distinct.len() > 1000, "the effects move: {} distinct frames", distinct.len());

    // From t = 30 s: a fresh renderer and stage, the checkpoint at 30 s, and the events after it.
    let from = 30 * RATE;
    let (_, checkpoint) = first.checkpoints.iter().find(|(n, _)| *n == from).unwrap();
    let mut renderer = Recorder::new(true);
    let (mut stage, service) = stage(&mut renderer);
    stage.effects_mut().unwrap().set_scene(&weather("snow"));
    stage.effects_mut().unwrap().restore(&mut renderer, checkpoint).unwrap();
    let base = base(&stage);
    for n in from + 1..MINUTE {
        let t = n as f64 / FPS;
        for event in first.events.iter().filter(|e| (e.at.0 - t).abs() < 1e-9) {
            service.replay(event).unwrap();
        }
        stage.tick(&mut renderer, &input(&base, n)).unwrap();
        assert_eq!(particle_hash(&stage), first.particles[n as usize], "frame {n} from the checkpoint");
    }
    stage.release(&mut renderer).unwrap();
}

#[test]
fn seeking_from_every_checkpoint_gives_the_log_of_sequential_playback() {
    let run = run_minute(false);
    let mut renderer = Recorder::new(false);
    let (mut stage, service) = stage(&mut renderer);
    stage.effects_mut().unwrap().set_scene(&weather("snow"));
    let base = base(&stage);
    let at = |n: u64| &run.checkpoints.iter().find(|(m, _)| *m == n).unwrap().1;
    let mut seeks = 0;
    // Take checkpoints every 10 s, as a take does; seek to every whole second after each, including backward.
    for from in [50u64, 40, 30, 20, 10, 0].map(|s| s * RATE) {
        for to in (from..MINUTE).step_by(RATE as usize * 7).skip(1) {
            stage.effects_mut().unwrap().restore(&mut renderer, at(from)).unwrap();
            for n in from + 1..=to {
                let t = n as f64 / FPS;
                for event in run.events.iter().filter(|e| (e.at.0 - t).abs() < 1e-9) {
                    service.replay(event).unwrap();
                }
                stage.tick(&mut renderer, &input(&base, n)).unwrap();
            }
            let Some((_, sequential)) = run.checkpoints.iter().find(|(n, _)| *n == to) else { continue };
            let seeked = stage.effects().unwrap().checkpoint(Seconds(to as f64 / FPS));
            assert_eq!(seeked.batches, sequential.batches, "seek {from} -> {to}: batches");
            assert_eq!(seeked.streams, sequential.streams, "seek {from} -> {to}: streams");
            assert_eq!(seeked.live, sequential.live, "seek {from} -> {to}: the override");
            assert!(systems_live(stage.effects().unwrap(), &stage.scene().particles));
            seeks += 1;
        }
    }
    assert!(seeks >= 15, "{seeks} seeks compared");
    service.take_events();
    stage.release(&mut renderer).unwrap();
}
