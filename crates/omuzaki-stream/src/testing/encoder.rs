//! The fake encoder: deterministic packets, keyframes and scripted failures, with its factory.
//!
//! Packet `k` of an encoder has `pts = dts = k`, a keyframe when `k % gop == 0`, and `key_bytes` or `delta_bytes`
//! bytes. The first eight bytes are the frame index, little endian, and the rest a fixed pattern, so a test can tell
//! which frame is where. The encoder counts its own packets, so one opened later starts again at `k = 0`.

use std::thread;
use std::time::Duration;

use bytes::Bytes;
use omuzaki_core::glam::UVec2;
use omuzaki_stream_api::encoder::{
    AlphaKind, CodecId, EncodeConfig, Encoder, EncoderFactory, EncoderInfo, Frame, StreamSpec, VideoSpec,
};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::packet::{Packet, StreamId};
use omuzaki_stream_api::time::Timebase;

/// The byte that fills everything after the frame index.
const PATTERN: u8 = 0xA5;

/// What a [`FakeEncoder`] produces. [`FakeEncoder::new`] raises a zero `size`, `fps` or `gop` to one.
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
    /// An encoder for `cfg`. A zero `size`, `fps` or `gop` is raised to one, so the stream spec is always valid.
    pub fn new(cfg: FakeEncoderConfig) -> Self {
        let cfg = FakeEncoderConfig { size: cfg.size.max(UVec2::ONE), fps: cfg.fps.max(1), gop: cfg.gop.max(1), ..cfg };
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
        self.queued.push(Packet {
            stream: StreamId(0),
            pts: self.next,
            dts: self.next,
            duration: 1,
            keyframe,
            data: Bytes::from(data),
            side_data: Vec::new(),
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

/// Opens [`FakeEncoder`]s with its configuration. Only H.264 at the configured size is offered.
#[derive(Clone, Copy, Debug)]
pub struct FakeEncoderFactory(pub FakeEncoderConfig);

impl EncoderFactory for FakeEncoderFactory {
    fn probe(&self) -> Vec<EncoderInfo> {
        // Fail closed: if the constructor ever rejects this info, offer nothing rather than panic.
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
        if cfg.video.size != self.0.size {
            return Err(StreamError::Config(format!(
                "the fake encoder is configured for {}x{}, not {}x{}",
                self.0.size.x, self.0.size.y, cfg.video.size.x, cfg.video.size.y
            )));
        }
        Ok(Box::new(FakeEncoder::new(self.0)))
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
    fn the_factory_refuses_other_codecs_and_sizes() {
        let factory = FakeEncoderFactory(config(30));
        let other = EncodeConfig::new(
            VideoSpec { size: UVec2::new(16, 16), fps: (30, 1), alpha: AlphaKind::None },
            CodecId::H264,
        )
        .unwrap();
        assert!(matches!(factory.create(&other), Err(StreamError::Config(_))));
        let vp9 = EncodeConfig::new(VideoSpec { size: SIZE, fps: (30, 1), alpha: AlphaKind::None }, CodecId::Vp9Alpha)
            .unwrap();
        assert!(matches!(factory.create(&vp9), Err(StreamError::Unavailable { .. })));
    }
}
