//! Proves the hot paths allocate nothing: `VoiceAnalyzer::push` after construction, `LipSync::step`, the voice
//! mailbox's publish and poll, and `CueExtractor::push` with the cue mailbox. Only allocations on the measuring thread
//! count.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{BlendMode, ChannelBuf, Cues, Layer};
use omuzaki_audio_api::{ClockSnapshot, EngineClock, TapInfo};
use omuzaki_core::time::Seconds;
use omuzaki_face_api::expression::ExpressionSet;
use omuzaki_track_voice::synth::{self, Vowel};
use omuzaki_track_voice::{
    CueExtractor, LipSync, LipSyncConfig, Viseme, VoiceAnalyzer, VowelProfile, cue_mailbox, voice_mailbox,
};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

#[test]
fn analysis_lip_sync_and_the_mailbox_allocate_nothing() {
    let mut samples = synth::vowel(Vowel::of(Viseme::Oh, 150.0, 1.0, 0.4), 48_000, 0.5, 0.002);
    samples.extend(synth::noise(48_000, 0.3, 0.002));
    let mut analyzer = VoiceAnalyzer::new(48_000, VowelProfile::default()).unwrap();
    let (mut writer, mut reader) = voice_mailbox();
    let mut lip = LipSync::new(&ExpressionSet::starter(), LipSyncConfig::default());
    let (rest, mut layer, mut cues) = (ChannelBuf::NEUTRAL, Layer::absent(BlendMode::Max), Cues::default());
    let mut frames = 0;
    let info = TapInfo { first_sample: 0, rate: 48_000, capture_offset: Seconds(0.02), honours_mute: true };
    let clock = Arc::new(EngineClock::new(ClockSnapshot::nominal(48_000)));
    let mut extractor = CueExtractor::new(&info, clock).unwrap();
    let (mut cue_writer, mut cue_reader) = cue_mailbox();
    let mut cue_frames = 0;
    // Warm-up: the extractor's medians fill their windows once.
    extractor.push(0, false, &samples, |_| {});
    let mut at = samples.len() as u64;

    MEASURING.with(|m| m.set(true));
    for (k, chunk) in samples.chunks(480).enumerate() {
        analyzer.push(chunk, |frame| {
            writer.publish(frame);
            frames += 1;
        });
        extractor.push(at, k % 7 == 3, chunk, |frame| {
            cue_writer.publish(frame);
            cue_frames += 1;
        });
        at += chunk.len() as u64;
        if let Some((_, from_tap)) = cue_reader.cues() {
            cues = from_tap;
        }
        if let Some(frame) = reader.poll() {
            lip.take(&frame);
        }
        lip.step(0.01, &rest, &mut layer, &mut cues);
    }
    MEASURING.with(|m| m.set(false));

    assert!(frames > 70, "{frames}");
    assert!(cue_frames > 70, "{cue_frames}");
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
