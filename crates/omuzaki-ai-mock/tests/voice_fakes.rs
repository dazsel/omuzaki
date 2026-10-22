//! The voice fakes give the numbers `ai-performer-voice-face.md` §6 works by hand (#1249).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)] // a panic is a test failure

use std::sync::atomic::Ordering;
use std::time::Duration;

use omuzaki_ai_api::{Block, RunError, Session, TensorBuf, TensorRef};
use omuzaki_ai_mock::{
    ARKIT, FACE_BLOCK, FAKE_TTS_RATE, FakeTts, FakeVoiceToFace, InlineStream, JAW_OPEN, SAMPLES_PER_CHAR, fake_speech,
    jaw_open, tts_signature,
};

fn speak(tts: &mut FakeTts, text: &str, voice: &str, speed: f32) -> Result<Vec<f32>, RunError> {
    let (text, voice) = (text.as_bytes(), voice.as_bytes());
    let (ts, vs) = ([text.len()], [voice.len()]);
    let (speed, expressiveness) = ([speed], [0.5f32]);
    let inputs = [
        TensorRef::u8(text, &ts),
        TensorRef::u8(voice, &vs),
        TensorRef::f32(&speed, &[]),
        TensorRef::f32(&expressiveness, &[]),
    ];
    let mut out = TensorBuf::for_outputs(&tts_signature());
    tts.run(&inputs, &mut out)?;
    Ok(out[0].as_f32().unwrap().to_vec())
}

/// 24 kHz to 16 kHz by linear interpolation: plenty for a 200 Hz tone.
fn to_16k(x: &[f32]) -> Vec<f32> {
    let n = x.len() * 2 / 3;
    (0..n)
        .map(|i| {
            let t = i as f64 * 1.5;
            let (k, f) = (t.floor() as usize, (t - t.floor()) as f32);
            let next = x.get(k + 1).copied().unwrap_or(0.0);
            x[k] * (1.0 - f) + next * f
        })
        .collect()
}

#[test]
fn hi_chat_is_eight_characters_of_60_ms() {
    let mut tts = FakeTts::new();
    let pcm = speak(&mut tts, "Hi chat!", "", 1.0).unwrap();
    assert_eq!(pcm.len(), 11_520);
    assert_eq!(pcm, fake_speech("Hi chat!", 1.0));
    assert_eq!(SAMPLES_PER_CHAR * 1000 / FAKE_TTS_RATE as usize, 60);
    // Speed is honoured: twice as fast is half as long.
    assert_eq!(speak(&mut tts, "Hi chat!", "", 2.0).unwrap().len(), 5_760);
    assert_eq!(tts.calls().load(Ordering::SeqCst), 2);
}

#[test]
fn each_character_opens_the_jaw_as_section_6_says() {
    let mut tts = FakeTts::new();
    let speech = to_16k(&speak(&mut tts, "Hi chat!", "", 1.0).unwrap());
    let face = FakeVoiceToFace::new();
    let spec = face.spec();
    let (mut sink, mut source) = InlineStream::open(Box::new(face), spec, 2, 3);
    let block = FACE_BLOCK as usize;
    let mut out = TensorBuf::for_outputs(&omuzaki_ai_mock::voice_to_face_signature());
    let mut jaw = Vec::new();
    for chunk in speech.chunks_exact(block) {
        let shape = [block];
        let seq = sink.push(&[TensorRef::f32(chunk, &shape)]).unwrap();
        assert_eq!(source.take(seq, &mut out), Block::Ready);
        let channels = out[0].as_f32().unwrap();
        assert_eq!(channels.len(), ARKIT);
        assert!(channels.iter().enumerate().all(|(i, &c)| i == JAW_OPEN || c == 0.0));
        jaw.push(channels[JAW_OPEN]);
    }
    assert_eq!(jaw.len(), 24, "three 20 ms blocks per character");
    let expected = [0.14, 0.53, 0.0, 0.14, 0.14, 0.53, 0.14, 0.0]; // H i ␣ c h a t !
    for (i, (keys, want)) in jaw.chunks(3).zip(expected).enumerate() {
        for &k in keys {
            assert!((k - want).abs() < 0.005, "character {i}: jawOpen {k}, expected {want}");
        }
    }
}

#[test]
fn a_latency_delays_the_envelope_and_missed_blocks_are_scripted() {
    // 160 samples of latency: block 0 depicts [-160, 160), half silence.
    let mut face = FakeVoiceToFace::with_latency(160);
    let tone: Vec<f32> = fake_speech("aaaa", 1.0);
    let speech = to_16k(&tone);
    let mut out = TensorBuf::for_outputs(face.signature());
    let shape = [320];
    face.run(&[TensorRef::f32(&speech[..320], &shape)], &mut out).unwrap();
    let half = out[0].as_f32().unwrap()[JAW_OPEN];
    assert!((half - jaw_open(&speech[..160]) / std::f32::consts::SQRT_2).abs() < 0.01, "{half}");
    face.run(&[TensorRef::f32(&speech[320..640], &shape)], &mut out).unwrap();
    assert!((out[0].as_f32().unwrap()[JAW_OPEN] - 0.53).abs() < 0.005);
    face.reset();
    face.run(&[TensorRef::f32(&speech[..320], &shape)], &mut out).unwrap();
    assert!((out[0].as_f32().unwrap()[JAW_OPEN] - half).abs() < 1e-6, "reset starts again from silence");

    let face = FakeVoiceToFace::new();
    let spec = face.spec();
    let (mut sink, mut source) = InlineStream::late_when(Box::new(face), spec, 2, 3, |seq| seq == 1);
    let mut out = TensorBuf::for_outputs(&omuzaki_ai_mock::voice_to_face_signature());
    let blocks: Vec<Block> = speech
        .chunks_exact(320)
        .take(3)
        .map(|chunk| {
            let seq = sink.push(&[TensorRef::f32(chunk, &shape)]).unwrap();
            source.take(seq, &mut out)
        })
        .collect();
    assert_eq!(blocks, [Block::Ready, Block::Missed, Block::Ready]);
}

#[test]
fn latency_failures_voices_and_the_gate_are_scripted() {
    let mut tts = FakeTts::new().with_voices(&["warm", "bright"]).failing_on([1]);
    assert!(speak(&mut tts, "ok", "warm", 1.0).is_ok());
    assert!(matches!(speak(&mut tts, "ok", "warm", 1.0), Err(RunError::Failed(_))), "call 1 fails");
    assert!(speak(&mut tts, "ok", "", 1.0).is_ok(), "the empty voice is the first");
    assert!(speak(&mut tts, "ok", "nobody", 1.0).is_err());

    let mut slow = FakeTts::new().with_latency(vec![Duration::from_millis(30)]);
    let started = std::time::Instant::now();
    speak(&mut slow, "ok", "", 1.0).unwrap();
    assert!(started.elapsed() >= Duration::from_millis(30));

    let (go, gate) = std::sync::mpsc::channel();
    let mut gated = FakeTts::new().gated(gate);
    let calls = gated.calls();
    let (done_tx, done) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        let pcm = speak(&mut gated, "Hi", "", 1.0).unwrap();
        done_tx.send(pcm.len()).unwrap();
    });
    // The call has started and waits at the gate: nothing is done until the test lets it through.
    while calls.load(Ordering::SeqCst) == 0 {
        std::thread::yield_now();
    }
    assert!(done.try_recv().is_err());
    go.send(()).unwrap();
    assert_eq!(done.recv().unwrap(), 2 * SAMPLES_PER_CHAR);
    worker.join().unwrap();
}
