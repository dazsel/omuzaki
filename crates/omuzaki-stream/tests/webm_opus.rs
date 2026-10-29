//! A `WebM` with its voice in the same file, end to end: two seconds of a tone encoded by libopus and a few VP9 frames
//! encoded by libvpx, muxed together, read back, and the audio decoded to the tone that went in. Skips, saying so, on
//! a machine without libopus or libvpx.

// Integration-test helpers may panic: a panic is a clear test failure. The tone maths casts small known values, and the
// test reads its own temporary folder.
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    clippy::disallowed_methods
)]

use omuzaki_stream::opus::{self, FRAME_SAMPLES, OpusDecoder, OpusEncoder, OpusPacket, OpusSettings, SAMPLE_RATE};
use omuzaki_stream::sinks::webm::{self, AUDIO_TRACK, SEEK_PRE_ROLL_NS, VIDEO_TRACK, WebmAudio, WebmSink, WebmTrack};
use omuzaki_stream::vp9::{self, EncoderSettings, I420, Vp9Encoder};

const FPS: u32 = 5;
const FRAMES: u64 = 10;
const SECONDS: usize = 2;
const TONE_HZ: f32 = 440.0;
const AMPLITUDE: f32 = 0.5;

fn libraries() -> bool {
    match opus::available().and_then(|()| vp9::available()) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("skipped: {e}");
            false
        }
    }
}

fn tone(sample: usize) -> f32 {
    AMPLITUDE * (std::f32::consts::TAU * TONE_HZ * sample as f32 / SAMPLE_RATE as f32).sin()
}

#[test]
fn a_tone_and_vp9_frames_share_one_webm_and_the_tone_decodes_back() {
    if !libraries() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("voice.webm");
    let mut audio = OpusEncoder::new(OpusSettings { channels: 1, bitrate: 64_000 }).unwrap();
    let pre_skip = audio.pre_skip();
    let track = WebmTrack { width: 64, height: 48, fps: (FPS, 1), alpha: false };
    let mut sink = WebmSink::create_with_audio(&path, track, Some(WebmAudio { channels: 1, pre_skip })).unwrap();
    let mut video = Vp9Encoder::new(EncoderSettings {
        width: 64,
        height: 48,
        fps: (FPS, 1),
        cq_level: 30,
        cpu_used: 8,
        threads: 1,
        studio_range: true,
        realtime: true,
    })
    .unwrap();

    // Each frame's span of audio goes in with the frame, as a recorder feeds them.
    let per_frame = SAMPLE_RATE as usize / FPS as usize;
    let (mut packets, mut frames, mut fed) = (Vec::<OpusPacket>::new(), Vec::new(), 0usize);
    for index in 0..FRAMES {
        let mut picture = I420::new(64, 48);
        picture.y.fill(16 + 20 * index as u8);
        picture.u.fill(128);
        picture.v.fill(128);
        let key = index % 5 == 0;
        video.encode(&picture, index as i64, key, &mut frames).unwrap();
        for f in frames.drain(..) {
            sink.write_frame(u64::try_from(f.pts).unwrap(), f.key, &f.data, None).unwrap();
        }
        let pcm: Vec<f32> = (fed..fed + per_frame).map(tone).collect();
        fed += per_frame;
        audio.encode(&pcm, &mut packets).unwrap();
        for p in packets.drain(..) {
            sink.write_audio(p.sample, &p.data).unwrap();
        }
    }
    assert_eq!(fed, SECONDS * SAMPLE_RATE as usize);
    audio.flush(&mut packets).unwrap();
    for p in packets.drain(..) {
        sink.write_audio(p.sample, &p.data).unwrap();
    }
    let bytes = sink.finish(FRAMES).unwrap();

    let file = std::fs::read(&path).unwrap();
    assert_eq!(bytes, file.len() as u64);
    let info = webm::read(&file).unwrap();
    assert_eq!(info.codec, "V_VP9");
    assert_eq!(info.frames.len(), FRAMES as usize);
    let track = info.audio.clone().expect("an audio track");
    assert_eq!((track.number, track.codec.as_str(), track.channels), (AUDIO_TRACK, "A_OPUS", 1));
    assert_eq!(track.sampling_frequency, 48_000.0);
    assert_eq!(track.codec_private, webm::opus_head(WebmAudio { channels: 1, pre_skip }));
    assert_eq!(track.codec_delay_ns, u64::from(pre_skip) * 1_000_000_000 / 48_000);
    assert_eq!(track.seek_pre_roll_ns, SEEK_PRE_ROLL_NS);

    // Every input sample plus the pre-skip is covered by whole 20 ms packets, stamped 20 ms apart.
    let expected = (fed + usize::from(pre_skip)).div_ceil(FRAME_SAMPLES);
    assert_eq!(info.audio_blocks.len(), expected);
    for (k, block) in info.audio_blocks.iter().enumerate() {
        assert_eq!(block.ts_ms, 20 * k as i64);
    }
    // One cluster per keyframe interval, and blocks in timestamp order through the file.
    assert_eq!(info.clusters, [0, 1000]);
    assert_eq!(info.cues, 2);
    assert!(info.blocks.windows(2).all(|w| w[0].1 <= w[1].1), "{:?}", info.blocks);
    assert!(info.blocks.iter().any(|b| b.0 == VIDEO_TRACK) && info.blocks.iter().any(|b| b.0 == AUDIO_TRACK));
    let duration = info.duration_ms.expect("a duration");
    assert!((duration - 2000.0).abs() < 25.0, "{duration}");

    // The audio decodes to the tone, aligned once the pre-skip is dropped.
    let mut decoder = OpusDecoder::new(1).unwrap();
    let mut pcm = Vec::new();
    for block in &info.audio_blocks {
        decoder.decode(&block.data, &mut pcm).unwrap();
    }
    let pcm = &pcm[usize::from(pre_skip)..];
    assert!(pcm.len() >= fed);
    // Skip the first 20 ms, where the codec is still settling.
    let span = FRAME_SAMPLES..fed;
    let error = span.clone().map(|i| (pcm[i] - tone(i)).powi(2)).sum::<f32>() / span.len() as f32;
    let signal = span.clone().map(|i| tone(i).powi(2)).sum::<f32>() / span.len() as f32;
    let snr_db = 10.0 * (signal / error).log10();
    assert!(snr_db > 20.0, "the tone comes back at {snr_db:.1} dB SNR");
}
