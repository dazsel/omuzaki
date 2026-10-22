//! Proves the hot paths allocate nothing: `VoiceAnalyzer::push` after construction, `LipSync::step`, and the voice
//! mailbox's publish and poll. Only allocations on the measuring thread count.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{BlendMode, ChannelBuf, Cues, Layer};
use omuzaki_face_api::expression::ExpressionSet;
use omuzaki_track_voice::synth::{self, Vowel};
use omuzaki_track_voice::{LipSync, LipSyncConfig, Viseme, VoiceAnalyzer, VowelProfile, voice_mailbox};

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

    MEASURING.with(|m| m.set(true));
    for chunk in samples.chunks(480) {
        analyzer.push(chunk, |frame| {
            writer.publish(frame);
            frames += 1;
        });
        if let Some(frame) = reader.poll() {
            lip.take(&frame);
        }
        lip.step(0.01, &rest, &mut layer, &mut cues);
    }
    MEASURING.with(|m| m.set(false));

    assert!(frames > 70, "{frames}");
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
}
