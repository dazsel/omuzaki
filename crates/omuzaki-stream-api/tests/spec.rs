//! Constructors, validation, serde and schema of the stream, encoder and output types.

use bytes::Bytes;
use omuzaki_core::glam::UVec2;
use omuzaki_stream_api::encoder::{
    AlphaKind, AudioSpec, CodecId, EncodeConfig, Frame, StreamKind, StreamSpec, VideoSpec,
};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::output::{Feed, OutputConfig, OutputState, OutputStatus};
use omuzaki_stream_api::time::{Ticks, Timebase};

const TB: Timebase = Timebase { num: 1, den: 30 };

fn video() -> VideoSpec {
    VideoSpec { size: UVec2::new(1920, 1080), fps: (30, 1), alpha: AlphaKind::Premultiplied }
}

#[test]
fn stream_spec_constructors_set_the_matching_payload() {
    let v = StreamSpec::video(CodecId::Vp9Alpha, "program", TB, video()).unwrap();
    assert_eq!((v.kind, v.video.is_some(), v.audio.is_none()), (StreamKind::Video, true, true));
    let a = StreamSpec::audio(
        CodecId::PcmF32,
        "mic",
        Timebase { num: 1, den: 48000 },
        AudioSpec { rate: 48000, channels: 2 },
    )
    .unwrap();
    assert_eq!((a.kind, a.audio.is_some()), (StreamKind::Audio, true));
    let d = StreamSpec::data(CodecId::InputRecords, "input", TB).unwrap();
    assert_eq!(d.validate(), Ok(()));
}

#[test]
fn invalid_specs_are_config_errors() {
    let bad = VideoSpec { size: UVec2::new(0, 8), ..video() };
    assert!(matches!(StreamSpec::video(CodecId::H264, "x", TB, bad), Err(StreamError::Config(_))));
    assert!(StreamSpec::video(CodecId::H264, "x", Timebase { num: 1, den: 0 }, video()).is_err());
    assert!(StreamSpec::audio(CodecId::Opus, "x", TB, AudioSpec { rate: 0, channels: 2 }).is_err());
    let mut mismatched = StreamSpec::data(CodecId::InputRecords, "x", TB).unwrap();
    mismatched.kind = StreamKind::Video;
    assert!(mismatched.validate().is_err(), "a video stream needs a VideoSpec");
}

#[test]
fn encode_config_defaults_and_validation() {
    let mut cfg = EncodeConfig::new(video(), CodecId::Vp9Alpha).unwrap();
    assert_eq!((cfg.keyint, cfg.bitrate_kbps, cfg.realtime), (30, None, true));
    assert_eq!(cfg.validate(), Ok(()));
    cfg.keyint = 0;
    assert!(cfg.validate().is_err());
    cfg.keyint = 30;
    cfg.threads = Some(0);
    assert!(cfg.validate().is_err());
}

#[test]
fn stream_spec_and_encode_config_round_trip_through_json() {
    let spec = StreamSpec::video(CodecId::Vp9Alpha, "program", TB, video())
        .unwrap()
        .with_extradata(Bytes::from_static(&[1, 2, 3]));
    let back: StreamSpec = serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
    assert_eq!(back, spec);
    let cfg = EncodeConfig::new(video(), CodecId::ProRes4444).unwrap();
    let back: EncodeConfig = serde_json::from_str(&serde_json::to_string(&cfg).unwrap()).unwrap();
    assert_eq!(back, cfg);
}

#[test]
fn settings_types_have_json_schemas() {
    for schema in [schemars::schema_for!(StreamSpec), schemars::schema_for!(EncodeConfig)] {
        assert!(serde_json::to_value(schema).unwrap()["$schema"].is_string());
    }
}

#[test]
fn output_config_validates_and_defaults_to_program() {
    let cfg = OutputConfig::new("main", UVec2::new(1280, 720), (60, 1)).unwrap();
    assert_eq!(cfg.feed, Feed::Program);
    assert_eq!(cfg.with_feed(Feed::Clean).feed, Feed::Clean);
    assert!(OutputConfig::new("", UVec2::new(1, 1), (30, 1)).is_err());
    assert!(OutputConfig::new("x", UVec2::new(0, 1), (30, 1)).is_err());
    assert!(OutputConfig::new("x", UVec2::new(1, 1), (30, 0)).is_err());
}

#[test]
fn a_new_output_is_idle_and_a_frame_starts_without_pixels() {
    let status = OutputStatus::default();
    assert_eq!((status.state, status.dropped, status.last_error), (OutputState::Idle, 0, None));
    let frame = Frame::new(UVec2::new(2, 2), 7, Ticks(9));
    assert!(frame.cpu.is_none() && frame.gpu.is_none());
    assert_eq!(frame.index, 7);
}

#[test]
fn every_codec_belongs_to_one_kind() {
    use CodecId::*;
    for c in [ProRes4444, Vp9Alpha, Png, H264] {
        assert_eq!(c.kind(), StreamKind::Video);
    }
    for c in [PcmS24, PcmF32, Opus] {
        assert_eq!(c.kind(), StreamKind::Audio);
    }
    assert_eq!(InputRecords.kind(), StreamKind::Data);
}

#[test]
fn a_codec_of_the_wrong_kind_is_refused() {
    let audio = AudioSpec { rate: 48000, channels: 2 };
    for codec in [CodecId::Opus, CodecId::InputRecords] {
        assert!(StreamSpec::video(codec, "x", TB, video()).is_err(), "{codec:?} on video");
        assert!(EncodeConfig::new(video(), codec).is_err(), "{codec:?} in an encode config");
    }
    for codec in [CodecId::H264, CodecId::InputRecords] {
        assert!(StreamSpec::audio(codec, "x", TB, audio).is_err(), "{codec:?} on audio");
    }
    for codec in [CodecId::H264, CodecId::PcmF32] {
        assert!(StreamSpec::data(codec, "x", TB).is_err(), "{codec:?} on data");
    }
    // A spec or config edited after construction is caught by validate().
    let mut spec = StreamSpec::video(CodecId::H264, "x", TB, video()).unwrap();
    spec.codec = CodecId::Opus;
    assert!(spec.validate().is_err());
    let mut cfg = EncodeConfig::new(video(), CodecId::H264).unwrap();
    cfg.codec = CodecId::PcmF32;
    assert!(cfg.validate().is_err());
}
