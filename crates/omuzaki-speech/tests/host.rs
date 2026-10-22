//! The host's threading, events and back-pressure, over a fake VAD and recogniser: no models needed.

mod common;

use std::time::{Duration, Instant};

use common::{FakeRecognizer, cat, drain, fake_host, feed, feed_all};
use omuzaki_speech::host::{ModelPaths, SpeechEvent};
use omuzaki_speech::synth::{silence, tone};
use omuzaki_speech::{HostConfig, SegmenterConfig, SpeechError, SpeechHost};

fn config() -> HostConfig {
    // The short 200 ms hangover keeps the 0.5 s gaps in these tests as separate utterances; the host default (600 ms)
    // is exercised by `the_default_hangover_merges_a_short_pause`.
    let segmenter = SegmenterConfig { min_silence_ms: 200, ..SegmenterConfig::default() };
    HostConfig { threads: 1, segmenter, ..HostConfig::default() }
}

fn finals(events: &[SpeechEvent]) -> Vec<(u64, f64, f64, String)> {
    events
        .iter()
        .filter_map(|e| match e {
            SpeechEvent::Final { utterance, start, end, text, .. } => Some((*utterance, *start, *end, text.clone())),
            _ => None,
        })
        .collect()
}

/// Within two VAD frames (64 ms).
fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 0.065
}

#[test]
fn two_utterances_give_start_end_and_final_events_at_the_right_times() {
    let audio = cat(&[&silence(1.0), &tone(0.8, 220.0, 0.3), &silence(1.0), &tone(0.5, 220.0, 0.3), &silence(1.0)]);
    let (mut host, mut input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    assert_eq!(feed(&mut input, &audio), 0);
    let stats = host.stop();
    let events = drain(&host);

    let onsets: Vec<f64> = events
        .iter()
        .filter_map(|e| if let SpeechEvent::SpeechStarted { at } = e { Some(*at) } else { None })
        .collect();
    assert_eq!(onsets.len(), 2, "{events:?}");
    assert!(near(onsets[0], 1.0) && near(onsets[1], 2.8), "{onsets:?}");

    let done = finals(&events);
    assert_eq!(done.len(), 2, "{events:?}");
    // Each utterance spans its tone plus 100 ms of padding on both sides.
    assert!(near(done[0].1, 0.9) && near(done[0].2, 1.9), "{done:?}");
    assert!(near(done[1].1, 2.7) && near(done[1].2, 3.4), "{done:?}");
    assert_eq!((done[0].0, done[1].0), (0, 1));
    // The fake recogniser echoes the language and the number of samples it was given.
    assert_eq!(done[0].3.split(' ').next(), Some("en"));
    assert_eq!((stats.utterances, stats.utterances_dropped, stats.samples_dropped), (2, 0, 0));
    assert_eq!(stats.frames, audio.len().div_ceil(512) as u64); // the last partial frame is padded
}

#[test]
fn events_arrive_in_order_for_each_utterance() {
    let audio = cat(&[&tone(0.6, 220.0, 0.3), &silence(1.0)]);
    let (mut host, mut input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    feed(&mut input, &audio);
    host.stop();
    let kinds: Vec<&str> = drain(&host)
        .iter()
        .map(|e| match e {
            SpeechEvent::SpeechStarted { .. } => "started",
            SpeechEvent::SpeechEnded { .. } => "ended",
            SpeechEvent::Final { .. } => "final",
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(kinds, ["started", "ended", "final"]);
}

#[test]
fn speech_still_open_when_the_host_stops_is_closed_and_transcribed() {
    let (mut host, mut input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    feed(&mut input, &cat(&[&silence(0.5), &tone(1.0, 220.0, 0.3)]));
    host.stop();
    assert_eq!(finals(&drain(&host)).len(), 1);
}

#[test]
fn a_short_blip_is_rejected_not_transcribed() {
    let audio = cat(&[&silence(0.5), &tone(0.1, 220.0, 0.3), &silence(1.0)]);
    let (mut host, mut input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    feed(&mut input, &audio);
    host.stop();
    let events = drain(&host);
    assert!(events.iter().any(|e| matches!(e, SpeechEvent::SpeechRejected { .. })), "{events:?}");
    assert!(!events.iter().any(|e| matches!(e, SpeechEvent::Final { .. })), "{events:?}");
}

#[test]
fn a_slow_recogniser_overflows_the_utterance_queue_and_the_oldest_are_dropped_and_reported() {
    // Eight 0.4 s utterances, 0.5 s apart, all pushed at once; Whisper takes 150 ms each and only two may wait.
    let one = cat(&[&tone(0.4, 220.0, 0.3), &silence(0.5)]);
    let audio: Vec<f32> = (0..8).flat_map(|_| one.iter().copied()).collect();
    let cfg = HostConfig { utterance_queue: 2, ..config() };
    let (mut host, mut input) = fake_host(cfg, Duration::ZERO, FakeRecognizer::new(Duration::from_millis(150)));
    feed_all(&mut input, &audio);
    let stats = host.stop();
    let events = drain(&host);

    let dropped: Vec<u64> = events
        .iter()
        .filter_map(|e| if let SpeechEvent::UtteranceDropped { utterance, .. } = e { Some(*utterance) } else { None })
        .collect();
    let done: Vec<u64> = finals(&events).iter().map(|f| f.0).collect();
    assert_eq!(stats.utterances, 8);
    assert!(!dropped.is_empty(), "nothing dropped: {events:?}");
    assert_eq!(stats.utterances_dropped, dropped.len() as u64);
    // Every utterance is either transcribed or reported dropped, never both and never neither.
    let mut all: Vec<u64> = dropped.iter().chain(&done).copied().collect();
    all.sort_unstable();
    assert_eq!(all, (0..8).collect::<Vec<_>>());
    // The policy keeps the newest: the last utterance is always transcribed, and what was dropped is older than it.
    assert!(done.contains(&7), "{done:?} / {dropped:?}");
}

#[test]
fn a_stalled_vad_thread_overflows_the_ring_and_the_dropped_samples_are_counted() {
    // 0.1 s of ring against a VAD that needs 5 ms per 32 ms frame, fed 2 s at once.
    let cfg = HostConfig { ring_seconds: 0.1, ..config() };
    let (mut host, mut input) = fake_host(cfg, Duration::from_millis(5), FakeRecognizer::new(Duration::ZERO));
    let refused = feed(&mut input, &silence(2.0));
    assert!(refused > 0, "the ring never filled");
    let stats = host.stop();
    assert_eq!(stats.samples_dropped, refused as u64);
}

#[test]
fn an_application_that_never_polls_loses_the_oldest_events_and_the_count_says_so() {
    let one = cat(&[&tone(0.4, 220.0, 0.3), &silence(0.5)]);
    let audio: Vec<f32> = (0..4).flat_map(|_| one.iter().copied()).collect();
    let cfg = HostConfig { event_queue: 3, ..config() };
    let (mut host, mut input) = fake_host(cfg, Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    feed(&mut input, &audio);
    let stats = host.stop();
    let events = drain(&host);
    assert_eq!(events.len(), 3);
    // 4 utterances x (started, ended) + their finals = 12 events; 3 are kept.
    assert_eq!(stats.events_dropped, 12 - 3);
    assert!(matches!(events.last(), Some(SpeechEvent::Final { utterance: 3, .. })), "{events:?}");
}

#[test]
fn the_language_can_change_while_running() {
    let audio = cat(&[&tone(0.5, 220.0, 0.3), &silence(1.0)]);
    let rec = FakeRecognizer::new(Duration::ZERO);
    let calls = std::sync::Arc::clone(&rec.calls);
    let (mut host, mut input) = fake_host(HostConfig { language: None, ..config() }, Duration::ZERO, rec);
    host.set_language(Some("de".into()));
    feed(&mut input, &audio);
    host.stop();
    assert_eq!(calls.lock().unwrap().iter().map(|c| c.1.clone()).collect::<Vec<_>>(), [Some("de".to_owned())]);
}

#[test]
fn a_failed_transcription_is_reported_and_the_next_utterance_still_works() {
    let one = cat(&[&tone(0.4, 220.0, 0.3), &silence(0.5)]);
    let audio = cat(&[&one, &one]);
    let mut rec = FakeRecognizer::new(Duration::ZERO);
    rec.fail_first = 1;
    let (mut host, mut input) = fake_host(config(), Duration::ZERO, rec);
    feed(&mut input, &audio);
    host.stop();
    let events = drain(&host);
    assert!(events.iter().any(|e| matches!(e, SpeechEvent::Error { utterance: Some(0), .. })), "{events:?}");
    assert_eq!(finals(&events).iter().map(|f| f.0).collect::<Vec<_>>(), [1]);
}

#[test]
fn partial_transcripts_precede_the_final_of_the_same_utterance() {
    let cfg = HostConfig { partial_interval_ms: 250, ..config() };
    let (mut host, mut input) = fake_host(cfg, Duration::ZERO, FakeRecognizer::new(Duration::from_millis(10)));
    // Real-time-ish pacing so the recogniser is idle when each partial is due.
    for block in tone(2.0, 220.0, 0.3).chunks(1600) {
        input.push(block);
        std::thread::sleep(Duration::from_millis(40));
    }
    feed(&mut input, &silence(1.0));
    host.stop();
    let events = drain(&host);
    let final_at = events.iter().position(|e| matches!(e, SpeechEvent::Final { .. })).expect("a final");
    let partials: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, SpeechEvent::Partial { utterance: 0, .. }))
        .map(|(i, _)| i)
        .collect();
    assert!(!partials.is_empty(), "{events:?}");
    assert!(partials.iter().all(|&i| i < final_at));
}

#[test]
fn open_reports_a_missing_model_by_name_and_does_not_panic() {
    let paths = ModelPaths {
        ort_library: "/nonexistent/libonnxruntime.so".into(),
        vad_model: "/nonexistent/silero_vad.onnx".into(),
        whisper_model: "/nonexistent/ggml-base.en.bin".into(),
    };
    let Err(error) = SpeechHost::open(&paths, &HostConfig::default()) else { panic!("opened without models") };
    assert!(matches!(error, SpeechError::ModelMissing { .. }), "{error}");
    let message = error.to_string();
    assert!(
        message.contains("/nonexistent/libonnxruntime.so") && message.contains("onnxruntime-linux-x64-1.28.3"),
        "{message}"
    );
}

#[test]
fn dropping_a_busy_host_aborts_promptly_instead_of_draining() {
    let one = cat(&[&tone(0.4, 220.0, 0.3), &silence(0.5)]);
    let audio: Vec<f32> = (0..4).flat_map(|_| one.iter().copied()).collect();
    let (host, mut input) = fake_host(config(), Duration::ZERO, FakeRecognizer::new(Duration::from_millis(400)));
    feed(&mut input, &audio);
    std::thread::sleep(Duration::from_millis(100));
    let started = Instant::now();
    drop(host);
    assert!(started.elapsed() < Duration::from_millis(900), "drop took {:?}", started.elapsed());
}

fn errors(events: &[SpeechEvent]) -> Vec<&str> {
    events
        .iter()
        .filter_map(|e| if let SpeechEvent::Error { message, .. } = e { Some(message.as_str()) } else { None })
        .collect()
}

#[test]
fn a_vad_that_fails_every_frame_reports_once_and_counts_the_rest() {
    let cfg = HostConfig { event_queue: 8, ..config() };
    let (mut host, mut input) = SpeechHost::start(
        &cfg,
        Box::new(common::BrokenVad { panic: false }),
        Box::new(FakeRecognizer::new(Duration::ZERO)),
    )
    .unwrap();
    feed(&mut input, &silence(3.0)); // 93 frames: one event per frame would overflow the 8-event queue
    let stats = host.stop();
    let events = drain(&host);
    assert_eq!(errors(&events).len(), 1, "{events:?}");
    assert_eq!((stats.vad_errors, stats.events_dropped), (stats.frames, 0));
    assert!(stats.frames >= 93);
}

#[test]
fn a_panicking_vad_is_reported_and_stops_the_host() {
    let (mut host, mut input) = SpeechHost::start(
        &config(),
        Box::new(common::BrokenVad { panic: true }),
        Box::new(FakeRecognizer::new(Duration::ZERO)),
    )
    .unwrap();
    input.push(&silence(0.2));
    let event = host.next_event_timeout(Duration::from_secs(5)).expect("an error event");
    assert!(
        matches!(&event, SpeechEvent::Error { message, .. } if message.contains("vad thread panicked") && message.contains("scripted VAD panic")),
        "{event:?}"
    );
    assert!(!host.is_running());
    assert_eq!(host.stop().workers_failed, 1);
}

#[test]
fn a_panicking_recogniser_is_reported_and_stops_the_host() {
    let (mut host, mut input) = SpeechHost::start(
        &config(),
        Box::new(common::EnergyVad { delay: Duration::ZERO }),
        Box::new(common::PanickingRecognizer),
    )
    .unwrap();
    host.request_transcripts().keep();
    feed(&mut input, &cat(&[&tone(0.5, 220.0, 0.3), &silence(1.0)]));
    let event = std::iter::from_fn(|| host.next_event_timeout(Duration::from_secs(5)))
        .find(|e| matches!(e, SpeechEvent::Error { .. }))
        .expect("an error event");
    assert!(
        matches!(&event, SpeechEvent::Error { message, .. } if message.contains("asr thread panicked")),
        "{event:?}"
    );
    assert_eq!(host.stop().workers_failed, 1);
    assert!(!host.is_running());
}

#[test]
fn a_missing_whisper_model_names_its_own_catalogue_entry() {
    let paths = ModelPaths {
        ort_library: "/nonexistent/lib.so".into(),
        vad_model: "/nonexistent/silero_vad.onnx".into(),
        whisper_model: "/nonexistent/ggml-small.en.bin".into(),
    };
    let dir = std::env::temp_dir();
    let present = |name: &str| {
        let p = dir.join(format!("omuzaki-audio-test-{}-{name}", std::process::id()));
        std::fs::write(&p, b"x").unwrap();
        p
    };
    let paths = ModelPaths { ort_library: present("lib"), vad_model: present("vad"), ..paths };
    let error = SpeechHost::open(&paths, &HostConfig::default()).err().expect("missing model");
    for p in [&paths.ort_library, &paths.vad_model] {
        let _ = std::fs::remove_file(p);
    }
    assert!(error.to_string().contains("whisper-small.en"), "{error}");
}

/// Two 0.4 s tones with a `gap` seconds of silence between them, then a second of silence to close the last one.
fn two_phrases(gap: f32) -> Vec<f32> {
    cat(&[&silence(0.5), &tone(0.4, 220.0, 0.3), &silence(gap), &tone(0.4, 220.0, 0.3), &silence(1.0)])
}

#[test]
fn without_a_consumer_the_host_only_listens() {
    let (mut host, mut input) = SpeechHost::start(
        &config(),
        Box::new(common::EnergyVad { delay: Duration::ZERO }),
        Box::new(FakeRecognizer::new(Duration::ZERO)),
    )
    .unwrap();
    feed(&mut input, &two_phrases(0.8));
    let stats = host.stop();
    let events = drain(&host);
    // Barge-in and the speaking indicator still work...
    assert_eq!(events.iter().filter(|e| matches!(e, SpeechEvent::SpeechStarted { .. })).count(), 2, "{events:?}");
    assert_eq!(events.iter().filter(|e| matches!(e, SpeechEvent::SpeechEnded { .. })).count(), 2, "{events:?}");
    // ...but Whisper never ran and nothing was queued for it.
    assert!(finals(&events).is_empty(), "{events:?}");
    assert_eq!((stats.utterances, stats.utterances_skipped, stats.asr_busy), (0, 2, Duration::ZERO));
}

#[test]
fn transcription_follows_the_claims_and_releases_with_the_last_one() {
    let (mut host, mut input) = SpeechHost::start(
        &config(),
        Box::new(common::EnergyVad { delay: Duration::ZERO }),
        Box::new(FakeRecognizer::new(Duration::ZERO)),
    )
    .unwrap();
    let first = host.request_transcripts();
    let second = first.clone();
    feed(&mut input, &two_phrases(0.8));
    drop(first); // one claim left: still on
    // The workers are asynchronous: wait until both transcripts are out before releasing the last claim.
    let mut done = Vec::new();
    let waiting = Instant::now();
    while done.len() < 2 && waiting.elapsed() < Duration::from_secs(5) {
        if let Some(event) = host.next_event_timeout(Duration::from_millis(50)) {
            done.extend(finals(&[event]));
        }
    }
    assert_eq!(done.len(), 2, "{done:?}");
    drop(second); // none left: the next utterance is only listened to
    feed(&mut input, &two_phrases(0.8));
    let stats = host.stop();
    assert_eq!(finals(&drain(&host)).len(), 0);
    assert_eq!((stats.utterances, stats.utterances_skipped), (2, 2));
}

#[test]
fn a_longer_hangover_merges_phrases_separated_by_a_short_pause_into_one_whisper_call() {
    let run = |min_silence_ms: u32| {
        let segmenter = SegmenterConfig { min_silence_ms, ..SegmenterConfig::default() };
        let (mut host, mut input) =
            fake_host(HostConfig { segmenter, ..config() }, Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
        feed(&mut input, &two_phrases(0.4));
        host.stop().utterances
    };
    assert_eq!(run(200), 2, "a 0.4 s pause splits at the 200 ms hangover");
    assert_eq!(run(600), 1, "and joins at 600 ms");
}

#[test]
fn the_default_hangover_merges_a_short_pause() {
    let (mut host, mut input) = SpeechHost::start(
        &HostConfig::default(),
        Box::new(common::EnergyVad { delay: Duration::ZERO }),
        Box::new(FakeRecognizer::new(Duration::ZERO)),
    )
    .unwrap();
    host.request_transcripts().keep();
    feed(&mut input, &two_phrases(0.4));
    assert_eq!(HostConfig::default().segmenter.min_silence_ms, omuzaki_speech::DEFAULT_HANGOVER_MS);
    assert_eq!(host.stop().utterances, 1);
}
