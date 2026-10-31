//! The `WebM` VP9 take with alpha, end to end: frames pushed into the stream service are encoded by libvpx, muxed with
//! their alpha in `BlockAdditional`, and decode back to the picture that went in. Skips, saying so, on a machine
//! without libvpx.

// Integration-test helpers may panic: a panic is a clear test failure. The picture maths casts small known values,
// and the test reads its own temporary folder.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::cast_lossless,
    clippy::disallowed_methods
)]

use omuzaki_core::glam::UVec2;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream::alpha::unpremultiply_u8;
use omuzaki_stream::i420::{luma, rgba_to_i420};
use omuzaki_stream::record::runtime::RecordPlan;
use omuzaki_stream::record::settings::RecordFormat;
use omuzaki_stream::service::{Program, PushOutcome, StreamService};
use omuzaki_stream::sinks::webm;
use omuzaki_stream::vp9::{self, EncoderSettings, I420, Vp9Decoder, Vp9Encoder};
use omuzaki_stream_api::time::Ticks;

const SIZE: UVec2 = UVec2::new(128, 72);
const FRAME_NS: i64 = 1_000_000_000 / 30;

fn libvpx() -> bool {
    match vp9::available() {
        Ok(()) => true,
        Err(e) => {
            eprintln!("skipped: {e}");
            false
        }
    }
}

/// A premultiplied soft disc moving right with `n`, its colour fixed, alpha 255 at the centre falling to 0 at the rim.
fn picture(n: u32) -> Image {
    let mut rgba8 = Vec::with_capacity((SIZE.x * SIZE.y * 4) as usize);
    let cx = 40.0 + n as f32;
    for y in 0..SIZE.y {
        for x in 0..SIZE.x {
            let (dx, dy) = (x as f32 - cx, y as f32 - 36.0);
            let a = (255.0 * (1.4 - (dx * dx + dy * dy).sqrt() / 24.0)).clamp(0.0, 255.0) as u32;
            for c in [230u32, 120, 60] {
                rgba8.push(((c * a + 127) / 255) as u8);
            }
            rgba8.push(a as u8);
        }
    }
    Image { size: SIZE, alpha: AlphaEncoding::Premultiplied, rgba8 }
}

fn straight(image: &Image) -> Vec<u8> {
    image
        .rgba8
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [0, 1, 2].map(|i| unpremultiply_u8(p[i], p[3])).into_iter().chain([p[3]]))
        .collect()
}

/// The frame index a millisecond timestamp on the 30 fps grid stands for.
fn index_of(ts_ms: i64) -> u32 {
    ((ts_ms * 30 + 500) / 1000) as u32
}

fn mean_abs(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    a.iter().zip(b).map(|(x, y)| (i32::from(*x) - i32::from(*y)).unsigned_abs() as f64).sum::<f64>() / a.len() as f64
}

#[test]
fn the_vp9_pair_round_trips_colour_and_alpha() {
    if !libvpx() {
        return;
    }
    let settings = |cq_level, studio_range| EncoderSettings {
        width: SIZE.x,
        height: SIZE.y,
        fps: (30, 1),
        cq_level,
        cpu_used: 6,
        threads: 2,
        studio_range,
        realtime: true,
    };
    let mut colour_enc = Vp9Encoder::new(settings(18, true)).unwrap();
    let mut alpha_enc = Vp9Encoder::new(settings(10, false)).unwrap();
    let (mut colour_dec, mut alpha_dec) = (Vp9Decoder::new().unwrap(), Vp9Decoder::new().unwrap());
    let (mut colour, mut alpha) = (I420::default(), I420::default());
    let (mut got_colour, mut got_alpha) = (I420::default(), I420::default());
    for n in 0..10 {
        let rgba = straight(&picture(n));
        assert!(rgba_to_i420(&rgba, SIZE.x, SIZE.y, &mut colour, &mut alpha));
        let (mut c, mut a) = (Vec::new(), Vec::new());
        colour_enc.encode(&colour, i64::from(n), n == 0, &mut c).unwrap();
        alpha_enc.encode(&alpha, i64::from(n), n == 0, &mut a).unwrap();
        assert_eq!((c.len(), a.len()), (1, 1), "one packet each per frame");
        assert_eq!(c[0].key, n == 0, "keyframes only where asked");
        colour_dec.decode(&c[0].data, &mut got_colour).unwrap();
        alpha_dec.decode(&a[0].data, &mut got_alpha).unwrap();
        let alpha_err = mean_abs(&got_alpha.y, &alpha.y);
        let luma_err = mean_abs(&got_colour.y, &colour.y);
        assert!(alpha_err < 1.5, "frame {n}: alpha off by {alpha_err:.2} on average");
        assert!(luma_err < 2.5, "frame {n}: luma off by {luma_err:.2} on average");
    }
}

#[test]
fn quality_level_0_is_lossless() {
    if !libvpx() {
        return;
    }
    let settings = EncoderSettings {
        width: SIZE.x,
        height: SIZE.y,
        fps: (30, 1),
        cq_level: 0,
        cpu_used: 3,
        threads: 2,
        studio_range: true,
        realtime: false,
    };
    let mut encoder = Vp9Encoder::new(settings).unwrap();
    let mut decoder = Vp9Decoder::new().unwrap();
    let (mut colour, mut alpha, mut got) = (I420::default(), I420::default(), I420::default());
    for n in 0..4 {
        let rgba = straight(&picture(n * 3));
        assert!(rgba_to_i420(&rgba, SIZE.x, SIZE.y, &mut colour, &mut alpha));
        let mut packets = Vec::new();
        encoder.encode(&colour, i64::from(n), n == 0, &mut packets).unwrap();
        decoder.decode(&packets[0].data, &mut got).unwrap();
        assert_eq!((got.y == colour.y, got.u == colour.u, got.v == colour.v), (true, true, true), "frame {n}");
    }
}

#[test]
fn a_webm_take_keeps_its_alpha_and_its_timeline() {
    if !libvpx() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let service = StreamService::new().unwrap();
    service.set_program(Program { size: SIZE, fps: (30, 1) });
    let started = service
        .record_start(RecordPlan {
            dir: dir.path().to_owned(),
            stem: "take".into(),
            format: RecordFormat::WebmVp9Alpha,
            tracks: Vec::new(),
            hold: None,
            guard: None,
        })
        .unwrap();
    assert_eq!(started.video, dir.path().join("take.webm"));
    // 75 frames: 2.5 s, so a second keyframe and cluster at 2 s.
    for n in 0..75u32 {
        assert_eq!(service.push(&picture(n), Ticks(i64::from(n) * FRAME_NS)), PushOutcome::Queued);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while service.counters().pumped <= u64::from(n) {
            assert!(std::time::Instant::now() < deadline, "the pump stalled at {n}");
            std::thread::yield_now();
        }
    }
    let report = service.record_stop().unwrap();
    assert_eq!(report.format, RecordFormat::WebmVp9Alpha);
    assert!(report.encoder.starts_with("libvpx-vp9"), "{}", report.encoder);
    assert_eq!(report.frames, 75);
    let bytes = std::fs::read(&report.video).unwrap();
    assert_eq!(report.bytes, bytes.len() as u64);

    let info = webm::read(&bytes).unwrap();
    assert_eq!((info.codec.as_str(), info.alpha_mode, info.width, info.height), ("V_VP9", 1, 128, 72));
    assert_eq!(info.duration_ms, Some(2500.0));
    assert!(info.sized);
    // Frames the encoder fell behind on are held, not lost: the timestamps still sit on the 30 fps grid.
    assert_eq!(info.frames.len() as u64 + report.dropped_frames, 75);
    let keys: Vec<_> = info.frames.iter().filter(|f| f.key).map(|f| f.ts_ms).collect();
    assert_eq!(keys.first(), Some(&0));
    assert_eq!(info.cues, keys.len());
    assert!(info.frames.iter().all(|f| f.alpha.is_some()), "every frame carries its alpha");

    // Decode every frame and compare the last with what went in.
    let (mut colour_dec, mut alpha_dec) = (Vp9Decoder::new().unwrap(), Vp9Decoder::new().unwrap());
    let (mut colour, mut alpha) = (I420::default(), I420::default());
    for frame in &info.frames {
        colour_dec.decode(&frame.data, &mut colour).unwrap();
        alpha_dec.decode(frame.alpha.as_deref().unwrap(), &mut alpha).unwrap();
        // Every frame, not just the last: a delta frame decodes only on top of the right predecessors.
        let want: Vec<u8> = straight(&picture(index_of(frame.ts_ms))).as_chunks::<4>().0.iter().map(|p| p[3]).collect();
        let err = mean_abs(&alpha.y, &want);
        assert!(err < 0.5, "frame at {} ms: alpha off by {err:.2} on average", frame.ts_ms);
    }
    let last = info.frames.last().unwrap();
    let n = index_of(last.ts_ms);
    let want = straight(&picture(n));
    let want_alpha: Vec<u8> = want.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
    assert!(mean_abs(&alpha.y, &want_alpha) < 0.5);
    let centre = (36 * 128 + 40 + n) as usize;
    let expected = luma(230, 120, 60);
    assert!(
        (i32::from(colour.y[centre]) - i32::from(expected)).abs() <= 3,
        "centre luma {} vs {expected}",
        colour.y[centre]
    );
    assert_eq!(alpha.y[0], 0, "the corner stays transparent");
}
