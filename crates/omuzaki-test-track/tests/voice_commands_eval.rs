//! Voice commands on the real stack (#1447): detection, false triggers and end-of-speech-to-event latency.
//!
//! - **Commands:** 40 lines that say the default English commands (each of the ten ids four ways, with and without
//!   filler words), spoken by Kokoro-82M in all four of its voices (160 utterances), fed to the speech host (Silero VAD
//!   and Whisper base.en through the registry) **in real time**, 10 ms blocks on a wall-clock schedule, with the
//!   [`CommandListener`] reading its events. A line is detected when exactly its command, and nothing else, is
//!   published. Latency is from the block holding the last sample of speech being pushed to the event's publish.
//! - **False triggers:** eight public-domain LibriVox fables (sections 1 to 8 of Aesop volume 3, about 11 minutes, pinned
//!   below by size and SHA-256 and never committed), and 20 near-miss lines in Kokoro's voice that use command words
//!   inside sentences ("I waved at them"), fed as fast as the host takes them.
//!
//! Run with the speech folders (`speech_support`) and `OMUZAKI_TTS_MODELS`; the eight fables go in the
//! `OMUZAKI_SPEECH_DATA` folder. `OMUZAKI_VC_HANGOVER_MS` overrides the segmenter's end-of-speech hangover.
//! `#[ignore]`d: it takes about ten minutes. It prints the report and asserts nothing about the numbers (the issue
//! asks for them whatever they are); a miss over target is filed as a bug.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::too_many_lines
)]

mod speech_support;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use omuzaki_ai_api::ModelRegistry as _;
use omuzaki_audio_api::TapPoint;
use omuzaki_audio_api::tap::VoiceUse;
use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::{EventSink, LiveEvent, PublishError};
use omuzaki_speech::commands::{CommandListener, VoiceCommand, VoiceCommands, default_phrases};
use omuzaki_speech::host::{AudioInput, HostConfig, SpeechEvent, SpeechHost};
use omuzaki_speech::slots::clips::decode_16k;
use omuzaki_speech::slots::tts::{TTS_RATE, TtsCall, Utterance, resample};
use omuzaki_speech::slots::{asr_slot, tts_slot, vad_slot};
use sha2::Digest as _;
use speech_support::{install, registry, tts_folders};

/// The command lines: `(expected id, line)`.
const LINES: &[(&str, &str)] = &[
    ("wave", "Wave!"),
    ("wave", "Wave, please."),
    ("wave", "Say hi!"),
    ("wave", "Can you wave hello?"),
    ("nod", "Nod."),
    ("nod", "Nod your head."),
    ("nod", "Please nod."),
    ("nod", "Okay, nod your head, please."),
    ("shake_head", "Shake your head."),
    ("shake_head", "Shake head."),
    ("shake_head", "Can you shake your head?"),
    ("shake_head", "Shake your head now."),
    ("smile", "Smile!"),
    ("smile", "Big smile!"),
    ("smile", "Smile, please."),
    ("smile", "Okay, smile."),
    ("surprised", "Look surprised!"),
    ("surprised", "Surprise!"),
    ("surprised", "Can you look surprised?"),
    ("surprised", "Look surprised, please."),
    ("sad", "Look sad."),
    ("sad", "Sad face."),
    ("sad", "Can you look sad?"),
    ("sad", "Sad face, please."),
    ("angry", "Look angry!"),
    ("angry", "Angry face!"),
    ("angry", "Look angry now."),
    ("angry", "Could you look angry?"),
    ("spin", "Spin!"),
    ("spin", "Do a spin."),
    ("spin", "Do a spin, please."),
    ("spin", "Okay, spin."),
    ("bow", "Bow."),
    ("bow", "Take a bow."),
    ("bow", "Take a bow, please."),
    ("bow", "Now take a bow."),
    ("clip", "Clip that!"),
    ("clip", "Clip it."),
    ("clip", "Clip that, please."),
    ("clip", "Okay, clip that."),
];

/// Sentences that use command words without being commands.
const NEAR_MISSES: &[&str] = &[
    "I waved at them from across the street.",
    "We should all smile more often, honestly.",
    "That was a really nice bow tie he had on.",
    "The clip that I sent you yesterday was hilarious.",
    "She gave me a little nod when I walked in.",
    "I was so surprised by the ending of that movie.",
    "Don't be sad, we will win the next one.",
    "He looked angry, but he was just tired.",
    "Let me spin up a new server for the game.",
    "Say hi to your mom for me, okay?",
    "I can't stop shaking my head at this play.",
    "Thanks for the follow, welcome to the stream!",
    "We need to clip the hedge before it rains.",
    "That surprise party was the best thing ever.",
    "Okay chat, let's take a short break now.",
    "The wave pool at that park is enormous.",
    "Her smile lit up the whole room.",
    "Take a look at this new character design.",
    "A sad face emoji in chat, really?",
    "You can bow out of the race if you want.",
];

/// The fables: file, size, SHA-256.
const FABLES: &[(&str, u64, &str)] = &[
    ("fables_03_01_aesop_64kb.mp3", 624_349, "71902150fefc0fb6b1af9907a2bb6689f977530ff7b92c4b0446ace387554cba"),
    ("fables_03_02_aesop_64kb.mp3", 702_298, "08b6824f76d24ceeb3bfc340c7af90462dc1a6e7682c4e09a1aacfd3286dae9f"),
    ("fables_03_03_aesop_64kb.mp3", 862_167, "5fbef33ad2523abe9a4cfe2b409a9cb957ecc50a1ac454199966c90fd79661e6"),
    ("fables_03_04_aesop_64kb.mp3", 699_581, "e764191c38f9acf8634acfa32680034b5b7843d4be5fee9d69804514586a8599"),
    ("fables_03_05_aesop_64kb.mp3", 375_663, "12bf43a277d1da9314ab460999e9117339018b72d58f710d33eaaf92dc7f567b"),
    ("fables_03_06_aesop_64kb.mp3", 568_551, "284315f5bb13c75c7c132d4c0c168b8c82a8f684b5868d321b84b3b752a00c0f"),
    ("fables_03_07_aesop_64kb.mp3", 580_463, "547d28d80ae07fbcb95f7abe5cd3c7d9e3985724a7c61c4992ddd69eb4292f11"),
    ("fables_03_08_aesop_64kb.mp3", 833_119, "d991f7f0add95e94c52972fa9e603867a86fa84ec4cfbbaddfa57d18a7cd3863"),
];

const BLOCK: usize = 160;

/// Records each command with the wall time it was published.
#[derive(Default)]
struct Inbox(Mutex<Vec<(String, Instant)>>);

impl EventSink for Inbox {
    fn publish(&self, event: LiveEvent) -> Result<(), PublishError> {
        let id = event.decode::<VoiceCommand>().and_then(Result::ok).map(|c| c.id).unwrap_or_default();
        self.0.lock().unwrap().push((id, Instant::now()));
        Ok(())
    }

    fn producer_dropped(&self, _: u64) {}
}

impl Inbox {
    fn take(&self) -> Vec<(String, Instant)> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

/// The host, its input and the listener, read on the test's thread.
struct Rig {
    host: SpeechHost,
    input: AudioInput,
    listener: CommandListener,
    inbox: Arc<Inbox>,
    /// Utterances started and finished (ended, rejected or transcribed).
    open: i64,
    finals: usize,
    /// The transcripts of the current line, for the misses' report (synthetic speech only).
    heard: Vec<String>,
    /// Whisper's own time per transcript, ms.
    asr_ms: Vec<f64>,
}

impl Rig {
    fn pump(&mut self) {
        while let Some(event) = self.host.try_next_event() {
            match &event {
                SpeechEvent::SpeechStarted { .. } => self.open += 1,
                SpeechEvent::SpeechRejected { .. }
                | SpeechEvent::UtteranceDropped { .. }
                | SpeechEvent::Error { utterance: Some(_), .. } => {
                    self.open -= 1;
                }
                SpeechEvent::Final { text, latency, .. } => {
                    self.asr_ms.push(latency.asr.as_secs_f64() * 1000.0);
                    self.open -= 1;
                    self.finals += 1;
                    self.heard.push(text.clone());
                }
                _ => {}
            }
            self.listener.on_event(&event);
        }
    }

    /// Pushes `audio` paced like a sound card when `real_time`, else as fast as the ring takes it. Returns the wall
    /// time each block was pushed.
    fn feed(&mut self, audio: &[f32], real_time: bool) -> Vec<Instant> {
        let start = Instant::now();
        let mut pushed = Vec::with_capacity(audio.len() / BLOCK + 1);
        for (k, block) in audio.chunks(BLOCK).enumerate() {
            if real_time {
                let due = start + Duration::from_millis(10 * k as u64);
                if let Some(wait) = due.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
            } else {
                while self.input.free() < block.len() {
                    self.pump();
                    std::thread::park_timeout(Duration::from_millis(1));
                }
            }
            assert_eq!(self.input.push(block), block.len(), "the ring overflowed");
            pushed.push(Instant::now());
            self.pump();
        }
        pushed
    }

    /// Feeds silence (in real time when asked) until every utterance started has finished, or 10 s pass.
    fn settle(&mut self, real_time: bool) {
        let silence = vec![0.0f32; BLOCK * 10];
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.open > 0 && Instant::now() < deadline {
            self.feed(&silence, real_time);
        }
        self.pump();
    }
}

fn verified(dir: &std::path::Path, name: &str, size: u64, sha256: &str) -> Option<std::path::PathBuf> {
    let path = dir.join(name);
    let mut bytes = Vec::new();
    std::fs::File::open(&path).ok()?.read_to_end(&mut bytes).ok()?;
    let hex = sha2::Sha256::digest(&bytes).iter().fold(String::new(), |mut hex, b| {
        let _ = write!(hex, "{b:02x}");
        hex
    });
    assert_eq!((bytes.len() as u64, hex.as_str()), (size, sha256), "{name} isn't the pinned file");
    Some(path)
}

/// The sample index after the last one louder than -40 dBFS.
fn speech_end(audio: &[f32]) -> usize {
    audio.iter().rposition(|s| s.abs() > 0.01).map_or(0, |i| i + 1)
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    sorted[((sorted.len() - 1) as f64 * p).round() as usize]
}

#[test]
#[ignore = "about ten minutes on the real models; run by hand for the report"]
fn voice_commands_detection_false_triggers_and_latency() {
    let Some(files) = tts_folders() else { return };
    let data = files.0[2].clone();
    let fables: Vec<_> = FABLES.iter().filter_map(|(n, s, h)| verified(&data, n, *s, h)).collect();
    if fables.len() < FABLES.len() {
        eprintln!("SKIPPED: put Aesop volume 3 sections 1 to 8 in OMUZAKI_SPEECH_DATA");
        return;
    }
    let dir = std::env::temp_dir().join(format!("omuzaki-vc-eval-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let registry = registry(files, &dir);
    install(&registry, &vad_slot(), "silero-vad");
    install(&registry, &asr_slot(), "whisper-base-en");
    let kokoro = install(&registry, &tts_slot(), "kokoro-82m");

    // Synthesise first, so Kokoro never competes with the host for the CPU.
    let voices = ["af_heart", "af_bella", "am_michael", "bf_emma"];
    let mut tts = registry.open(&tts_slot(), Some(&kokoro)).unwrap();
    let mut call = TtsCall::new();
    let mut speak = |line: &str, voice: &str| {
        let pcm = call.speak(tts.as_mut(), &Utterance { voice, ..Utterance::plain(line) }).unwrap().to_vec();
        resample(&pcm, TTS_RATE, 16_000)
    };
    let commands_audio: Vec<(&str, &str, &str, Vec<f32>)> = voices
        .iter()
        .flat_map(|voice| LINES.iter().map(move |(id, line)| (*voice, *id, *line)))
        .map(|(voice, id, line)| (voice, id, line, speak(line, voice)))
        .collect();
    let near_audio: Vec<(&str, Vec<f32>)> =
        NEAR_MISSES.iter().enumerate().map(|(i, line)| (*line, speak(line, voices[i % voices.len()]))).collect();
    drop(tts);

    let hangover = std::env::var("OMUZAKI_VC_HANGOVER_MS").ok().and_then(|v| v.parse().ok());
    let mut config = HostConfig { utterance_queue: 1_024, ..HostConfig::default() };
    if let Some(ms) = hangover {
        config.segmenter.min_silence_ms = ms;
    }
    let (host, input) =
        SpeechHost::open(registry.open(&vad_slot(), None).unwrap(), registry.open(&asr_slot(), None).unwrap(), &config)
            .unwrap();
    host.request_transcripts().keep();
    let commands = VoiceCommands::new(default_phrases());
    commands.set_enabled(true);
    let inbox = Arc::new(Inbox::default());
    let listener = CommandListener::new(commands, inbox.clone(), &TapPoint::Voice(VoiceUse::Listen), Seconds(0.0));
    let mut rig = Rig { host, input, listener, inbox, open: 0, finals: 0, heard: Vec::new(), asr_ms: Vec::new() };

    // Commands, in real time.
    let lead = vec![0.0f32; 4_800];
    let tail = vec![0.0f32; 16_000];
    let mut detected: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut confusion: BTreeMap<(&str, String), usize> = BTreeMap::new();
    let mut latencies = Vec::new();
    let mut per_voice: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    for (voice, id, line, audio) in &commands_audio {
        rig.feed(&lead, true);
        let utterance = [audio.as_slice(), &tail].concat();
        let pushed = rig.feed(&utterance, true);
        rig.settle(true);
        let end_block = speech_end(audio).saturating_sub(1) / BLOCK;
        let spoken_end = pushed[end_block.min(pushed.len() - 1)];
        let got = rig.inbox.take();
        let heard = std::mem::take(&mut rig.heard);
        let ids: Vec<&str> = got.iter().map(|(g, _)| g.as_str()).collect();
        let hit = ids == [*id];
        let entry = detected.entry(id).or_default();
        entry.1 += 1;
        let voice_entry = per_voice.entry(voice).or_default();
        voice_entry.1 += 1;
        if hit {
            entry.0 += 1;
            voice_entry.0 += 1;
            latencies.push(got[0].1.saturating_duration_since(spoken_end).as_secs_f64() * 1000.0);
        } else {
            eprintln!("miss: {voice} {line:?} heard {heard:?}, gave {ids:?}");
        }
        *confusion.entry((id, if ids.is_empty() { "-".to_owned() } else { ids.join("+") })).or_default() += 1;
    }

    let mut asr_ms = std::mem::take(&mut rig.asr_ms);
    asr_ms.sort_by(f64::total_cmp);

    // Near misses and the fables, as fast as the host takes them.
    let mut near_triggers = Vec::new();
    for (line, audio) in &near_audio {
        rig.feed(&[lead.as_slice(), audio, &tail].concat(), false);
        rig.settle(false);
        rig.heard.clear();
        let got = rig.inbox.take();
        if !got.is_empty() {
            near_triggers.push(format!("{line:?} -> {:?}", got.iter().map(|g| g.0.as_str()).collect::<Vec<_>>()));
        }
    }
    let mut fable_seconds = 0.0;
    let mut fable_triggers = Vec::new();
    for path in &fables {
        let audio = decode_16k(path).unwrap();
        fable_seconds += audio.len() as f64 / 16_000.0;
        rig.feed(&audio, false);
        rig.feed(&tail, false);
        rig.settle(false);
        rig.heard.clear();
        fable_triggers.extend(rig.inbox.take().into_iter().map(|g| g.0));
    }
    let stats = rig.host.stats();

    // The report.
    let total: usize = detected.values().map(|d| d.1).sum();
    let hits: usize = detected.values().map(|d| d.0).sum();
    latencies.sort_by(f64::total_cmp);
    eprintln!("== Voice commands, English, {} ms hangover", config.segmenter.min_silence_ms);
    eprintln!("detection: {hits}/{total} = {:.1} %", 100.0 * hits as f64 / total as f64);
    for (id, (h, n)) in &detected {
        eprintln!("  {id:<12} {h}/{n}");
    }
    for (voice, (h, n)) in &per_voice {
        eprintln!("  voice {voice:<10} {h}/{n}");
    }
    eprintln!("confusion (expected -> published):");
    for ((id, got), n) in &confusion {
        if got != id {
            eprintln!("  {id} -> {got}: {n}");
        }
    }
    eprintln!(
        "end of speech to event: p50 {:.0} ms, p95 {:.0} ms, max {:.0} ms (n = {})",
        percentile(&latencies, 0.5),
        percentile(&latencies, 0.95),
        latencies.last().copied().unwrap_or(f64::NAN),
        latencies.len()
    );
    eprintln!(
        "of which Whisper on the command lines: p50 {:.0} ms, p95 {:.0} ms",
        percentile(&asr_ms, 0.5),
        percentile(&asr_ms, 0.95)
    );
    eprintln!(
        "false triggers: {} in {:.1} min of fables ({:.2} per 10 min): {fable_triggers:?}",
        fable_triggers.len(),
        fable_seconds / 60.0,
        fable_triggers.len() as f64 / (fable_seconds / 600.0)
    );
    eprintln!("near misses: {} of {} triggered: {near_triggers:#?}", near_triggers.len(), NEAR_MISSES.len());
    eprintln!("host: {stats:?}");
    eprintln!("finals read: {}", rig.finals);
    rig.host.stop();
    let _ = std::fs::remove_dir_all(&dir);
}
