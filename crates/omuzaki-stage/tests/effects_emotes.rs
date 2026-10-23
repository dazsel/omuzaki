//! Chat emote pictures (particles.md D13; #1276, #814): a burst's emotes are fetched once off the frame thread and then
//! drawn from their own atlas cells; anything refused, failed or switched off draws the placeholder heart without
//! contacting a host off the list; the frame step never waits and allocates nothing while fetches run; a live batch's
//! cells are pinned; the cache is read before the network and kept in bounds; and only the text reference leaves the
//! stage. A fake `Fetcher` stands in for the network.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
// The counting allocator is the one unsafe impl: it forwards to `System` unchanged (the other hot-loop tests do the same).
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

#[path = "frame/double.rs"]
mod double;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Cursor;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use omuzaki_ai_api::{Body, FetchError, Fetcher};
use omuzaki_anim_api::HumanoidPose;
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{Actor, Mode};
use omuzaki_render_api::{
    AlphaEncoding, Camera, CellPick, Projection, Quality, Renderer, Scene, SpawnBatch, TargetDesc,
};
use omuzaki_settings::{Paths, Store};
use omuzaki_settings_api::service::{Layer, SettingsExt};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::effects::emotes::{CACHE_MAX_AGE, EmoteSource, MAX_BYTES, allowed, prune_cache};
use omuzaki_stage::effects::log::EffectLimits;
use omuzaki_stage::effects::service::StageEffects;
use omuzaki_stage::effects::settings::ChatEmotes;
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage_api::effects::EffectService;
use omuzaki_stage_api::{BurstRequest, ChatEmoteRef};

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

/// A small PNG of `colour`.
fn png(colour: [u8; 4]) -> Vec<u8> {
    let picture = image::RgbaImage::from_pixel(32, 16, image::Rgba(colour));
    let mut out = Cursor::new(Vec::new());
    picture.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// What the fake serves at `url`: too many bytes under `/big`, a GIF under `/notpng`, else a PNG.
fn reply(url: &str) -> Vec<u8> {
    if url.ends_with("/big") {
        let mut bytes = png([255, 0, 0, 255]);
        bytes.resize(usize::try_from(MAX_BYTES).unwrap() * 4, 0);
        bytes
    } else if url.ends_with("/notpng") {
        b"GIF89a not a png".to_vec()
    } else {
        png([40, 200, 120, 255])
    }
}

/// A network that records every URL it is asked for and, when gated, answers each only once the test lets it.
#[derive(Default)]
struct FakeNet {
    asked: Mutex<Vec<String>>,
    entered: Mutex<Option<Sender<()>>>,
    gate: Mutex<Option<Receiver<()>>>,
}

impl FakeNet {
    fn asked(&self) -> Vec<String> {
        let mut asked = self.asked.lock().unwrap().clone();
        asked.sort();
        asked
    }
}

impl Fetcher for FakeNet {
    fn open(&self, url: &str, _from: u64) -> Result<Body, FetchError> {
        self.asked.lock().unwrap().push(url.to_owned());
        if let Some(entered) = self.entered.lock().unwrap().as_ref() {
            let _ = entered.send(());
        }
        if let Some(gate) = self.gate.lock().unwrap().as_ref() {
            gate.recv().map_err(|_| FetchError::Network { url: url.to_owned(), reason: "the test ended".into() })?;
        }
        Ok(Body { start: 0, reader: Box::new(Cursor::new(reply(url))) })
    }
}

fn camera() -> Camera {
    Camera::look_at(
        Vec3::new(0.0, 1.0, 2.6),
        Vec3::new(0.0, 0.9, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 20.0 },
    )
}

/// A stage with the mock character, a renderer that keeps textures but draws nothing, and the clock it runs on.
struct Rig {
    renderer: Recorder,
    stage: Option<Stage>,
    service: Arc<StageEffects>,
    pose: HumanoidPose,
    t: f64,
}

impl Rig {
    fn new(source: EmoteSource) -> Self {
        let mut renderer = Recorder::new(false);
        let target = renderer
            .create_target(&TargetDesc { size: UVec2::new(32, 32), alpha: AlphaEncoding::Straight })
            .expect("a target");
        let mut stage = Stage::new(
            &mut renderer,
            built(),
            UploadOptions { face_px_per_m: 600.0 },
            Scene::new(camera()),
            target,
            Quality::offline(),
            StageClock::live(),
        )
        .expect("the stage starts");
        let service = Arc::new(StageEffects::new(3).with_emotes(source));
        stage.attach_effects(Arc::clone(&service), EffectLimits::live());
        let mut pose = HumanoidPose::REST;
        seed_base_pose(&stage.built().rig, &mut pose);
        let mut rig = Self { renderer, stage: Some(stage), service, pose, t: 0.0 };
        rig.tick(0.0);
        rig
    }

    /// One frame `dt` seconds after the last.
    fn tick(&mut self, dt: f64) {
        self.t += dt;
        #[allow(clippy::cast_possible_truncation, reason = "a frame's step")]
        let input = FrameInput { pose: self.pose, dt: dt as f32, time: Some(Seconds(self.t)), ..FrameInput::default() };
        self.stage.as_mut().unwrap().tick(&mut self.renderer, &input).unwrap();
    }

    /// An emote rain for `emotes`, drawn on the next frame; returns the cells its batch drew.
    fn rain(&mut self, emotes: &[ChatEmoteRef]) -> Vec<u16> {
        let request = BurstRequest { emotes: emotes.to_vec(), ..BurstRequest::new(Slug::new("emote-rain").unwrap()) };
        self.service.burst(&request).unwrap();
        let before = self.lists().len();
        self.tick(1.0 / 30.0);
        let lists = self.lists();
        assert_eq!(lists.len(), before + 1, "one new emote batch");
        lists.last().unwrap().clone()
    }

    /// The cells of every live emote batch, oldest first.
    fn lists(&self) -> Vec<Vec<u16>> {
        let batches: &[SpawnBatch] = &self.stage.as_ref().unwrap().scene().particles;
        batches
            .iter()
            .filter(|b| matches!(b.cells, CellPick::List { .. }))
            .map(|b| b.cells.as_slice().to_vec())
            .collect()
    }

    /// Frames at a held time (nothing ages) until every request the frame step made has come back and been uploaded.
    fn settle(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            self.tick(0.0);
            let counts = self.service.emotes().unwrap().counts();
            let back = counts.loaded.load(Ordering::Acquire)
                + counts.missed.load(Ordering::Acquire)
                + counts.dropped.load(Ordering::Acquire);
            if back >= counts.wanted.load(Ordering::Acquire) {
                break;
            }
            assert!(Instant::now() < deadline, "the emote worker answered within 30 s: {counts:?}");
            std::thread::yield_now();
        }
        // At most two uploads a frame, and at most eight pictures in flight.
        for _ in 0..4 {
            self.tick(0.0);
        }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        if let Some(stage) = self.stage.take() {
            let _ = stage.release(&mut self.renderer);
        }
    }
}

fn emote(id: &str, url: &str) -> ChatEmoteRef {
    ChatEmoteRef { id: id.into(), name: format!("emote{id}"), image: Some(url.to_owned()) }
}

fn twitch(id: u32) -> ChatEmoteRef {
    emote(&id.to_string(), &format!("https://static-cdn.jtvnw.net/emoticons/v2/{id}/static/light/2.0"))
}

#[test]
fn a_messages_three_emotes_are_fetched_once_and_then_drawn_from_their_own_cells() {
    let net = Arc::new(FakeNet::default());
    let mut rig = Rig::new(EmoteSource::new(net.clone()));
    let three = [twitch(25), twitch(88), twitch(354)];
    assert_eq!(rig.rain(&three), [0, 0, 0], "not fetched yet: the placeholder");
    let findings = rig.service.status().findings;
    assert!(
        findings.iter().any(|f| f.contains("placeholder")),
        "the status says the placeholder was drawn: {findings:?}"
    );
    rig.settle();
    assert_eq!(net.asked().len(), 3, "each picture fetched once");
    let cells = rig.rain(&three);
    assert!(cells.iter().all(|&c| c != 0), "every emote draws its picture: {cells:?}");
    let mut distinct = cells.clone();
    distinct.sort_unstable();
    distinct.dedup();
    assert_eq!(distinct.len(), 3, "three emotes, three cells: {cells:?}");
    assert_eq!(rig.rain(&three), cells, "the same emotes keep their cells");
    rig.settle();
    assert_eq!(net.asked().len(), 3, "nothing is fetched twice");
}

#[test]
fn a_refused_or_bad_picture_draws_the_placeholder_and_no_host_off_the_list_is_contacted() {
    let net = Arc::new(FakeNet::default());
    let mut rig = Rig::new(EmoteSource::new(net.clone()));
    // A burst refuses a plain http picture outright.
    let plain = [emote("1", "http://static-cdn.jtvnw.net/emoticons/v2/1/static/light/2.0")];
    let request = BurstRequest { emotes: plain.to_vec(), ..BurstRequest::new(Slug::new("emote-rain").unwrap()) };
    assert!(rig.service.burst(&request).is_err(), "an http picture is refused at the request");
    let emotes = [
        emote("2", "https://evil.example/emote.png"),
        emote("3", "https://static-cdn.jtvnw.net/big"),
        emote("4", "https://static-cdn.jtvnw.net/notpng"),
        emote("5", "https://user@static-cdn.jtvnw.net/emoticons/v2/5/static/light/2.0"),
        emote("6", "https://static-cdn.jtvnw.net:8443/emoticons/v2/6/static/light/2.0"),
        emote("7", "https://static-cdn.jtvnw.net.evil.example/emoticons/7.png"),
    ];
    rig.rain(&emotes);
    rig.settle();
    assert_eq!(
        net.asked(),
        ["https://static-cdn.jtvnw.net/big", "https://static-cdn.jtvnw.net/notpng"],
        "only the listed host over HTTPS is contacted"
    );
    assert_eq!(rig.service.emotes().unwrap().counts().missed.load(Ordering::Acquire), 6);
    assert_eq!(rig.rain(&emotes), [0; 6], "every one draws the placeholder");
    // A plugin's declared host is allowed as the first-party ones are.
    let hosts = EmoteSource::new(net).with_host("Emotes.Example.org").hosts;
    assert!(allowed("https://emotes.example.org/a.png", &hosts));
    assert!(allowed("https://yt3.ggpht.com/x=s64:443", &hosts), "a colon in the path is no port");
    assert!(!allowed("https://yt3.ggpht.com:444/x", &hosts));
}

#[test]
fn the_frame_step_never_waits_on_a_fetch_and_allocates_nothing() {
    let net = Arc::new(FakeNet::default());
    let (entered, fetching) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    *net.entered.lock().unwrap() = Some(entered);
    *net.gate.lock().unwrap() = Some(gate);
    let mut rig = Rig::new(EmoteSource::new(net.clone()));
    // Warm-up: the first emote burst makes the system and the atlas.
    rig.rain(&[twitch(1)]);
    fetching.recv_timeout(Duration::from_secs(30)).expect("the worker asks the network");
    // The fetch is held open: a slow server. The frames go on, allocation-free, with bursts asking for more.
    let request = BurstRequest { emotes: vec![twitch(2)], ..BurstRequest::new(Slug::new("emote-rain").unwrap()) };
    let mut total = 0;
    for n in 0..90 {
        if n % 30 == 0 {
            rig.service.burst(&request).unwrap();
        }
        let ((), count) = allocations_in(|| rig.tick(1.0 / 30.0));
        total += count;
    }
    assert_eq!(total, 0, "the frame step allocates nothing while a fetch is open");
    assert!(rig.lists().iter().flatten().all(|&c| c == 0), "everything waiting draws the placeholder");
    release.send(()).unwrap();
    release.send(()).unwrap();
    rig.settle();
    assert!(rig.rain(&[twitch(1)]).iter().all(|&c| c != 0), "the slow picture arrives in the end");
    rig.service.take_events();
}

#[test]
fn with_the_setting_off_nothing_is_fetched_and_switched_on_the_next_emote_is() {
    omuzaki_settings_api::check::assert_crate(env!("CARGO_PKG_NAME"), omuzaki_stage::link);
    let folder = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&Paths::new(folder.path().join("config"), folder.path().join("data"))));
    let person = Actor::Person { name: "test".to_owned() };
    store.set::<ChatEmotes>(&false, Layer::Base, Mode::Apply, &person).unwrap();
    let net = Arc::new(FakeNet::default());
    let mut rig = Rig::new(EmoteSource::new(net.clone()).with_settings(store.clone()));
    let three = [twitch(10), twitch(11), twitch(12)];
    rig.rain(&three);
    rig.settle();
    assert!(net.asked().is_empty(), "off: no fetch at all");
    assert_eq!(rig.rain(&three), [0, 0, 0]);
    // That rain asked again; let those answers come back before the switch.
    rig.settle();
    store.set::<ChatEmotes>(&true, Layer::Base, Mode::Apply, &person).unwrap();
    rig.rain(&[twitch(13)]);
    rig.settle();
    assert_eq!(net.asked(), ["https://static-cdn.jtvnw.net/emoticons/v2/13/static/light/2.0"], "on again: fetched");
}

#[test]
fn a_hundred_emotes_fill_the_atlas_evicting_only_cells_no_live_batch_draws() {
    let net = Arc::new(FakeNet::default());
    let mut rig = Rig::new(EmoteSource::new(net.clone()));
    // 63 emotes, each drawn by a live batch once fetched: every cell beside the placeholder is taken and pinned.
    let first: Vec<ChatEmoteRef> = (0..63).map(twitch).collect();
    let mut drawn = Vec::new();
    for chunk in first.chunks(8) {
        rig.rain(chunk);
        rig.settle();
        drawn.push(rig.rain(chunk));
    }
    assert_eq!(net.asked().len(), 63, "each fetched once");
    let mut cells: Vec<u16> = drawn.iter().flatten().copied().collect();
    assert!(cells.iter().all(|&c| c != 0), "every emote has its picture");
    cells.sort_unstable();
    cells.dedup();
    assert_eq!(cells.len(), 63, "63 distinct cells");
    // While those batches live, a new emote finds no cell, and the old ones keep theirs.
    rig.rain(&[twitch(500)]);
    rig.settle();
    assert_eq!(rig.rain(&[twitch(500)]), [0], "every cell is pinned, so a new emote draws the placeholder");
    rig.settle();
    for (chunk, before) in first.chunks(8).zip(&drawn) {
        assert_eq!(&rig.rain(chunk), before, "a pinned cell keeps its emote");
    }
    // Once the batches have landed, 37 more emotes (101 in all) take the least recently used cells.
    rig.tick(10.0);
    let more: Vec<ChatEmoteRef> = (100..137).map(twitch).collect();
    for chunk in more.chunks(8) {
        rig.rain(chunk);
        rig.settle();
    }
    rig.tick(10.0);
    let shown: Vec<u16> = more.chunks(8).flat_map(|chunk| rig.rain(chunk)).collect();
    assert!(shown.iter().all(|&c| c != 0), "the new emotes took cells: {shown:?}");
    let fetched = net.asked().len();
    rig.tick(10.0);
    for chunk in first.chunks(8) {
        rig.rain(chunk);
        rig.settle();
    }
    assert!(net.asked().len() > fetched, "evicted emotes are fetched again");
}

#[test]
#[allow(clippy::disallowed_methods, reason = "the test makes and reads its own temporary folder")]
fn the_cache_is_read_before_the_network_and_kept_within_its_bounds() {
    let folder = tempfile::tempdir().unwrap();
    let cache = folder.path().join("chat-emotes");
    let two = [twitch(7), twitch(8)];
    {
        let net = Arc::new(FakeNet::default());
        let mut rig = Rig::new(EmoteSource::new(net.clone()).with_cache(cache.clone()));
        rig.rain(&two);
        rig.settle();
        assert_eq!(net.asked().len(), 2);
    }
    let net = Arc::new(FakeNet::default());
    let mut rig = Rig::new(EmoteSource::new(net.clone()).with_cache(cache.clone()));
    rig.rain(&two);
    rig.settle();
    assert!(net.asked().is_empty(), "the second run reads the cache: {:?}", net.asked());
    assert_eq!(rig.service.emotes().unwrap().counts().cached.load(Ordering::Acquire), 2);
    assert!(rig.rain(&two).iter().all(|&c| c != 0));

    // Stale entries go at start, then the oldest until the folder fits.
    let cache = folder.path().join("pruned");
    std::fs::create_dir_all(&cache).unwrap();
    let write = |name: &str, len: usize, age: Duration| {
        let path = cache.join(name);
        std::fs::write(&path, vec![1u8; len]).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
    };
    write("stale.png", 10, CACHE_MAX_AGE + Duration::from_secs(60));
    write("old.png", 100, Duration::from_secs(300));
    write("new.png", 100, Duration::from_secs(5));
    let kept = prune_cache(&cache, CACHE_MAX_AGE, 150);
    assert!(!cache.join("stale.png").exists() && !cache.join("old.png").exists());
    assert!(cache.join("new.png").exists());
    assert!(kept <= 150, "{kept} bytes kept");
}

#[test]
fn only_the_text_reference_leaves_the_stage() {
    let net = Arc::new(FakeNet::default());
    let mut rig = Rig::new(EmoteSource::new(net));
    let one = [twitch(42)];
    rig.rain(&one);
    rig.settle();
    rig.rain(&one);
    let checkpoint = rig.stage.as_ref().unwrap().effects().unwrap().checkpoint(Seconds(rig.t));
    let events = rig.service.take_events();
    let text = serde_json::to_string(&events).unwrap() + &serde_json::to_string(&checkpoint).unwrap();
    assert!(text.contains("https://static-cdn.jtvnw.net/emoticons/v2/42/static/light/2.0"), "the reference is kept");
    assert!(!text.contains("PNG") && !text.contains("IHDR"), "no picture bytes in events or checkpoints");
    assert!(text.len() < 64 * 1024, "{} bytes: no pixels in there", text.len());
}
