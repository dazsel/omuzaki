//! Proves the hot paths allocate nothing: `VoiceAnalyzer::push` after construction, `LipSync::step`, the voice
//! mailbox's publish and poll, `CueExtractor::push` with the cue mailbox, and the AI mouth drive's push and poll. Only
//! allocations on the measuring thread count.

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

#[allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]
mod mouth {
    //! The AI mouth drive's push and poll, over a stream fake that allocates nothing itself.

    use std::sync::{Arc, Mutex};

    use omuzaki_ai_api::{Block, BlockSink, BlockSource, RunError, StreamSpec, StreamStats, TensorBuf, TensorRef};
    use omuzaki_ai_mock::{fake_speech, voice_to_face_signature};
    use omuzaki_anim_api::playout::PlayoutDelay;
    use omuzaki_core::time::Seconds;
    use omuzaki_performer_api::voice::{CutProbe, CutSource, CutState, PlayGap, Played};
    use omuzaki_track_voice::ai_mouth::{EnergyVisemes, MouthConfig, MouthDrive};

    use super::{ALLOCATIONS, MEASURING, Ordering};

    struct Jaws {
        values: [f32; 64],
        next: u64,
    }

    struct Sink(Arc<Mutex<Jaws>>);
    struct Source(Arc<Mutex<Jaws>>);

    impl BlockSink for Sink {
        fn push(&mut self, inputs: &[TensorRef<'_>]) -> Result<u64, RunError> {
            let omuzaki_ai_api::TensorData::F32(block) = inputs[0].data else {
                return Err(RunError::Failed(String::new()));
            };
            let power = block.iter().map(|x| x * x).sum::<f32>() / block.len() as f32;
            let mut jaws = self.0.lock().unwrap();
            let seq = jaws.next;
            jaws.values[(seq % 64) as usize] = (2.5 * power.sqrt()).min(1.0);
            jaws.next += 1;
            Ok(seq)
        }

        fn restart(&mut self) {}
    }

    impl BlockSource for Source {
        fn take(&mut self, seq: u64, out: &mut [TensorBuf]) -> Block {
            let jaw = self.0.lock().unwrap().values[(seq % 64) as usize];
            match out[0].resize(&[52]) {
                Ok(omuzaki_ai_api::TensorDataMut::F32(values)) => {
                    values.fill(0.0);
                    values[24] = jaw;
                    Block::Ready
                }
                _ => Block::Missed,
            }
        }

        fn queue(&self) -> u32 {
            1
        }

        fn latency(&self) -> std::time::Duration {
            std::time::Duration::from_millis(20)
        }

        fn stats(&self) -> StreamStats {
            StreamStats::default()
        }
    }

    struct Log {
        records: Vec<Played>,
        now: f64,
    }

    #[derive(Clone)]
    struct Feed(Arc<Mutex<Log>>);

    impl CutSource for Feed {
        fn state(&self) -> CutState {
            CutState::Live
        }

        fn head(&self) -> u64 {
            let log = self.0.lock().unwrap();
            log.records.iter().take_while(|r| r.start.0 <= log.now + 1e-9).count() as u64
        }

        fn record(&self, index: u64) -> Result<Played, PlayGap> {
            let head = self.head();
            if index >= head {
                return Err(PlayGap::NotYet);
            }
            Ok(self.0.lock().unwrap().records[index as usize])
        }
    }

    fn feed(at0: f64, samples: u64) -> Feed {
        let records = (0..samples / 120)
            .map(|i| Played {
                index: i,
                consumed: i * 120,
                n: 120,
                start: Seconds(at0 + i as f64 * 0.005),
                end: Seconds(at0 + (i + 1) as f64 * 0.005),
            })
            .collect();
        Feed(Arc::new(Mutex::new(Log { records, now: 0.0 })))
    }

    #[test]
    fn the_mouth_drive_allocates_nothing_per_block() {
        let jaws = Arc::new(Mutex::new(Jaws { values: [0.0; 64], next: 0 }));
        let spec = StreamSpec { sample_rate: 16_000, block: 320, latency: 0 };
        let mut drive = MouthDrive::new(
            Box::new(Sink(Arc::clone(&jaws))),
            Box::new(Source(jaws)),
            spec,
            TensorBuf::for_outputs(&voice_to_face_signature()),
            Box::new(EnergyVisemes),
            MouthConfig::default(),
        );
        let pcm = fake_speech("Hi chat! Welcome back.", 1.0);
        let mut playout = Box::new(PlayoutDelay::<64>::new());
        let mut released = 0usize;
        // Two replies; only the second is measured (the first sizes the model's output buffer).
        for (reply, at0) in [(0, 1.0), (1, 4.0)] {
            let feed = feed(at0, pcm.len() as u64);
            let probe = CutProbe::new(Arc::new(feed.clone()));
            feed.0.lock().unwrap().now = at0 - 0.2;
            drive.attach(probe);
            MEASURING.with(|m| m.set(reply == 1));
            for (i, slice) in pcm.chunks(240).enumerate() {
                drive.push(Seconds(at0 + i as f64 * 0.01), i as u64 * 240, 24_000, slice, false);
            }
            let mut t = at0 - 0.2;
            while t < at0 + 2.5 {
                feed.0.lock().unwrap().now = t;
                drive.poll(Seconds(t), &mut |stamp, layer| {
                    let _ = playout.push(stamp, *layer);
                    released += 1;
                });
                t += 0.0025;
            }
            MEASURING.with(|m| m.set(false));
        }
        assert!(released > 200, "{released}");
        assert_eq!(drive.stats().refused, 0);
        assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    }
}
