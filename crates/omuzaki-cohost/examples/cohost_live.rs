//! The AI co-host live, end to end on this machine (the track spine's exists-when, CLAUDE.md §6): a person speaks to
//! the kit character; the co-host hears them through the local speech recognition (Silero VAD and Whisper, installed
//! through the registry), answers through a local LLM on an OpenAI-compatible server on loopback, speaks through Kokoro
//! on the TTS host, and its face and head move with the reply on the character: the voice through the same lip sync
//! and life cues as the person's microphone, its emotes as held expression triggers on the perform host.
//!
//! The run is real time. An audio thread plays the mock engine's graph in 5 ms blocks on the wall clock and feeds the
//! person's lines (Kokoro in another voice, at 16 kHz) to the speech host as a microphone would; the main thread steps
//! the co-host and the live stage (the perform loop at 60 Hz, on the render mock) against the engine's clock, the
//! program clock. Afterwards the frame inputs the loop produced are drawn again, in order, on the software adapter, so
//! the pictures show exactly what the loop posed.
//!
//! ```text
//! cargo run --release -p omuzaki-cohost --features demo --example cohost_live -- <out folder>
//! ```
//!
//! Needs `OMUZAKI_SPEECH_MODELS`, `OMUZAKI_ORT_CACHE`, `OMUZAKI_SPEECH_DATA` (the install smoke tests' clips) and
//! `OMUZAKI_TTS_MODELS` (the folders the speech tests of `omuzaki-test-track` name: every file is checked against its pinned size and SHA-256, nothing is downloaded), and an
//! OpenAI-compatible server at `OMUZAKI_COHOST_LLM` (default `http://127.0.0.1:8080/v1`) serving
//! `OMUZAKI_COHOST_MODEL`; `OMUZAKI_COHOST_ASR` picks the Whisper (default `whisper-base-en`). Writes the transcript (`transcript.jsonl`), the character's channels at every step
//! (`channels.csv`), what the microphone heard and what the co-host said (`person.wav`, `cohost.wav`) and the
//! performance at 30 frames a second (`frames/`), and prints the numbers.

#![allow(clippy::too_many_lines, reason = "a measurement script: one straight line reads best")]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#![allow(clippy::disallowed_methods, reason = "evidence written to a folder the developer names; not an op")]

use std::collections::VecDeque;
use std::error::Error;
use std::fmt::Write as _;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use omuzaki_ai::{Registry, RegistrySettings};
use omuzaki_ai_api::{
    FetchError, FileSource, ModelRef, ModelRegistry, NoProgress, PinnedFile, Progress, Sha256, SlotRef, Version,
};
use omuzaki_anim::life::Life;
use omuzaki_anim::retarget::ArkitRetarget;
use omuzaki_anim::{BusAnimator, BusAnimatorParams};
use omuzaki_anim_api::{Channel, ChannelBuf, DeviceProfile, HumanoidPose, LifeConfig};
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::{BusSpec, Channels, GraphSpec, SourceRef};
use omuzaki_audio_api::ids::{BusId, DeviceRole, FeedId};
use omuzaki_audio_api::tap::{DropPolicy, TapFormat, TapPoint, TapRate, TapRequest};
use omuzaki_audio_mock::MockEngine;
use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef};
use omuzaki_cohost::stage::PerformStage;
use omuzaki_cohost::{Cohost, CohostThread, attach_voice};
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::time::{Clock, FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_llm::OpenAiClient;
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::provider::{ProviderConfig, ProviderEnv, SecretLabel, SecretStore, SecretValue};
use omuzaki_ops_api::{Actor, OpCtx, find};
use omuzaki_performer::brain::LlmBrain;
use omuzaki_performer::persona::{Persona, Sample};
use omuzaki_performer::show::{Parts, Phase, Show, ShowConfig};
use omuzaki_performer_api::event::{PerformerEvent, TranscriptLine};
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::stage::Stage as _;
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{AlphaEncoding, Camera, Projection, Quality, Renderer, Scene, TargetDesc, budget};
use omuzaki_render_mock::MockRenderer;
use omuzaki_speech::slots::tts::{TTS_RATE, TtsCall, Utterance, resample};
use omuzaki_speech::slots::{asr_slot, tts_slot, vad_slot};
use omuzaki_speech::tts::{FEEDS, TtsHost, TtsVoice};
use omuzaki_speech::{EchoLog, HostConfig, SpeechHost};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::face::{FaceRig, InlineExecutor};
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::ops::render::still::encode_png;
use omuzaki_stage::perform::host::PerformHost;
use omuzaki_stage::perform::input::{NoSource, SourceSlot};
use omuzaki_stage::perform::live::LiveStage;
use omuzaki_stage::perform::{Performer, PerformerParts};
use omuzaki_stage::recipe::LightRig;
use omuzaki_stage_api::ids::MemberId;
use omuzaki_track_voice::LipSyncConfig;
use serde_json::json;
use sha2::Digest as _;

type Result<T, E = Box<dyn Error>> = std::result::Result<T, E>;

/// What the person says, one line per turn.
const LINES: [&str; 2] =
    ["Hey Mochi, say hi to everyone in chat!", "Mochi, what do you think we should build on stream today?"];
/// The person's voice: one of Kokoro's own, not the co-host's.
const PERSON_VOICE: &str = "am_michael";
/// The co-host's voice.
const COHOST_VOICE: &str = "kokoro-82m/af_heart";
/// The microphone's rate and the engine's.
const MIC_RATE: u32 = 16_000;
const ENGINE_RATE: u32 = 48_000;
/// One audio block, 5 ms.
const BLOCK_S: f64 = 0.005;
/// The frames the pictures are drawn at, and their size.
const VIDEO_FPS: f64 = 30.0;
const PICTURE: u32 = 540;
/// A sample louder than this is sound.
const SOUND: f32 = 0.01;
/// Mouth open enough to see.
const OPEN: f32 = 0.05;

// ------------------------------------------------------------------------------------------------ the models

/// Verified files from the prepared folders; never downloads.
struct Folders(Vec<PathBuf>);

impl FileSource for Folders {
    fn get(&self, pin: &PinnedFile, _: &dyn Progress) -> Result<PathBuf, FetchError> {
        let name = pin.urls.first().and_then(|url| url.rsplit('/').next()).unwrap_or_default();
        let Some(path) = self.0.iter().map(|dir| dir.join(name)).find(|p| p.is_file()) else {
            return Err(FetchError::Io { path: name.to_owned(), reason: "not in the prepared folders".into() });
        };
        let io = |e: std::io::Error| FetchError::Io { path: name.to_owned(), reason: e.to_string() };
        let size = std::fs::metadata(&path).map_err(io)?.len();
        if size != pin.size {
            return Err(FetchError::Size { url: name.to_owned(), expected: pin.size, actual: size });
        }
        let mut file = std::fs::File::open(&path).map_err(io)?;
        let mut hasher = sha2::Sha256::new();
        let mut buffer = vec![0u8; 1 << 16];
        loop {
            let read = file.read(&mut buffer).map_err(io)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let actual = Sha256(hasher.finalize().into());
        if actual != pin.sha256 {
            return Err(FetchError::Hash { url: name.to_owned(), expected: pin.sha256, actual });
        }
        Ok(path)
    }
}

fn folder(var: &str) -> Result<PathBuf> {
    std::env::var_os(var).map(PathBuf::from).ok_or_else(|| format!("set {var}").into())
}

fn install(registry: &Registry, slot: &SlotRef, id: &str) -> Result<ModelRef> {
    let model = registry
        .models(Some(slot))
        .into_iter()
        .find(|m| m.model.id.as_str() == id)
        .ok_or_else(|| format!("{id} is not registered for {slot}"))?
        .model;
    registry.install(&model, &NoProgress).map_err(|e| format!("installing {id}: {e}"))?;
    Ok(model)
}

/// No keys: the server is on this machine.
struct NoKeys;

impl SecretStore for NoKeys {
    fn get(&self, _: &SecretLabel) -> Option<SecretValue> {
        None
    }
}

// ------------------------------------------------------------------------------------------------ the character

/// The kit's default character, created through `character.create` as a person or an agent makes it, and built by the
/// caching builder the app runs.
fn kit() -> Result<BuiltCharacter> {
    let mut doc = MemDoc::new();
    let registration = find("character.create").ok_or("character.create is not registered")?;
    let op = (registration.decode)(json!({"id": "kit", "name": "Mochi", "template": "soft-sd", "seed": 7}))
        .map_err(|e| format!("{e:?}"))?;
    let (actor, clock) = (Actor::Person { name: "person".into() }, FakeClock::default());
    let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).map_err(|e| format!("{e:?}"))?;
    doc.apply(&plan.patch).map_err(|e| format!("{e:?}"))?;
    let node = doc.get(&DocPath::parse("characters/kit").map_err(|e| format!("{e:?}"))?).ok_or("no character")?;
    let def = CharacterDef::from_node(&node.clone()).map_err(|e| format!("{e:?}"))?;
    Ok(CachingBuilder::new()
        .build(
            &CharId::new("kit").map_err(|e| format!("{e:?}"))?,
            &def,
            &BuildOptions::default(),
            &BuildServices::default(),
        )
        .map_err(|e| format!("{e}"))?)
}

/// The head and shoulders, from the front, in the studio's viewport light.
fn scene(built: &BuiltCharacter) -> Scene {
    let (lo, hi) = built
        .meshes
        .iter()
        .flat_map(|m| m.mesh.geometry.positions.iter().copied())
        .fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(lo, hi), p| (lo.min(p), hi.max(p)));
    let height = hi.y - lo.y;
    // The upper half: a chibi's head is most of it.
    let centre = Vec3::new(f32::midpoint(lo.x, hi.x), hi.y - 0.3 * height, f32::midpoint(lo.z, hi.z));
    let fov_y_deg = 30.0_f32;
    let distance = 0.36 * height / (0.5 * fov_y_deg.to_radians()).tan();
    let mut scene = Scene::new(Camera::look_at(
        centre + Vec3::new(0.0, 0.0, distance),
        centre,
        Vec3::Y,
        Projection::Perspective { fov_y_deg, near: 0.05, far: 20.0 },
    ));
    if let Ok(rig) = LightRig::load("viewer-studio") {
        if let Ok(lights) = rig.to_lights() {
            scene.lights = lights;
        }
        scene.background = rig.background();
        scene.post = rig.post(false);
    }
    scene
}

/// The performer the app runs: no camera (the co-host has none), the real retarget, life layer and animator.
fn performer(base: &HumanoidPose) -> Performer {
    let parts = PerformerParts {
        source: Box::new(NoSource),
        retarget: Box::new(ArkitRetarget::new(DeviceProfile::Webcam)),
        life: Box::new(Life::new(LifeConfig::new(7))),
        animator: Box::new(BusAnimator::new(BusAnimatorParams::default())),
    };
    Performer::new(parts, ChannelBuf::NEUTRAL, *base)
}

fn stage_on(renderer: &mut dyn Renderer, built: BuiltCharacter, size: u32) -> Result<Stage> {
    let scene = scene(&built);
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::splat(size), alpha: AlphaEncoding::Straight })
        .map_err(|e| format!("{e:?}"))?;
    let options = UploadOptions { face_px_per_m: 2000.0 };
    let mut stage = Stage::new(renderer, built, options, scene, target, Quality::live(), StageClock::live())
        .map_err(|e| format!("{e:?}"))?;
    stage
        .set_face(renderer, FaceRig::standard().with_executor(Box::new(InlineExecutor)))
        .map_err(|e| format!("{e:?}"))?;
    Ok(stage)
}

fn mochi() -> Persona {
    let mut persona = Persona::named("Mochi");
    // What Whisper makes of the name.
    persona.aliases = vec!["Mochie".into(), "Mochee".into(), "Mocchi".into(), "Moochi".into()];
    persona.personality = "Cheerful, curious and a little playful; loves making things with the streamer.".into();
    persona.style = "One or two short spoken sentences. Warm and casual.".into();
    persona.samples = vec![Sample {
        user: "Mochi, say hello!".into(),
        reply: "[emote happy 0.8 hold 2s] Hello everyone, welcome in!".into(),
    }];
    persona
}

// ------------------------------------------------------------------------------------------------ audio

fn bus(id: &str, feeds: &[&str]) -> Result<BusSpec> {
    Ok(BusSpec {
        id: BusId::new(id)?,
        channels: Channels::Mono,
        sources: feeds.iter().map(|f| FeedId::new(*f).map(|id| SourceRef::Feed { id })).collect::<Result<_, _>>()?,
        chain: vec![],
        gain_db: 0.0,
        muted: false,
        sends: vec![],
        outputs: vec![DeviceRole::new("out")?],
    })
}

/// Sound segments of `pcm` at `rate`: `(start, end)` in seconds, joining gaps shorter than `gap`.
fn segments(pcm: &[f32], rate: u32, gap: f64) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (i, s) in pcm.iter().enumerate() {
        if s.abs() < SOUND {
            continue;
        }
        let t = i as f64 / f64::from(rate);
        match out.last_mut() {
            Some(last) if t - last.1 < gap => last.1 = t,
            _ => out.push((t, t)),
        }
    }
    out
}

fn write_wav(path: &Path, pcm: &[f32], rate: u32) -> Result<()> {
    let spec =
        hound::WavSpec { channels: 1, sample_rate: rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut wav = hound::WavWriter::create(path, spec)?;
    for &s in pcm {
        wav.write_sample((s * 32767.0).round().clamp(-32768.0, 32767.0) as i16)?;
    }
    wav.finalize()?;
    Ok(())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

// ------------------------------------------------------------------------------------------------ the run

/// The program clock: the engine's, which the TTS host's play times are on.
struct Program(Arc<omuzaki_audio_api::clock::EngineClock>);

impl Clock for Program {
    fn now(&self) -> Seconds {
        self.0.read().time
    }
}

/// One step of the perform loop as the run saw it.
struct Step {
    t: f64,
    input: FrameInput,
}

fn main() -> Result<()> {
    let out = PathBuf::from(std::env::args().nth(1).ok_or("usage: cohost_live <out folder>")?);
    std::fs::create_dir_all(out.join("frames"))?;
    omuzaki_cohost::link();
    omuzaki_stage::link();
    omuzaki_ai_ort::link();
    omuzaki_speech::link();
    omuzaki_llm::link();

    // The models, through the registry as the app installs them.
    let files = Folders(vec![
        folder("OMUZAKI_SPEECH_MODELS")?,
        folder("OMUZAKI_ORT_CACHE")?,
        folder("OMUZAKI_SPEECH_DATA")?,
        folder("OMUZAKI_TTS_MODELS")?,
    ]);
    let data = std::env::temp_dir().join(format!("omuzaki-cohost-live-{}", std::process::id()));
    std::fs::create_dir_all(&data)?;
    let registry = Registry::new(&data, Arc::new(files), Version::new(0, 1, 0), RegistrySettings::default())?;
    let vad = install(&registry, &vad_slot(), "silero-vad")?;
    let asr_id = std::env::var("OMUZAKI_COHOST_ASR").unwrap_or_else(|_| "whisper-base-en".into());
    let asr = install(&registry, &asr_slot(), &asr_id)?;
    let kokoro = install(&registry, &tts_slot(), "kokoro-82m")?;

    // The person's lines, synthesised before the run so they never compete with it for the CPU.
    let mut person = Vec::new();
    {
        let mut tts = registry.open(&tts_slot(), Some(&kokoro))?;
        let mut call = TtsCall::new();
        for line in LINES {
            let pcm = call.speak(tts.as_mut(), &Utterance { voice: PERSON_VOICE, ..Utterance::plain(line) })?;
            person.push(resample(pcm, TTS_RATE, MIC_RATE));
        }
    }

    // The LLM on loopback.
    let endpoint = std::env::var("OMUZAKI_COHOST_LLM").unwrap_or_else(|_| "http://127.0.0.1:8080/v1".into());
    let model = std::env::var("OMUZAKI_COHOST_MODEL").map_err(|_| "set OMUZAKI_COHOST_MODEL to the served model")?;
    let runtime = omuzaki_llm::runtime::dedicated().map_err(|e| format!("{e}"))?;
    let config = ProviderConfig::new("local", "openai-compatible", endpoint).with_model(model.clone());
    let client =
        OpenAiClient::new(&config, &ProviderEnv::new(Arc::new(NoKeys), runtime)).map_err(|e| format!("{e}"))?;

    // The character on the live stage.
    let built = kit()?;
    let mut base = HumanoidPose::REST;
    seed_base_pose(&built.rig, &mut base);
    let mut mock = MockRenderer::new();
    let stage = stage_on(&mut mock, built, 64)?;
    let mut live =
        LiveStage::new(performer(&base), stage, Arc::new(SourceSlot::new()), Quality::live(), budget::LIVE_GPU)
            .map_err(|e| format!("{e:?}"))?;
    let host: Arc<dyn PerformHost> = Arc::new(live.handle().clone());

    // The co-host: its stage over the perform host, its voice into the lip sync, Kokoro on the mock engine.
    let member = MemberId::new("mochi")?;
    let stage = Arc::new(PerformStage::new(Arc::clone(&host), member.clone()));
    let inventory = stage.inventory(&member).map_err(|e| format!("{e:?}"))?;
    let lead = attach_voice(host.as_ref(), LipSyncConfig::default()).map_err(|e| format!("{e:?}"))?;
    let engine = Arc::new(MockEngine::new());
    let graph = GraphSpec {
        buses: vec![bus("ai_voice", &FEEDS[..2])?, bus("ai_voice_private", &FEEDS[2..])?],
        ..GraphSpec::default()
    };
    engine.apply(&graph).map_err(|e| format!("{e:?}"))?;
    let request = TapRequest {
        point: TapPoint::BusPostFader(BusId::new("ai_voice")?),
        format: TapFormat { rate: TapRate::Graph48k, mono: true },
        capacity: Seconds(80.0),
        policy: DropPolicy::DropNewest,
        consumer: "cohost-live".into(),
        leaves_device: false,
    };
    let mut tap = engine.tap_ring(&request).map_err(|e| format!("{e:?}"))?.0;
    let voice = VoiceId::new(COHOST_VOICE)?;
    let echo = EchoLog::new();
    let mut tts = TtsHost::open(
        engine.clone() as Arc<dyn AudioEngine>,
        registry.open(&tts_slot(), Some(&kokoro))?,
        TtsVoice::new(voice.clone()),
        Some(Box::new(lead.clone())),
    )?;
    tts.spawn_pump()?;
    tts.set_echo(echo.clone());
    let (mut speech, mut input) = SpeechHost::open(
        registry.open(&vad_slot(), Some(&vad))?,
        registry.open(&asr_slot(), Some(&asr))?,
        &HostConfig::default(),
    )?;
    speech.request_transcripts().keep();
    // The engine's first block and the microphone's first sample are both program time zero.
    let ears = speech.ears(Seconds(0.0), echo);
    let mut persona = mochi();
    if model.to_ascii_lowercase().starts_with("qwen3") {
        // Qwen3's own switch: a co-host answers at once instead of reasoning first.
        persona.style.push_str(" /no_think");
    }
    let brain = LlmBrain::new(Arc::new(client) as Arc<dyn LlmClient>, persona.clone());
    let parts = Parts::new(
        Box::new(brain),
        Box::new(ears),
        Box::new(tts),
        Arc::clone(&stage) as Arc<dyn omuzaki_performer_api::stage::Stage>,
    );
    let show = Show::new(&persona, parts, ShowConfig::new(member, voice));
    let cohost = Cohost::new(show, stage, Some(lead));
    eprintln!(
        "ready: {model} at loopback; the character can show {:?}",
        inventory.expressions.iter().map(ToString::to_string).collect::<Vec<_>>()
    );

    // The audio thread: the engine plays and the microphone hears, 5 ms at a time on the wall clock.
    let mic: Arc<Mutex<VecDeque<f32>>> = Arc::default();
    let running = Arc::new(AtomicBool::new(true));
    let audio = {
        let (engine, mic, running) = (Arc::clone(&engine), Arc::clone(&mic), Arc::clone(&running));
        std::thread::Builder::new().name("audio".into()).spawn(move || {
            let mic_block = (f64::from(MIC_RATE) * BLOCK_S) as usize;
            let engine_block = (f64::from(ENGINE_RATE) * BLOCK_S) as usize;
            let (start, mut blocks, mut heard) = (Instant::now(), 0u32, Vec::new());
            let mut buf = vec![0.0f32; mic_block];
            while running.load(Ordering::Acquire) {
                engine.advance(engine_block);
                {
                    let mut queue = lock(&mic);
                    for s in &mut buf {
                        *s = queue.pop_front().unwrap_or(0.0);
                    }
                }
                input.push(&buf);
                heard.extend_from_slice(&buf);
                blocks += 1;
                let due = start + Duration::from_secs_f64(BLOCK_S * f64::from(blocks));
                std::thread::sleep(due.saturating_duration_since(Instant::now()));
            }
            heard
        })?
    };

    // The co-host on its own thread, as the app runs it, on the engine's clock; the main loop runs the stage.
    let clock = engine.clock();
    let (sender, transcript) = std::sync::mpsc::channel();
    let mut cohost = CohostThread::start(cohost, Arc::new(Program(Arc::clone(&clock))), sender)?;
    let wall = Instant::now();
    let (mut lines, mut steps, mut said) = (Vec::<TranscriptLine>::new(), Vec::<Step>::new(), Vec::<f32>::new());
    let mut frame_ms = Vec::new();
    let (mut next, mut cue, mut done) = (0usize, Some(1.0_f64), None::<f64>);
    let mut quiet_since = 0.0_f64;
    let mut buf = vec![0.0f32; 4_800];
    let mut i = 0u64;
    loop {
        let now = clock.read().time;
        lines.extend(transcript.try_iter());
        let started = Instant::now();
        if live.frame(&mut mock, now).map_err(|e| format!("{e:?}"))?.is_some() {
            frame_ms.push(started.elapsed().as_secs_f64() * 1e3);
            steps.push(Step { t: now.0, input: live.input().clone() });
        }
        loop {
            let read = tap.read(&mut buf).len;
            if read == 0 {
                break;
            }
            if buf[..read].iter().any(|s| s.abs() >= SOUND) {
                quiet_since = (said.len() + read) as f64 / f64::from(ENGINE_RATE);
            }
            said.extend_from_slice(&buf[..read]);
        }
        // The script: a line, the co-host's answer, a second of quiet, the next line.
        if let Some(at) = cue
            && now.0 >= at
        {
            lock(&mic).extend(person[next].iter().copied());
            quiet_since = now.0 + person[next].len() as f64 / f64::from(MIC_RATE);
            next += 1;
            cue = None;
        }
        let status = cohost.with(|c, now| c.show().status(now));
        if cue.is_none()
            && done.is_none()
            && status.phase == Phase::Idle
            && status.turns as usize >= next
            && now.0 - quiet_since > 1.5
        {
            if next < person.len() {
                cue = Some(now.0);
            } else {
                done = Some(now.0 + 1.0);
            }
        }
        if done.is_some_and(|end| now.0 >= end) || now.0 > 75.0 {
            break;
        }
        i += 1;
        let due = wall + Duration::from_secs_f64(BLOCK_S * i as f64);
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
    }
    let end = clock.read().time;
    cohost.stop();
    lines.extend(transcript.try_iter());
    running.store(false, Ordering::Release);
    let mic_heard = audio.join().map_err(|_| "the audio thread panicked")?;
    drop(cohost);
    let _ = speech.stop();
    live.into_stage().release(&mut mock).map_err(|e| format!("{e:?}"))?;

    // ---------------------------------------------------------------------------------------------- the record
    let mut transcript = String::new();
    for line in &lines {
        transcript.push_str(&serde_json::to_string(line)?);
        transcript.push('\n');
    }
    std::fs::write(out.join("transcript.jsonl"), &transcript)?;
    let columns = [
        Channel::JAW_OPEN,
        Channel::MOUTH_SMILE_LEFT,
        Channel::MOUTH_FUNNEL,
        Channel::BROW_INNER_UP,
        Channel::EYE_BLINK_LEFT,
        Channel::HEAD_YAW,
        Channel::HEAD_PITCH,
        Channel::HEAD_ROLL,
    ];
    let mut csv = format!("t,{}\n", columns.map(|c| c.info().name).join(","));
    for step in &steps {
        let values: Vec<String> = columns.iter().map(|c| format!("{:.4}", step.input.channels[*c])).collect();
        let _ = writeln!(csv, "{:.4},{}", step.t, values.join(","));
    }
    std::fs::write(out.join("channels.csv"), csv)?;
    write_wav(&out.join("person.wav"), &resample(&mic_heard, MIC_RATE, ENGINE_RATE), ENGINE_RATE)?;
    write_wav(&out.join("cohost.wav"), &said, ENGINE_RATE)?;

    // ---------------------------------------------------------------------------------------------- the numbers
    println!("transcript:");
    for line in &lines {
        println!("  {:>6} ms  {}", line.t, serde_json::to_string(&line.event)?);
    }
    let person_said = segments(&mic_heard, MIC_RATE, 0.6);
    let cohost_said = segments(&said, ENGINE_RATE, 0.6);
    let ms = |t: f64| (t * 1000.0).round();
    let jaw = |step: &Step| step.input.channels[Channel::JAW_OPEN];
    for (turn, &(_, spoke_end)) in person_said.iter().enumerate() {
        let after = |t: u64| t as f64 / 1000.0 >= spoke_end;
        let heard_at =
            lines.iter().find(|l| after(l.t) && matches!(l.event, PerformerEvent::Heard { .. })).map(|l| l.t);
        let thinking =
            lines.iter().find(|l| after(l.t) && matches!(l.event, PerformerEvent::Thinking { .. })).map(|l| l.t);
        let speaking =
            lines.iter().find(|l| after(l.t) && matches!(l.event, PerformerEvent::Speaking { .. })).map(|l| l.t);
        let voice = cohost_said.iter().find(|(start, _)| *start >= spoke_end).copied();
        let mouth = voice.and_then(|(start, _)| steps.iter().find(|s| s.t >= start && jaw(s) > OPEN)).map(|s| s.t);
        println!(
            "turn {}: the person stopped at {} ms; heard (final transcript) +{} ms; turn started +{} ms; first chunk \
             plays +{} ms; first sound +{} ms; mouth open +{} ms after the first sound",
            turn + 1,
            ms(spoke_end),
            heard_at.map_or("-".into(), |t| format!("{}", t as f64 - ms(spoke_end))),
            thinking.map_or("-".into(), |t| format!("{}", t as f64 - ms(spoke_end))),
            speaking.map_or("-".into(), |t| format!("{}", t as f64 - ms(spoke_end))),
            voice.map_or("-".into(), |(start, _)| format!("{}", ms(start - spoke_end))),
            match (voice, mouth) {
                (Some((start, _)), Some(m)) => format!("{}", ms(m - start)),
                _ => "-".into(),
            },
        );
    }
    let speaking = |t: f64| cohost_said.iter().any(|(a, b)| t >= *a && t <= *b + 0.05);
    let (mut open_while, mut while_n, mut open_quiet, mut quiet_n) = (0usize, 0usize, 0usize, 0usize);
    let (mut head_while, mut head_quiet) = (Vec::new(), Vec::new());
    for pair in steps.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let head = |s: &Step| [Channel::HEAD_YAW, Channel::HEAD_PITCH, Channel::HEAD_ROLL].map(|c| s.input.channels[c]);
        let moved = head(a).iter().zip(head(b)).map(|(x, y)| (y - x).abs()).sum::<f32>() / (b.t - a.t).max(1e-6) as f32;
        if speaking(b.t) {
            while_n += 1;
            open_while += usize::from(jaw(b) > OPEN);
            head_while.push(moved);
        } else {
            quiet_n += 1;
            open_quiet += usize::from(jaw(b) > OPEN);
            head_quiet.push(moved);
        }
    }
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len().max(1) as f32;
    let voiced: f64 = cohost_said.iter().map(|(a, b)| b - a).sum();
    println!(
        "mouth: open in {:.0} % of the steps while the co-host's voice plays ({while_n} steps), {:.1} % while it is quiet \
         ({quiet_n} steps)",
        100.0 * open_while as f64 / while_n.max(1) as f64,
        100.0 * open_quiet as f64 / quiet_n.max(1) as f64,
    );
    println!(
        "head: turns {:.3} rad/s on average while speaking, {:.3} rad/s while quiet",
        mean(&head_while),
        mean(&head_quiet)
    );
    frame_ms.sort_by(f64::total_cmp);
    let p = |q: f64| frame_ms.get(((frame_ms.len().max(1) - 1) as f64 * q).round() as usize).copied().unwrap_or(0.0);
    println!(
        "run: {:.1} s of program time, {} display frames (perform loop, rig, springs, CPU face paint on the render mock: \
         p50 {:.1} ms, p95 {:.1} ms), the co-host spoke {voiced:.1} s",
        end.0,
        steps.len(),
        p(0.5),
        p(0.95),
    );

    // ---------------------------------------------------------------------------------------------- the pictures
    let mut gpu = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .map_err(|e| format!("a software adapter (install mesa-vulkan-drivers): {e:?}"))?;
    let mut stage = stage_on(&mut gpu, kit()?, PICTURE)?;
    let target = stage.target();
    let (mut next_frame, mut written) = (0.0_f64, 0u32);
    let drawing = Instant::now();
    for step in &steps {
        stage.tick(&mut gpu, &step.input).map_err(|e| format!("{e:?}"))?;
        if step.t < next_frame {
            continue;
        }
        let image = gpu.read_target(target).map_err(|e| format!("{e:?}"))?;
        let png = encode_png(&image).map_err(|e| format!("{e:?}"))?;
        // A frame stands until the next one, so the pictures keep time with the audio whatever the loop's rate.
        while step.t >= next_frame {
            std::fs::write(out.join("frames").join(format!("{written:05}.png")), &png)?;
            written += 1;
            next_frame += 1.0 / VIDEO_FPS;
        }
    }
    stage.release(&mut gpu).map_err(|e| format!("{e:?}"))?;
    let dir = out.display();
    println!(
        "pictures: {written} frames at {VIDEO_FPS} fps in {:.0} s on the software adapter; to watch:\n  ffmpeg -framerate \
         {VIDEO_FPS} -i {dir}/frames/%05d.png -i {dir}/person.wav -i {dir}/cohost.wav -filter_complex amix=inputs=2 \
         -c:v libx264 -pix_fmt yuv420p {dir}/cohost.mp4",
        drawing.elapsed().as_secs_f64(),
    );
    std::fs::remove_dir_all(&data)?;
    Ok(())
}
