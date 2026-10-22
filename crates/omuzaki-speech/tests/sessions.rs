//! The host on registry sessions (#1269): `SpeechHost::open` takes a `vad@1` and an `asr@1` session, refuses one of
//! another slot, runs scripted fake sessions to the same events as the trait fakes, and counts a slot's "no answer this
//! time" (`RunError::Unavailable`) instead of reporting it as an error.

// Integration-test helpers may panic (a clear failure) and convert frame counts.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use common::{drain, feed_all};
use omuzaki_ai_api::{DType, RunError, Session, SessionInfo, Signature, TensorBuf, TensorRef};
use omuzaki_ai_mock::FakeSession;
use omuzaki_speech::host::SpeechEvent;
use omuzaki_speech::segmenter::FRAME;
use omuzaki_speech::slots::{asr_signature, vad_signature};
use omuzaki_speech::{HostConfig, SegmenterConfig, SpeechError, SpeechHost};

fn config() -> HostConfig {
    let segmenter = SegmenterConfig { min_silence_ms: 200, ..SegmenterConfig::default() };
    HostConfig { segmenter, ..HostConfig::default() }
}

fn f32s(values: &[f32], shape: &[usize]) -> TensorBuf {
    let mut buf = TensorBuf::new(DType::F32);
    buf.set(&TensorRef::f32(values, shape)).unwrap();
    buf
}

fn text(value: &str) -> TensorBuf {
    let mut buf = TensorBuf::new(DType::U8);
    buf.set(&TensorRef::u8(value.as_bytes(), &[value.len()])).unwrap();
    buf
}

/// A `vad@1` session that answers `probabilities`, one per run, then silence.
fn scripted_vad(probabilities: &[f32]) -> FakeSession {
    let state = vec![0.0f32; 256];
    let script = probabilities.iter().map(|&p| vec![f32s(&[p], &[1, 1]), f32s(&state, &[2, 1, 128])]).collect();
    FakeSession::scripted(vad_signature(), script)
}

/// An `asr@1` session that always hears `said`, in English.
fn scripted_asr(said: &str) -> FakeSession {
    FakeSession::scripted(asr_signature(), vec![vec![text(said), text("en")]])
}

/// 0.5 s of silence, 1 s of speech, 1.5 s of silence, as VAD frames.
fn one_phrase() -> Vec<f32> {
    let frames = |seconds: f32, p: f32| vec![p; (seconds * 16_000.0 / FRAME as f32).round() as usize];
    [frames(0.5, 0.02), frames(1.0, 0.95), frames(1.5, 0.02)].concat()
}

#[test]
fn scripted_sessions_give_a_phrase_and_its_text() {
    let script = one_phrase();
    let (mut host, mut input) =
        SpeechHost::open(Box::new(scripted_vad(&script)), Box::new(scripted_asr("hello chat")), &config()).unwrap();
    host.request_transcripts().keep();
    feed_all(&mut input, &vec![0.0; script.len() * FRAME]);
    let stats = host.stop();
    let events = drain(&host);
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            SpeechEvent::SpeechStarted { .. } => "started",
            SpeechEvent::SpeechEnded { .. } => "ended",
            SpeechEvent::Final { .. } => "final",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["started", "ended", "final"], "{events:?}");
    let SpeechEvent::Final { text, start, end, .. } = &events[2] else { unreachable!() };
    assert_eq!(text, "hello chat");
    // Speech from 0.5 s to 1.5 s, with the segmenter's 100 ms of padding either side.
    assert!((start - 0.4).abs() < 0.07 && (end - 1.6).abs() < 0.07, "{start} {end}");
    assert_eq!((stats.frames, stats.utterances, stats.vad_errors), (script.len() as u64, 1, 0));
}

#[test]
fn a_session_of_another_slot_is_refused() {
    let wrong = FakeSession::scripted(Signature::default(), vec![]);
    let refused = SpeechHost::open(Box::new(wrong), Box::new(scripted_asr("x")), &config()).err();
    assert!(matches!(refused, Some(SpeechError::Signature("vad@1"))), "{refused:?}");
    let swapped = SpeechHost::open(Box::new(scripted_asr("x")), Box::new(scripted_vad(&[0.0])), &config()).err();
    assert!(matches!(swapped, Some(SpeechError::Signature("vad@1"))), "{swapped:?}");
}

/// Answers [`RunError::Unavailable`] for its first `missing` runs (a slot swapping models), then like `inner`.
struct Swapping {
    inner: FakeSession,
    missing: u64,
    runs: Arc<AtomicU64>,
}

impl Session for Swapping {
    fn signature(&self) -> &Signature {
        self.inner.signature()
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        if self.runs.fetch_add(1, Ordering::Relaxed) < self.missing {
            return Err(RunError::Unavailable);
        }
        self.inner.run(inputs, outputs)
    }

    fn reset(&mut self) {
        self.inner.reset();
    }

    fn info(&self) -> SessionInfo {
        self.inner.info()
    }
}

#[test]
fn a_slot_switching_models_is_counted_not_reported() {
    // The VAD misses its first 5 frames (heard as silence) and the recogniser its first utterance (skipped): two
    // phrases give one transcript and no error events.
    let phrase = one_phrase();
    let script = [phrase.clone(), phrase].concat();
    let vad = Swapping { inner: scripted_vad(&script), missing: 5, runs: Arc::default() };
    let asr = Swapping { inner: scripted_asr("second"), missing: 1, runs: Arc::default() };
    let (mut host, mut input) = SpeechHost::open(Box::new(vad), Box::new(asr), &config()).unwrap();
    host.request_transcripts().keep();
    // The fake replays its script from the 6th run on, so the first 5 frames of the stream shift the phrases by
    // 5 frames; pad the end so the second still closes.
    feed_all(&mut input, &vec![0.0; (script.len() + 5) * FRAME]);
    let stats = host.stop();
    let events = drain(&host);
    assert!(!events.iter().any(|e| matches!(e, SpeechEvent::Error { .. })), "{events:?}");
    let finals: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            SpeechEvent::Final { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(finals, ["second"], "{events:?}");
    assert_eq!((stats.vad_unavailable, stats.asr_unavailable, stats.vad_errors), (5, 1, 0), "{stats:?}");
    assert_eq!(stats.utterances, 2);
}
