//! Recording end to end: frames pushed into the stream service come out as a take on disk, with their alpha, the
//! audio tracks beside them and a manifest that adds up; and the `record.*` ops drive it.

// Integration-test helpers may panic: a panic is a clear test failure. The picture maths casts small known values,
// the WAV samples compared are written exactly (0.5, 0.25, 0.0 are exact in f32), and the test reads its own temporary
// folder.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    clippy::float_cmp,
    clippy::disallowed_methods
)]

use std::path::Path;
use std::sync::Arc;

use omuzaki_audio_api::ring::TapRead;
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::Mode;
use omuzaki_ops_api::{Actor, OpCtx, Services, ServicesBuilder, find};
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_settings_api::service::{Layer, Settings, SettingsExt as _};
use omuzaki_stream::alpha::unpremultiply_u8;
use omuzaki_stream::record::runtime::{AudioTrack, RecordPlan, TrackStart};
use omuzaki_stream::record::settings::{Dir, RecordFormat};
use omuzaki_stream::record::take::{GapCause, TakeReport};
use omuzaki_stream::service::{Program, PushOutcome, StreamService};
use omuzaki_stream::timeline::AudioSource;
use omuzaki_stream_api::time::Ticks;
use serde_json::{Value, json};

/// Plays scripted runs once each, whole (the test's buffers are larger than any run).
struct Runs(std::collections::VecDeque<(TapRead, Vec<f32>)>);

impl AudioSource for Runs {
    fn read(&mut self, out: &mut [f32]) -> TapRead {
        let Some((mut run, samples)) = self.0.pop_front() else { return TapRead::default() };
        out[..samples.len()].copy_from_slice(&samples);
        run.len = samples.len();
        run
    }
}

const SIZE: UVec2 = UVec2::new(64, 36);
const FRAME_NS: i64 = 1_000_000_000 / 30;

/// A premultiplied test picture: a soft disc whose alpha falls from 255 at the centre to 0 at the rim, its colour
/// changing with `n` so each frame is told apart by its centre pixel.
fn picture(n: u8) -> Image {
    let mut rgba8 = Vec::with_capacity((SIZE.x * SIZE.y * 4) as usize);
    for y in 0..SIZE.y {
        for x in 0..SIZE.x {
            let (dx, dy) = (x as f32 - 32.0, y as f32 - 18.0);
            let a = (255.0 * (1.0 - (dx * dx + dy * dy).sqrt() / 18.0)).clamp(0.0, 255.0) as u32;
            let straight = [n as u32, 200, 40];
            for c in straight {
                rgba8.push(((c * a + 127) / 255) as u8);
            }
            rgba8.push(a as u8);
        }
    }
    Image { size: SIZE, alpha: AlphaEncoding::Premultiplied, rgba8 }
}

/// Waits (with a deadline, never a fixed sleep) until the service has fanned out `n` frames.
fn wait_pumped(service: &StreamService, n: u64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while service.counters().pumped < n {
        assert!(std::time::Instant::now() < deadline, "the pump fanned out {:?}", service.counters());
        std::thread::yield_now();
    }
}

fn push_frames(service: &StreamService, count: u8) {
    for n in 0..count {
        let outcome = service.push(&picture(n), Ticks(i64::from(n) * FRAME_NS));
        assert_eq!(outcome, PushOutcome::Queued);
        // One frame at a time, as a 30 fps loop does: the pump takes each before the next arrives.
        wait_pumped(service, u64::from(n) + 1);
    }
}

fn png_pixels(path: &Path) -> Vec<u8> {
    image::open(path).unwrap().to_rgba8().into_raw()
}

#[test]
fn a_take_lands_on_disk_with_alpha_audio_and_a_manifest() {
    let dir = tempfile::tempdir().unwrap();
    let service = StreamService::new().unwrap();
    service.set_program(Program { size: SIZE, fps: (30, 1) });
    assert_eq!(service.push(&picture(0), Ticks(0)), PushOutcome::Idle, "nothing is copied while nothing runs");

    // 0.5 s of mic at 1 kHz mono, with a muted stretch and a lost stretch, lined up to start at stream position 100.
    let mic = Runs(std::collections::VecDeque::from([
        (TapRead { start_sample: 50, ..TapRead::default() }, vec![0.5; 250]),
        (TapRead { start_sample: 300, muted: true, ..TapRead::default() }, vec![0.9; 100]),
        (TapRead { start_sample: 450, gap_before: 50, ..TapRead::default() }, vec![0.25; 150]),
    ]));
    let track = AudioTrack {
        name: "mic".into(),
        source: Box::new(mic),
        rate: 1000,
        channels: 1,
        start: TrackStart::Sample(100),
    };
    let started = service
        .record_start(RecordPlan {
            dir: dir.path().to_owned(),
            stem: "take".into(),
            format: RecordFormat::PngSequence,
            tracks: vec![track],
            hold: None,
        })
        .unwrap();
    assert_eq!((started.size, started.fps), ([64, 36], [30, 1]));
    assert!(service.record_status().recording);

    push_frames(&service, 15);
    let report: TakeReport = service.record_stop().unwrap();
    assert!(!service.record_status().recording);

    assert_eq!((report.frames, report.dropped_frames), (15, 0));
    assert!((report.duration_s - 0.5).abs() < 1e-9);
    assert_eq!(report.video, dir.path().join("take"));
    let written: TakeReport =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("take.take.json")).unwrap()).unwrap();
    assert_eq!(written, report, "the manifest on disk is the report");

    // Every frame is there, straight alpha, matching the premultiplied input through the documented formula.
    for n in 0..15u8 {
        let file = dir.path().join("take").join(format!("take_{n:07}.png"));
        let px = png_pixels(&file);
        let want = picture(n).rgba8;
        for (got, pre) in px.as_chunks::<4>().0.iter().zip(want.as_chunks::<4>().0) {
            let a = pre[3];
            let expected = [unpremultiply_u8(pre[0], a), unpremultiply_u8(pre[1], a), unpremultiply_u8(pre[2], a), a];
            assert_eq!(*got, expected, "frame {n}");
        }
        let centre = (18 * 64 + 32) * 4;
        assert_eq!((px[centre], px[centre + 3]), (n, 255), "frame {n} is where it belongs");
    }

    // The mic: samples from position 100, the muted run as silence, the lost run as silence, then the rest.
    let mic = &report.tracks[0];
    assert_eq!(mic.name, "mic");
    let samples: Vec<f32> =
        hound::WavReader::open(&mic.files[0]).unwrap().samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(samples.len(), 500);
    assert!(samples[..200].iter().all(|&s| s == 0.5));
    assert!(samples[200..350].iter().all(|&s| s == 0.0), "muted, then lost: silence");
    assert!(samples[350..].iter().all(|&s| s == 0.25));
    let causes: Vec<_> =
        mic.gaps.iter().map(|g| (g.cause, (g.at_s * 1000.0).round(), (g.len_s * 1000.0).round())).collect();
    assert_eq!(causes, [(GapCause::Muted, 200.0, 100.0), (GapCause::Lost, 300.0, 50.0)]);
}

#[test]
fn a_track_waiting_for_the_first_frame_starts_where_that_frame_was_captured() {
    let dir = tempfile::tempdir().unwrap();
    let service = StreamService::new().unwrap();
    service.set_program(Program { size: SIZE, fps: (30, 1) });
    // A 1 kHz source whose every sample holds its stream position, from 1500 (1.5 s): the tap opened before the take.
    let positions: Vec<f32> = (1500..3000).map(|p| p as f32).collect();
    let mic =
        Runs(std::collections::VecDeque::from([(TapRead { start_sample: 1500, ..TapRead::default() }, positions)]));
    // The engine clock in this test: sample n at n milliseconds.
    let align = Box::new(|t: Ticks| u64::try_from(t.0 / 1_000_000).unwrap());
    let track = AudioTrack {
        name: "mic".into(),
        source: Box::new(mic),
        rate: 1000,
        channels: 1,
        start: TrackStart::FirstFrame(align),
    };
    let plan = RecordPlan {
        dir: dir.path().to_owned(),
        stem: "take".into(),
        format: RecordFormat::PngSequence,
        tracks: vec![track],
        hold: None,
    };
    service.record_start(plan).unwrap();
    // The first frame is captured at 2 s.
    for n in 0..6u8 {
        let outcome = service.push(&picture(n), Ticks(2_000_000_000 + i64::from(n) * FRAME_NS));
        assert_eq!(outcome, PushOutcome::Queued);
        wait_pumped(&service, u64::from(n) + 1);
    }
    let report = service.record_stop().unwrap();
    let samples: Vec<f32> =
        hound::WavReader::open(&report.tracks[0].files[0]).unwrap().samples::<f32>().map(Result::unwrap).collect();
    assert_eq!(samples.first().copied(), Some(2000.0), "the track's first sample is the first frame's");
    assert_eq!(samples.len(), 1000);
    assert!(report.tracks[0].gaps.is_empty(), "{:?}", report.tracks[0].gaps);
}

#[test]
fn skipped_slots_are_repeats_and_a_second_take_waits_for_the_first() {
    let dir = tempfile::tempdir().unwrap();
    let service = StreamService::new().unwrap();
    service.set_program(Program { size: SIZE, fps: (30, 1) });
    let plan = |stem: &str| RecordPlan {
        dir: dir.path().to_owned(),
        stem: stem.into(),
        format: RecordFormat::PngSequence,
        tracks: Vec::new(),
        hold: None,
    };
    service.record_start(plan("gappy")).unwrap();
    assert!(matches!(service.record_start(plan("other")), Err(omuzaki_stream_api::error::StreamError::Busy)));
    // Frames at slots 0, 1 and 4: slots 2 and 3 passed with no frame (the stage hitched).
    for (k, slot) in [0i64, 1, 4].into_iter().enumerate() {
        service.push(&picture(slot as u8), Ticks(slot * FRAME_NS));
        wait_pumped(&service, k as u64 + 1);
    }
    let report = service.record_stop().unwrap();
    assert_eq!((report.frames, report.dropped_frames), (5, 2));
    let centre = (18 * 64 + 32) * 4;
    let centres: Vec<u8> =
        (0..5).map(|i| png_pixels(&dir.path().join("gappy").join(format!("gappy_{i:07}.png")))[centre]).collect();
    assert_eq!(centres, [0, 1, 1, 1, 4], "a skipped slot repeats the frame before it");
}

fn call(name: &str, args: Value, services: &Services) -> Value {
    omuzaki_stream::link();
    let op = (find(name).expect("registered").decode)(args).unwrap();
    let (doc, clock, actor) = (MemDoc::new(), FakeClock::new(Seconds(0.0)), Actor::Person { name: "host".into() });
    let ctx = OpCtx::new(&doc, &actor, &clock).with_services(services);
    op.plan(&ctx).unwrap().output.expect("the op answers")
}

#[test]
fn the_record_ops_start_report_and_stop_a_take_in_the_configured_folder() {
    let dir = tempfile::tempdir().unwrap();
    let service = Arc::new(StreamService::new().unwrap());
    service.set_program(Program { size: SIZE, fps: (30, 1) });
    let settings = Arc::new(omuzaki_settings_mock::MemorySettings::default());
    let folder = dir.path().to_string_lossy().into_owned();
    let person = Actor::Person { name: "host".into() };
    settings.set::<Dir>(&folder, Layer::Base, Mode::Apply, &person).unwrap();
    let services =
        ServicesBuilder::new().insert::<StreamService>(service.clone()).insert::<dyn Settings>(settings).build();

    assert_eq!(call("record.status", json!({}), &services)["recording"], false);
    let started = call("record.start", json!({ "name": "op take" }), &services);
    assert_eq!(started["started"], true);
    let best = if omuzaki_stream::vp9::available().is_ok() { "webm_vp9_alpha" } else { "png_sequence" };
    assert_eq!(started["format"], best, "auto resolves to the best format this machine writes");
    let stem = started["stem"].as_str().unwrap().to_owned();
    assert!(stem.ends_with("_op_take"), "{stem}");
    assert_eq!(call("record.status", json!({}), &services)["recording"], true);

    push_frames(&service, 3);
    let report = call("record.stop", json!({}), &services);
    assert_eq!(report["frames"], 3);
    assert!(dir.path().join(format!("{stem}.take.json")).exists());
    assert_eq!(call("record.status", json!({}), &services)["recording"], false);
}
