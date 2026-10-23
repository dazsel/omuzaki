//! `voice.preview` (§3.4): one sentence in a voice, to `monitor_out` and never to `program`. The preview writes its
//! own feed, [`PREVIEW_FEED`], and refuses to play when the project's audio graph would carry that feed anywhere but
//! `monitor_out`, so a preview can't reach the stream whatever the graph says.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use omuzaki_ai_api::ModelRegistry;
use omuzaki_audio_api::graph::{GraphSpec, MONITOR_OUT_ROLE, SourceRef};
use omuzaki_audio_api::tap::FeedRequest;
use omuzaki_audio_api::{AudioEngine, BusId, DeviceRole, FeedId};
use omuzaki_core::time::Seconds;
use omuzaki_performer_api::limits::CHUNK_MAX_CHARS;
use omuzaki_performer_api::speech::VoiceError;
use omuzaki_performer_api::voice::VoiceAssignment;

use super::open_session;
use crate::slots::tts::{TTS_RATE, TtsCall, Utterance};
use crate::tts::{StyleControls, TtsVoice, trim};

/// The preview's feed. A bus that sources it and outputs only to `monitor_out` plays it.
pub const PREVIEW_FEED: &str = "voice_preview";

/// How long a preview waits for the engine to play what it wrote, beyond the clip's own length.
const PLAY_SLACK: Duration = Duration::from_secs(2);

/// A preview that started: what will be heard, roughly.
#[derive(Clone, Debug, PartialEq)]
pub struct Previewing {
    /// The text spoken.
    pub text: String,
    /// About how long it plays, at the assignment's speed (15 characters a second at speed 1).
    pub seconds: f32,
}

/// Plays one line in a voice to the person's monitor. Implemented by [`TtsPreview`]; the app inserts it as a service.
pub trait VoicePreview: Send + Sync {
    /// Starts speaking `text` in `assignment`'s voice and returns at once; a preview already playing is replaced.
    ///
    /// # Errors
    /// [`VoiceError::Provider`] when nothing would play it on `monitor_out` alone, or the voice's model can't run.
    fn preview(&self, assignment: &VoiceAssignment, text: &str) -> Result<Previewing, VoiceError>;
}

/// Where `feed` ends up in `graph`: the output roles of every bus it reaches through sources and sends.
pub fn reaches(graph: &GraphSpec, feed: &FeedId) -> BTreeSet<DeviceRole> {
    let source = SourceRef::Feed { id: feed.clone() };
    let mut queue: VecDeque<&BusId> =
        graph.buses.iter().filter(|b| b.sources.contains(&source)).map(|b| &b.id).collect();
    let mut seen: BTreeSet<&BusId> = BTreeSet::new();
    let mut roles = BTreeSet::new();
    while let Some(id) = queue.pop_front() {
        if !seen.insert(id) {
            continue;
        }
        let Some(bus) = graph.bus(id) else { continue };
        roles.extend(bus.outputs.iter().cloned());
        queue.extend(bus.sends.iter().map(|s| &s.to));
    }
    roles
}

/// The preview on the app's registry and audio engine.
pub struct TtsPreview {
    registry: Arc<dyn ModelRegistry>,
    engine: Arc<dyn AudioEngine>,
    playing: Mutex<Option<JoinHandle<()>>>,
}

impl TtsPreview {
    /// A preview over `registry`'s `tts@1` models, writing [`PREVIEW_FEED`] on `engine`.
    pub fn new(registry: Arc<dyn ModelRegistry>, engine: Arc<dyn AudioEngine>) -> Self {
        Self { registry, engine, playing: Mutex::new(None) }
    }

    /// Waits for the preview playing now, if any, to end.
    pub fn wait(&self) {
        let handle = self.playing.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    fn feed() -> FeedId {
        FeedId::new(PREVIEW_FEED).unwrap_or_else(|_| unreachable!("a literal feed id"))
    }
}

impl VoicePreview for TtsPreview {
    fn preview(&self, assignment: &VoiceAssignment, text: &str) -> Result<Previewing, VoiceError> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > CHUNK_MAX_CHARS {
            return Err(VoiceError::bad_chunk(text.chars().count()));
        }
        let feed = Self::feed();
        let roles = reaches(&self.engine.graph(), &feed);
        let monitor = DeviceRole::new(MONITOR_OUT_ROLE).unwrap_or_else(|_| unreachable!("a literal role"));
        if roles.is_empty() {
            return Err(VoiceError::Provider(format!(
                "nothing plays the voice preview: add a bus with the `{PREVIEW_FEED}` feed that outputs to {MONITOR_OUT_ROLE}"
            )));
        }
        if let Some(other) = roles.iter().find(|r| **r != monitor) {
            return Err(VoiceError::Provider(format!(
                "the voice preview would reach {other}; it may only play to {MONITOR_OUT_ROLE}"
            )));
        }
        let session = open_session(self.registry.as_ref(), &assignment.voice)?;
        let voice = TtsVoice::of(assignment);
        let rate = voice.feed_rate();
        let speed = voice.model_speed(&StyleControls::of(&[]));
        let (line, name) = (text.to_owned(), assignment.voice.voice().to_owned());
        let engine = Arc::clone(&self.engine);
        let worker = std::thread::Builder::new()
            .name("voice preview".into())
            .spawn(move || {
                let utterance = Utterance { text: &line, voice: &name, speed, ..Utterance::plain(&line) };
                if let Err(e) = play(engine.as_ref(), session, &utterance, rate, &feed) {
                    tracing::warn!(target: "omuzaki_speech::voices", "voice preview: {e}");
                }
            })
            .map_err(|e| VoiceError::Provider(format!("can't start the preview: {e}")))?;
        let previous = self.playing.lock().unwrap_or_else(PoisonError::into_inner).replace(worker);
        if let Some(previous) = previous {
            // The new feed replaced the old one on the engine, so the old preview's wait ends at once.
            let _ = previous.join();
        }
        Ok(Previewing { text: text.to_owned(), seconds: text.chars().count() as f32 / 15.0 / assignment.speed })
    }
}

/// Synthesises `utterance`, writes it into a fresh `feed` at `rate` and keeps the feed open until it has played.
fn play(
    engine: &dyn AudioEngine,
    mut session: Box<dyn omuzaki_ai_api::Session>,
    utterance: &Utterance<'_>,
    rate: u32,
    feed: &FeedId,
) -> Result<(), String> {
    let mut call = TtsCall::new();
    let pcm = call.speak(session.as_mut(), utterance).map_err(|e| e.to_string())?;
    let pcm = trim(pcm, TTS_RATE);
    let length = pcm.len() as f64 / f64::from(rate);
    let request = FeedRequest {
        id: feed.clone(),
        rate,
        mono: true,
        capacity: Seconds(length + 0.5),
        consumer: "voice.preview".into(),
    };
    let mut writer = engine.feed(&request).map_err(|e| e.to_string())?;
    let written = writer.push(pcm);
    let playhead = writer.playhead();
    let deadline = Instant::now() + Duration::from_secs_f64(length) + PLAY_SLACK;
    while Instant::now() < deadline {
        let played = playhead.latest().map_or(0, |r| r.consumed + u64::from(r.n));
        if played >= written as u64 || playhead.cut_at_sample() != u64::MAX {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}
