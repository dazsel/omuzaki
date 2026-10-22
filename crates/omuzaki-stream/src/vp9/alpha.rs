//! VP9 with alpha behind the [`Encoder`] trait, for the replay buffer (`docs/design/record.md` D3): the colour and alpha
//! pictures encoded side by side, the alpha frame carried as `BlockAdditional` side data (id 1), as `FFmpeg`'s encoder
//! carries it, so the ring, a disk segment and the `WebM` muxer pass it through unchanged.

use bytes::Bytes;
use omuzaki_stream_api::encoder::{
    AlphaKind, CodecId, EncodeConfig, Encoder, EncoderFactory, EncoderInfo, Frame, StreamSpec, VideoSpec,
};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::packet::{Packet, SideData, SideKind, StreamId};
use omuzaki_stream_api::time::Timebase;

use super::{EncoderSettings, I420, Vp9Encoder};
use crate::i420;
use crate::record::runtime::{VP9_ALPHA_CQ, VP9_COLOUR_CQ, VP9_CPU_USED};

/// The `BlockAdditional` id of the alpha frame (Matroska's for VP9 alpha).
pub const ALPHA_BLOCK_ID: u64 = 1;
/// Threads converting each frame to the two pictures.
const CONVERT_THREADS: usize = 4;

/// A VP9 encoder pair for straight-alpha RGBA frames. One packet out per frame in (real-time mode, no look-ahead).
pub struct Vp9AlphaEncoder {
    colour: Vp9Encoder,
    alpha: Vp9Encoder,
    spec: StreamSpec,
    pictures: (I420, I420),
    outs: (Vec<super::Packet>, Vec<super::Packet>),
    keyint: u64,
    since_key: u64,
    force_key: bool,
    next: i64,
    queued: Vec<Packet>,
}

impl Vp9AlphaEncoder {
    /// Opens both encoders for `cfg` (codec [`CodecId::Vp9Alpha`]).
    ///
    /// # Errors
    /// [`StreamError::Unavailable`] when libvpx can't be loaded, [`StreamError::Config`] for another codec or a bad
    /// picture format.
    pub fn new(cfg: &EncodeConfig) -> Result<Self, StreamError> {
        cfg.validate()?;
        if cfg.codec != CodecId::Vp9Alpha {
            return Err(StreamError::Config(format!("the VP9 alpha encoder can't make {:?}", cfg.codec)));
        }
        let settings = |cq_level, studio_range| EncoderSettings {
            width: cfg.video.size.x,
            height: cfg.video.size.y,
            fps: cfg.video.fps,
            cq_level,
            cpu_used: VP9_CPU_USED,
            threads: cfg.threads.map_or_else(super::default_threads, u32::from),
            studio_range,
            realtime: cfg.realtime,
        };
        let colour = Vp9Encoder::new(settings(VP9_COLOUR_CQ, true))?;
        let alpha = Vp9Encoder::new(settings(VP9_ALPHA_CQ, false))?;
        let video = VideoSpec { alpha: AlphaKind::Straight, ..cfg.video };
        let timebase = Timebase { num: cfg.video.fps.1, den: cfg.video.fps.0 };
        let spec = StreamSpec::video(CodecId::Vp9Alpha, "program", timebase, video)?;
        Ok(Self {
            colour,
            alpha,
            spec,
            pictures: (I420::default(), I420::default()),
            outs: (Vec::new(), Vec::new()),
            keyint: u64::from(cfg.keyint.max(1)),
            since_key: 0,
            force_key: false,
            next: 0,
            queued: Vec::new(),
        })
    }
}

impl Encoder for Vp9AlphaEncoder {
    fn spec(&self) -> StreamSpec {
        self.spec.clone()
    }

    fn send(&mut self, frame: &Frame<'_>) -> Result<(), StreamError> {
        let image = frame.cpu.ok_or_else(|| StreamError::Config("the VP9 alpha encoder reads the CPU image".into()))?;
        let (colour, alpha) = &mut self.pictures;
        if !i420::rgba_to_i420_threads(&image.rgba8, image.size.x, image.size.y, colour, alpha, CONVERT_THREADS) {
            return Err(StreamError::Config("a frame's pixels do not match its size".into()));
        }
        let key = self.force_key || self.since_key == 0 || self.since_key >= self.keyint;
        let pts = self.next;
        let (colour_out, alpha_out) = &mut self.outs;
        colour_out.clear();
        alpha_out.clear();
        let (colour_encoder, alpha_encoder) = (&mut self.colour, &mut self.alpha);
        let (colour_picture, alpha_picture) = (&self.pictures.0, &self.pictures.1);
        // The two pictures are independent: encode them side by side.
        std::thread::scope(|scope| {
            let alpha_job = scope.spawn(|| alpha_encoder.encode(alpha_picture, pts, key, alpha_out));
            colour_encoder.encode(colour_picture, pts, key, colour_out)?;
            alpha_job.join().map_err(|_| StreamError::Encode("the alpha encoder panicked".into()))?
        })?;
        let (Some(c), Some(a)) = (colour_out.pop(), alpha_out.pop()) else {
            return Err(StreamError::Encode(format!("libvpx returned no packet for frame {}", frame.index)));
        };
        self.since_key = if c.key { 1 } else { self.since_key + 1 };
        self.force_key = false;
        self.next += 1;
        self.queued.push(Packet {
            stream: StreamId(0),
            pts,
            dts: pts,
            duration: 1,
            keyframe: c.key,
            data: Bytes::from(c.data),
            side_data: vec![SideData {
                kind: SideKind::BlockAdditional { id: ALPHA_BLOCK_ID },
                data: Bytes::from(a.data),
            }],
        });
        Ok(())
    }

    fn receive(&mut self, out: &mut Vec<Packet>) -> Result<(), StreamError> {
        out.append(&mut self.queued);
        Ok(())
    }

    fn request_keyframe(&mut self) {
        self.force_key = true;
    }

    fn flush(&mut self, out: &mut Vec<Packet>) -> Result<(), StreamError> {
        out.append(&mut self.queued);
        Ok(())
    }
}

/// Makes [`Vp9AlphaEncoder`]s.
#[derive(Clone, Copy, Debug, Default)]
pub struct Vp9AlphaFactory;

impl EncoderFactory for Vp9AlphaFactory {
    fn probe(&self) -> Vec<EncoderInfo> {
        if super::available().is_err() {
            return Vec::new();
        }
        let name = format!("libvpx-vp9 {}", super::version().unwrap_or_default());
        EncoderInfo::new(CodecId::Vp9Alpha, name, false, true).into_iter().collect()
    }

    fn create(&self, cfg: &EncodeConfig) -> Result<Box<dyn Encoder>, StreamError> {
        Ok(Box::new(Vp9AlphaEncoder::new(cfg)?))
    }
}
