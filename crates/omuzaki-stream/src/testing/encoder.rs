//! The fake encoder: deterministic packets, keyframes and scripted failures, with its factory.
//!
//! Packet `k` of an encoder has `pts = dts = k`, a keyframe when `k % gop == 0`, and `key_bytes` or `delta_bytes`
//! bytes. The first eight bytes are the frame index, little endian, and the rest a fixed pattern, so a test can tell
//! which frame is where. The encoder counts its own packets, so one opened later starts again at `k = 0`. With
//! `side_data_every` set, every n-th packet also carries one `BlockAdditional` chunk (id [`SIDE_DATA_ID`]) whose eight
//! bytes are a fixed function of the frame index, so a test can follow side data through a ring, a container and a
//! save.

use std::thread;
use std::time::Duration;

use bytes::Bytes;
use omuzaki_core::glam::UVec2;
use omuzaki_stream_api::encoder::{
    AlphaKind, CodecId, EncodeConfig, Encoder, EncoderFactory, EncoderInfo, Frame, StreamSpec, VideoSpec,
};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::packet::{Packet, SideData, SideKind, StreamId};
use omuzaki_stream_api::time::Timebase;

/// The byte that fills everything after the frame index.
const PATTERN: u8 = 0xA5;

/// The `BlockAdditional` id of the side data a [`FakeEncoder`] makes.
pub const SIDE_DATA_ID: u64 = 1;

/// The bytes of the side data for the frame with this index: its index scrambled, little endian, so they differ from
/// the payload's own frame index and from the pattern.
pub fn side_data_bytes(frame_index: u64) -> [u8; 8] {
    frame_index.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(1).to_le_bytes()
}

/// What a [`FakeEncoder`] produces. [`FakeEncoder::new`] raises a zero `size`, `fps`, `gop` or `side_data_every` to one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FakeEncoderConfig {
    /// The frame size; every frame sent must match it.
    pub size: UVec2,
    /// Frames per second; the stream's timebase is `1/fps`.
    pub fps: u32,
    /// Frames between keyframes.
    pub gop: u32,
    /// Bytes in each keyframe.
    pub key_bytes: usize,
    /// Bytes in each delta frame.
    pub delta_bytes: usize,
    /// `send` fails once, with [`StreamError::Encode`], when this encoder has accepted this many frames. The count is
    /// the encoder's own, so a fresh encoder from the factory starts again at zero.
    pub fail_at_frame: Option<u64>,
    /// `send` sleeps this long, to model a slow encoder.
    pub delay: Option<Duration>,
    /// Every n-th packet (`k % n == 0`, so the first one included) carries a `BlockAdditional` chunk of
    /// [`side_data_bytes`] for its frame; `Some(1)` is every packet. `None` makes no side data.
    pub side_data_every: Option<u32>,
}

/// A deterministic [`Encoder`] for tests; see the module docs for the packets it makes.
pub struct FakeEncoder {
    cfg: FakeEncoderConfig,
    spec: StreamSpec,
    /// The number of the next packet this encoder makes (`k`).
    next: i64,
    queued: Vec<Packet>,
    keyframe_requested: bool,
    failed: bool,
    flushed: bool,
}

impl FakeEncoder {
    /// An encoder for `cfg`. A zero `size`, `fps`, `gop` or `side_data_every` is raised to one, so the stream spec is
    /// always valid.
    pub fn new(cfg: FakeEncoderConfig) -> Self {
        let cfg = FakeEncoderConfig {
            size: cfg.size.max(UVec2::ONE),
            fps: cfg.fps.max(1),
            gop: cfg.gop.max(1),
            side_data_every: cfg.side_data_every.map(|n| n.max(1)),
            ..cfg
        };
        let video = VideoSpec { size: cfg.size, fps: (cfg.fps, 1), alpha: AlphaKind::None };
        // Clamped above, so the video spec cannot be invalid.
        let Ok(spec) = StreamSpec::video(CodecId::H264, "program", Timebase { num: 1, den: cfg.fps }, video) else {
            unreachable!("a clamped configuration is a valid video spec")
        };
        Self { cfg, spec, next: 0, queued: Vec::new(), keyframe_requested: false, failed: false, flushed: false }
    }
}

impl Encoder for FakeEncoder {
    fn spec(&self) -> StreamSpec {
        self.spec.clone()
    }

    fn send(&mut self, frame: &Frame<'_>) -> Result<(), StreamError> {
        if self.flushed {
            return Err(StreamError::Stopped);
        }
        if frame.size != self.cfg.size {
            return Err(StreamError::Config(format!(
                "frame is {}x{}, the encoder takes {}x{}",
                frame.size.x, frame.size.y, self.cfg.size.x, self.cfg.size.y
            )));
        }
        if let Some(delay) = self.cfg.delay {
            thread::sleep(delay);
        }
        if !self.failed && self.cfg.fail_at_frame == u64::try_from(self.next).ok() {
            self.failed = true;
            return Err(StreamError::Encode(format!("fake failure after {} frames", self.next)));
        }
        // Taken before the test so a requested keyframe is used even on a frame that is also a multiple of `gop`.
        let forced = std::mem::take(&mut self.keyframe_requested);
        let keyframe = forced || self.next % i64::from(self.cfg.gop) == 0;
        let len = if keyframe { self.cfg.key_bytes } else { self.cfg.delta_bytes };
        let mut data = vec![PATTERN; len];
        let head = frame.index.to_le_bytes();
        let n = head.len().min(len);
        data[..n].copy_from_slice(&head[..n]);
        let side_data = match self.cfg.side_data_every {
            Some(n) if self.next % i64::from(n) == 0 => vec![SideData {
                kind: SideKind::BlockAdditional { id: SIDE_DATA_ID },
                data: Bytes::copy_from_slice(&side_data_bytes(frame.index)),
            }],
            _ => Vec::new(),
        };
        self.queued.push(Packet {
            stream: StreamId(0),
            pts: self.next,
            dts: self.next,
            duration: 1,
            keyframe,
            data: Bytes::from(data),
            side_data,
        });
        self.next += 1;
        Ok(())
    }

    fn receive(&mut self, out: &mut Vec<Packet>) -> Result<(), StreamError> {
        out.append(&mut self.queued);
        Ok(())
    }

    fn request_keyframe(&mut self) {
        self.keyframe_requested = true;
    }

    fn flush(&mut self, out: &mut Vec<Packet>) -> Result<(), StreamError> {
        self.flushed = true;
        self.receive(out)
    }
}

/// Opens [`FakeEncoder`]s. Only H.264 without alpha is offered, at any size and at a whole number of frames per second
/// (a ratio that divides evenly, such as `60/2`, is honoured at its quotient).
///
/// The encoder's size, `fps` and `gop` come from the request (`gop` is its `keyint`), so the stream it reports is the
/// one asked for; the factory's own configuration supplies `key_bytes`, `delta_bytes`, `fail_at_frame`, `delay` and
/// `side_data_every`, and its `size`, `fps` and `gop` are not used. A request the fake cannot honour is refused:
/// another codec, alpha, or a frame rate with a remainder. The bitrate, thread count and `realtime` hints are ignored.
#[derive(Clone, Copy, Debug)]
pub struct FakeEncoderFactory(pub FakeEncoderConfig);

impl EncoderFactory for FakeEncoderFactory {
    fn probe(&self) -> Vec<EncoderInfo> {
        // `EncoderInfo::new` fails only for a codec that is not video; H.264 is, so this always offers one encoder.
        // `probe` cannot return an error, and `probe_offers_h264` fails if that ever changes.
        EncoderInfo::new(CodecId::H264, "fake", false, false).into_iter().collect()
    }

    fn create(&self, cfg: &EncodeConfig) -> Result<Box<dyn Encoder>, StreamError> {
        cfg.validate()?;
        if cfg.codec != CodecId::H264 {
            return Err(StreamError::Unavailable {
                what: format!("the {:?} codec", cfg.codec),
                hint: "the fake encoder only produces H.264".into(),
            });
        }
        if cfg.video.alpha != AlphaKind::None {
            return Err(StreamError::Unavailable {
                what: "alpha".into(),
                hint: "the fake encoder produces no alpha channel".into(),
            });
        }
        let (num, den) = cfg.video.fps;
        // A ratio that divides evenly is honoured at its quotient (`60/2` is 30 fps); one with a remainder is refused.
        let (Some(fps), Some(0)) = (num.checked_div(den), num.checked_rem(den)) else {
            return Err(StreamError::Config(format!(
                "the fake encoder takes whole frames per second, not {num}/{den}"
            )));
        };
        Ok(Box::new(FakeEncoder::new(FakeEncoderConfig { size: cfg.video.size, fps, gop: cfg.keyint, ..self.0 })))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_stream_api::time::Ticks;

    const SIZE: UVec2 = UVec2::new(8, 8);

    fn config(gop: u32) -> FakeEncoderConfig {
        FakeEncoderConfig {
            size: SIZE,
            fps: 30,
            gop,
            key_bytes: 100,
            delta_bytes: 10,
            fail_at_frame: None,
            delay: None,
            side_data_every: None,
        }
    }

    fn frame<'a>(index: u64) -> Frame<'a> {
        Frame::new(SIZE, index, Ticks::default())
    }

    fn encode(enc: &mut dyn Encoder, frames: std::ops::Range<u64>) -> Vec<Packet> {
        let mut out = Vec::new();
        for index in frames {
            enc.send(&frame(index)).unwrap();
            enc.receive(&mut out).unwrap();
        }
        out
    }

    fn video_config() -> EncodeConfig {
        EncodeConfig::new(VideoSpec { size: SIZE, fps: (30, 1), alpha: AlphaKind::None }, CodecId::H264).unwrap()
    }

    #[test]
    fn keyframes_fall_every_gop_frames_and_the_payload_names_the_frame() {
        let mut enc = FakeEncoder::new(config(30));
        assert_eq!(enc.spec().timebase, Timebase { num: 1, den: 30 });
        let packets = encode(&mut enc, 0..90);
        let keys: Vec<usize> = packets.iter().enumerate().filter(|(_, p)| p.keyframe).map(|(k, _)| k).collect();
        assert_eq!(keys, [0, 30, 60]);
        for (k, p) in packets.iter().enumerate() {
            let k = u64::try_from(k).unwrap();
            assert_eq!((p.pts, p.dts), (i64::try_from(k).unwrap(), i64::try_from(k).unwrap()));
            assert_eq!(p.data.len(), if p.keyframe { 100 } else { 10 });
            assert_eq!(p.data[..8], k.to_le_bytes());
            assert!(p.data[8..].iter().all(|&b| b == PATTERN));
        }
    }

    #[test]
    fn a_scripted_failure_happens_once_and_a_fresh_encoder_starts_on_a_keyframe() {
        let mut cfg = config(30);
        cfg.fail_at_frame = Some(40);
        let factory = FakeEncoderFactory(cfg);
        let mut enc = factory.create(&video_config()).unwrap();
        let mut out = Vec::new();
        for index in 0..40 {
            enc.send(&frame(index)).unwrap();
            enc.receive(&mut out).unwrap();
        }
        assert!(matches!(enc.send(&frame(40)), Err(StreamError::Encode(_))));
        enc.send(&frame(40)).unwrap();
        enc.receive(&mut out).unwrap();
        assert_eq!(out.len(), 41);

        let mut fresh = factory.create(&video_config()).unwrap();
        let first = encode(fresh.as_mut(), 40..41);
        assert!(first[0].keyframe);
        assert_eq!(first[0].data[..8], 40u64.to_le_bytes());
    }

    #[test]
    fn a_requested_keyframe_is_used_once_and_flush_ends_the_stream() {
        let mut enc = FakeEncoder::new(config(30));
        encode(&mut enc, 0..2);
        enc.request_keyframe();
        let packets = encode(&mut enc, 2..4);
        assert_eq!(packets.iter().map(|p| p.keyframe).collect::<Vec<_>>(), [true, false]);
        let mut rest = Vec::new();
        enc.flush(&mut rest).unwrap();
        assert_eq!(rest.len(), 0);
        assert!(matches!(enc.send(&frame(4)), Err(StreamError::Stopped)));
    }

    #[test]
    fn the_factory_refuses_what_it_cannot_honour_and_builds_the_rest_from_the_request() {
        let factory = FakeEncoderFactory(config(30));
        let vp9 = EncodeConfig::new(VideoSpec { size: SIZE, fps: (30, 1), alpha: AlphaKind::None }, CodecId::Vp9Alpha)
            .unwrap();
        assert!(matches!(factory.create(&vp9), Err(StreamError::Unavailable { .. })));
        let alpha =
            EncodeConfig::new(VideoSpec { size: SIZE, fps: (30, 1), alpha: AlphaKind::Straight }, CodecId::H264)
                .unwrap();
        assert!(matches!(factory.create(&alpha), Err(StreamError::Unavailable { .. })));
        let ntsc =
            EncodeConfig::new(VideoSpec { size: SIZE, fps: (30000, 1001), alpha: AlphaKind::None }, CodecId::H264)
                .unwrap();
        assert!(matches!(factory.create(&ntsc), Err(StreamError::Config(_))));
        let odd =
            EncodeConfig::new(VideoSpec { size: SIZE, fps: (61, 2), alpha: AlphaKind::None }, CodecId::H264).unwrap();
        assert!(matches!(factory.create(&odd), Err(StreamError::Config(_))));
        let reducible =
            EncodeConfig::new(VideoSpec { size: SIZE, fps: (60, 2), alpha: AlphaKind::None }, CodecId::H264).unwrap();
        let spec = factory.create(&reducible).unwrap().spec();
        assert_eq!(spec.timebase, Timebase { num: 1, den: 30 });
        assert_eq!(spec.video.map(|v| v.fps), Some((30, 1)));
    }

    #[test]
    fn a_60_fps_request_with_keyint_60_gets_a_60_fps_encoder_with_that_keyint() {
        // The factory's own size, fps and gop (8x8, 30, 30) differ from the request's.
        let factory = FakeEncoderFactory(config(30));
        let size = UVec2::new(16, 4);
        let mut request =
            EncodeConfig::new(VideoSpec { size, fps: (60, 1), alpha: AlphaKind::None }, CodecId::H264).unwrap();
        request.keyint = 60;
        let mut enc = factory.create(&request).unwrap();
        let spec = enc.spec();
        assert_eq!(spec.timebase, Timebase { num: 1, den: 60 });
        assert_eq!(spec.video.map(|v| (v.size, v.fps)), Some((size, (60, 1))));
        let mut out = Vec::new();
        for index in 0..121 {
            enc.send(&Frame::new(size, index, Ticks::default())).unwrap();
            enc.receive(&mut out).unwrap();
        }
        let keys: Vec<i64> = out.iter().filter(|p| p.keyframe).map(|p| p.pts).collect();
        assert_eq!(keys, [0, 60, 120]);
        assert!(matches!(enc.send(&frame(121)), Err(StreamError::Config(_))), "8x8 frames do not fit 16x4");
    }

    #[test]
    fn probe_offers_h264() {
        let infos = FakeEncoderFactory(config(30)).probe();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].codec, CodecId::H264);
    }

    #[test]
    fn side_data_follows_the_configured_cadence_and_names_its_frame() {
        let mut cfg = config(30);
        assert!(encode(&mut FakeEncoder::new(cfg), 0..10).iter().all(|p| p.side_data.is_empty()));
        cfg.side_data_every = Some(3);
        let packets = encode(&mut FakeEncoder::new(cfg), 5..20);
        for (k, p) in packets.iter().enumerate() {
            if k % 3 == 0 {
                let frame_index = 5 + u64::try_from(k).unwrap();
                let expected = SideData {
                    kind: SideKind::BlockAdditional { id: SIDE_DATA_ID },
                    data: Bytes::copy_from_slice(&side_data_bytes(frame_index)),
                };
                assert_eq!(p.side_data, [expected]);
            } else {
                assert_eq!(p.side_data.len(), 0);
            }
        }
        assert_ne!(side_data_bytes(1), side_data_bytes(2));
        cfg.side_data_every = Some(0);
        assert!(encode(&mut FakeEncoder::new(cfg), 0..4).iter().all(|p| p.side_data.len() == 1));
    }
}
