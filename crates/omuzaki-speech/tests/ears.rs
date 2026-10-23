//! The barge-in and privacy signals of the speech host (#1458, `ai-performer.md` §5.4, §5.5), over the energy VAD and a
//! fake recogniser: `Held` fires once, exactly three frames after speech starts, and never on a two-frame blip; a
//! private tap flag reaches `Heard::Final`; a mute drops the speech in progress and reports itself; the ears drop the
//! performer's own echo and count it.

#![allow(clippy::unwrap_used, clippy::float_cmp, clippy::cast_precision_loss)]

mod common;

use std::sync::atomic::Ordering;
use std::time::Duration;

use common::{FakeRecognizer, cat, drain, fake_host, feed};
use omuzaki_audio_api::ring::TapFlags;
use omuzaki_core::time::Seconds;
use omuzaki_performer_api::ears::{Ears, Heard};
use omuzaki_speech::host::SpeechEvent;
use omuzaki_speech::segmenter::FRAME;
use omuzaki_speech::synth::{clicks, silence, tone};
use omuzaki_speech::{EchoLog, HostConfig, SegmenterConfig};

fn config() -> HostConfig {
    HostConfig {
        segmenter: SegmenterConfig { min_silence_ms: 200, ..SegmenterConfig::default() },
        ..HostConfig::default()
    }
}

fn held(events: &[SpeechEvent]) -> Vec<f64> {
    events.iter().filter_map(|e| if let SpeechEvent::Held { at } = e { Some(*at) } else { None }).collect()
}

fn started(events: &[SpeechEvent]) -> Vec<f64> {
    events.iter().filter_map(|e| if let SpeechEvent::SpeechStarted { at } = e { Some(*at) } else { None }).collect()
}

#[test]
fn held_fires_once_per_utterance_exactly_three_frames_after_the_start() {
    let audio = cat(&[&silence(1.0), &tone(0.8, 220.0, 0.3), &silence(1.0), &tone(0.5, 220.0, 0.3), &silence(1.0)]);
    let (mut host, mut input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    feed(&mut input, &audio);
    host.stop();
    let events = drain(&host);
    let (starts, helds) = (started(&events), held(&events));
    assert_eq!((starts.len(), helds.len()), (2, 2), "{events:?}");
    let frame = FRAME as f64 / 16_000.0;
    for (start, held) in starts.iter().zip(&helds) {
        // The start frame, then two more: Held is at the end of the third, 96 ms after the start.
        assert!((held - start - 3.0 * frame).abs() < 1e-9, "start {start}, held {held}");
    }
    let kinds: Vec<&str> = events
        .iter()
        .map(|e| match e {
            SpeechEvent::SpeechStarted { .. } => "started",
            SpeechEvent::Held { .. } => "held",
            SpeechEvent::SpeechEnded { .. } => "ended",
            SpeechEvent::Final { .. } => "final",
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    // Within an utterance: started, held, ended, then (from the other thread) final. The next one may start first.
    let at = |kind: &str| kinds.iter().position(|k| *k == kind).unwrap();
    assert!(at("started") < at("held") && at("held") < at("ended") && at("ended") < at("final"), "{kinds:?}");
}

#[test]
fn a_blip_or_clicks_never_hold() {
    // 25 ms of tone reaches two frames at most; keyboard clicks are single frames.
    let audio =
        cat(&[&silence(0.5), &tone(0.025, 220.0, 0.3), &silence(1.0), &clicks(2.0, 0.25, 0.3, 3), &silence(0.5)]);
    let (mut host, mut input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    feed(&mut input, &audio);
    host.stop();
    let events = drain(&host);
    assert!(held(&events).is_empty(), "{events:?}");
    assert!(!started(&events).is_empty(), "the blip did start: it just never held");
}

#[test]
fn a_private_flag_reaches_the_final_and_the_heard_event() {
    let (mut host, mut input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    let mut ears = host.ears(Seconds(100.0), EchoLog::new());
    feed(&mut input, &cat(&[&silence(0.5), &tone(0.6, 220.0, 0.3), &silence(0.6)]));
    input.set_flags(TapFlags { muted: false, private: true });
    feed(&mut input, &cat(&[&tone(0.6, 220.0, 0.3), &silence(0.6)]));
    input.set_flags(TapFlags::default());
    feed(&mut input, &cat(&[&tone(0.6, 220.0, 0.3), &silence(0.6)]));
    host.stop();
    let heard: Vec<Heard> = std::iter::from_fn(|| ears.next()).collect();
    let finals: Vec<(u64, bool, f64)> = heard
        .iter()
        .filter_map(|h| match h {
            Heard::Final { utterance, private, start, .. } => Some((*utterance, *private, start.0)),
            _ => None,
        })
        .collect();
    assert_eq!(finals.iter().map(|f| (f.0, f.1)).collect::<Vec<_>>(), [(0, false), (1, true), (2, false)]);
    // Times are on the runner clock: the host's first sample is at 100 s, the first tone at 0.5 s less 0.1 s padding.
    assert!((finals[0].2 - 100.4).abs() < 0.065, "{finals:?}");
    let first = |wanted: fn(&Heard) -> bool| heard.iter().position(wanted).unwrap();
    let (onset, held, done) = (
        first(|h| matches!(h, Heard::Onset { .. })),
        first(|h| matches!(h, Heard::Held { .. })),
        first(|h| matches!(h, Heard::Final { .. })),
    );
    assert!(onset < held && held < done, "{heard:?}");
}

#[test]
fn a_mute_drops_the_speech_in_progress_and_reports_itself() {
    let (mut host, mut input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    feed(&mut input, &cat(&[&silence(0.5), &tone(0.3, 220.0, 0.3)]));
    // Muted mid-word: the engine silences the tap and flags it.
    input.set_flags(TapFlags { muted: true, private: false });
    feed(&mut input, &silence(1.0));
    input.set_flags(TapFlags::default());
    feed(&mut input, &cat(&[&silence(0.3), &tone(0.6, 220.0, 0.3), &silence(0.6)]));
    host.stop();
    let events = drain(&host);
    let muted: Vec<bool> =
        events.iter().filter_map(|e| if let SpeechEvent::Muted(m) = e { Some(*m) } else { None }).collect();
    assert_eq!(muted, [true, false], "{events:?}");
    let finals = events.iter().filter(|e| matches!(e, SpeechEvent::Final { .. })).count();
    assert_eq!(finals, 1, "the muted speech is never transcribed: {events:?}");
    // Nothing at all between the mute and the unmute.
    let from = events.iter().position(|e| *e == SpeechEvent::Muted(true)).unwrap();
    assert_eq!(events[from + 1], SpeechEvent::Muted(false), "{events:?}");
}

#[test]
fn the_ears_drop_the_performers_own_echo_and_count_it() {
    // The fake recogniser says "en <samples>": the performer "said" exactly that while the tone played.
    let (mut host, mut input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    let echo = EchoLog::new();
    let mut ears = host.ears(Seconds(0.0), echo.clone());
    let echoes = ears.echoes();
    feed(&mut input, &cat(&[&silence(0.5), &tone(0.6, 220.0, 0.3), &silence(0.6)]));
    host.stop();
    // The utterance runs from its first speech frame (0.48 s) less 100 ms of padding to the end of its last speech frame
    // (1.12 s) plus 100 ms: 0.38 s to 1.22 s, 13,440 samples at 16 kHz.
    echo.said("en 13440", Seconds(0.5), Seconds(1.1));
    let heard: Vec<Heard> = std::iter::from_fn(|| ears.next()).collect();
    assert!(!heard.iter().any(|h| matches!(h, Heard::Final { .. })), "{heard:?}");
    assert_eq!(echoes.load(Ordering::Relaxed), 1);
    assert!(heard.iter().any(|h| matches!(h, Heard::Held { .. })), "the barge-in still fires: {heard:?}");
}
