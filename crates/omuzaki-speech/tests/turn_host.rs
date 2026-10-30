//! The host with turn-taking on ([`HostConfig::conversation`]): speculative transcription at a pause becomes the
//! final when the turn ends there, and is forgotten when the person goes on. Over the fake VAD and recogniser.

mod common;

use std::time::Duration;

use common::{FakeRecognizer, cat, drain, fake_host, feed};
use omuzaki_speech::HostConfig;
use omuzaki_speech::host::SpeechEvent;
use omuzaki_speech::synth::{silence, tone};

/// The fake recogniser's text is `"en <samples>"`: how much audio it was given.
fn final_samples(events: &[SpeechEvent]) -> Vec<usize> {
    events
        .iter()
        .filter_map(|e| match e {
            SpeechEvent::Final { text, .. } => text.split(' ').nth(1).and_then(|n| n.parse().ok()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_turn_that_ends_on_its_first_pause_takes_the_speculative_text() {
    let audio = cat(&[&silence(0.5), &tone(0.8, 220.0, 0.3), &silence(2.0)]);
    let (mut host, mut input) =
        fake_host(HostConfig::conversation(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    feed(&mut input, &audio);
    let stats = host.stop();
    let events = drain(&host);
    let finals = final_samples(&events);
    assert_eq!(finals.len(), 1, "{events:?}");
    assert_eq!((stats.speculations, stats.speculation_hits), (1, 1), "{stats:?}");
    // The speculative cut: the tone, its 100 ms of padding before, and the 96 ms of pause.
    assert!(finals[0] >= 14_000 && finals[0] <= 16_500, "{finals:?}");
}

#[test]
fn a_person_who_goes_on_after_the_pause_gets_one_final_of_everything() {
    let audio = cat(&[&silence(0.5), &tone(0.6, 220.0, 0.3), &silence(0.3), &tone(0.6, 220.0, 0.3), &silence(2.0)]);
    let (mut host, mut input) =
        fake_host(HostConfig::conversation(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    feed(&mut input, &audio);
    let stats = host.stop();
    let events = drain(&host);
    let finals = final_samples(&events);
    assert_eq!(finals.len(), 1, "{events:?}");
    assert_eq!((stats.speculations, stats.speculation_hits), (2, 1), "{stats:?}");
    // Both tones and the pause between them.
    assert!(finals[0] >= 24_000, "{finals:?}");
}

#[test]
fn a_slow_recogniser_still_gives_the_final_once_its_speculation_lands() {
    let audio = cat(&[&silence(0.5), &tone(0.8, 220.0, 0.3), &silence(2.0)]);
    let (mut host, mut input) =
        fake_host(HostConfig::conversation(), Duration::ZERO, FakeRecognizer::new(Duration::from_millis(300)));
    feed(&mut input, &audio);
    let stats = host.stop();
    let events = drain(&host);
    assert_eq!(final_samples(&events).len(), 1, "{events:?}");
    assert_eq!(stats.speculation_hits, 1, "{stats:?}");
}

#[test]
fn without_turn_taking_the_host_behaves_as_before() {
    let audio = cat(&[&silence(0.5), &tone(0.8, 220.0, 0.3), &silence(2.0)]);
    let (mut host, mut input) = fake_host(HostConfig::default(), Duration::ZERO, FakeRecognizer::new(Duration::ZERO));
    feed(&mut input, &audio);
    let stats = host.stop();
    let events = drain(&host);
    assert_eq!(final_samples(&events).len(), 1, "{events:?}");
    assert_eq!(stats.speculations, 0);
    assert!(!events.iter().any(|e| matches!(e, SpeechEvent::Backchannel { .. })));
}
