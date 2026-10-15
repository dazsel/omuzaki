//! What a stream is, the frame an encoder or output receives, and the encoder traits.

use std::any::Any;

use bytes::Bytes;
use omuzaki_core::glam::UVec2;
use omuzaki_render_api::Image;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::StreamError;
use crate::packet::Packet;
use crate::time::{Ticks, Timebase};

/// What a stream carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    /// Pictures.
    Video,
    /// Samples.
    Audio,
    /// Opaque timed records (the input track).
    Data,
}

/// How a stream's bytes are coded. Grows with the formats of `docs/design/record.md` D1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CodecId {
    /// Apple `ProRes` 4444, with alpha.
    #[serde(rename = "prores_4444")]
    #[schemars(rename = "prores_4444")]
    ProRes4444,
    /// VP9 with the alpha plane as side data (the default record format).
    Vp9Alpha,
    /// One PNG per frame.
    Png,
    /// H.264, no alpha.
    H264,
    /// Signed 24-bit PCM.
    PcmS24,
    /// 32-bit float PCM.
    PcmF32,
    /// Opus.
    Opus,
    /// The input track: opaque records, one per frame.
    InputRecords,
}

impl CodecId {
    /// The kind of stream this codec codes; a [`StreamSpec`] or [`EncodeConfig`] with another kind is invalid.
    pub fn kind(self) -> StreamKind {
        match self {
            Self::ProRes4444 | Self::Vp9Alpha | Self::Png | Self::H264 => StreamKind::Video,
            Self::PcmS24 | Self::PcmF32 | Self::Opus => StreamKind::Audio,
            Self::InputRecords => StreamKind::Data,
        }
    }
}

/// How alpha relates to colour in a frame or stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlphaKind {
    /// No alpha channel.
    None,
    /// Colour is independent of alpha.
    Straight,
    /// Colour is already multiplied by alpha.
    Premultiplied,
}

/// The picture format of a video stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct VideoSpec {
    /// Width and height in pixels.
    #[schemars(with = "[u32; 2]")]
    pub size: UVec2,
    /// Frames per second as `(numerator, denominator)`.
    pub fps: (u32, u32),
    /// How alpha is carried.
    pub alpha: AlphaKind,
}

impl VideoSpec {
    /// Checks that the size and frame rate are non-zero.
    ///
    /// # Errors
    /// [`StreamError::Config`] naming the offending field.
    pub fn validate(&self) -> Result<(), StreamError> {
        if self.size.x == 0 || self.size.y == 0 {
            return Err(StreamError::Config(format!("video size {}x{} has a zero side", self.size.x, self.size.y)));
        }
        if self.fps.0 == 0 || self.fps.1 == 0 {
            return Err(StreamError::Config(format!("frame rate {}/{} has a zero part", self.fps.0, self.fps.1)));
        }
        Ok(())
    }
}

/// The sample format of an audio stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct AudioSpec {
    /// Samples per second per channel.
    pub rate: u32,
    /// Channel count.
    pub channels: u8,
}

impl AudioSpec {
    /// Checks that the rate and channel count are non-zero.
    ///
    /// # Errors
    /// [`StreamError::Config`] naming the offending field.
    pub fn validate(&self) -> Result<(), StreamError> {
        if self.rate == 0 || self.channels == 0 {
            return Err(StreamError::Config(format!(
                "audio {} Hz x {} channels has a zero part",
                self.rate, self.channels
            )));
        }
        Ok(())
    }
}

/// Everything a muxer needs to add a stream; an encoder reports it once it is open.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct StreamSpec {
    /// What the stream carries.
    pub kind: StreamKind,
    /// How it is coded.
    pub codec: CodecId,
    /// A label for the track (`program`, `mic`).
    pub name: String,
    /// The unit of its packets' times.
    pub timebase: Timebase,
    /// Set for [`StreamKind::Video`].
    pub video: Option<VideoSpec>,
    /// Set for [`StreamKind::Audio`].
    pub audio: Option<AudioSpec>,
    /// Codec parameters the muxer writes in the header (D6: a take continues in one file only while they match).
    #[serde(with = "bytes_as_vec")]
    #[schemars(with = "Vec<u8>")]
    pub extradata: Bytes,
}

impl StreamSpec {
    /// A video stream, with empty extradata.
    ///
    /// # Errors
    /// [`StreamError::Config`] if the picture format or timebase is invalid, or the codec is not a video codec.
    pub fn video(
        codec: CodecId,
        name: impl Into<String>,
        timebase: Timebase,
        video: VideoSpec,
    ) -> Result<Self, StreamError> {
        video.validate()?;
        check_timebase(timebase)?;
        check_codec(codec, StreamKind::Video)?;
        Ok(Self {
            kind: StreamKind::Video,
            codec,
            name: name.into(),
            timebase,
            video: Some(video),
            audio: None,
            extradata: Bytes::new(),
        })
    }

    /// An audio stream, with empty extradata.
    ///
    /// # Errors
    /// [`StreamError::Config`] if the sample format or timebase is invalid, or the codec is not an audio codec.
    pub fn audio(
        codec: CodecId,
        name: impl Into<String>,
        timebase: Timebase,
        audio: AudioSpec,
    ) -> Result<Self, StreamError> {
        audio.validate()?;
        check_timebase(timebase)?;
        check_codec(codec, StreamKind::Audio)?;
        Ok(Self {
            kind: StreamKind::Audio,
            codec,
            name: name.into(),
            timebase,
            video: None,
            audio: Some(audio),
            extradata: Bytes::new(),
        })
    }

    /// A data stream, with empty extradata.
    ///
    /// # Errors
    /// [`StreamError::Config`] if the timebase is invalid or the codec is not a data codec.
    pub fn data(codec: CodecId, name: impl Into<String>, timebase: Timebase) -> Result<Self, StreamError> {
        check_timebase(timebase)?;
        check_codec(codec, StreamKind::Data)?;
        Ok(Self {
            kind: StreamKind::Data,
            codec,
            name: name.into(),
            timebase,
            video: None,
            audio: None,
            extradata: Bytes::new(),
        })
    }

    /// Returns the spec with these codec parameters.
    #[must_use]
    pub fn with_extradata(mut self, extradata: Bytes) -> Self {
        self.extradata = extradata;
        self
    }

    /// Checks a spec that was deserialised or built field by field: the payload matches the kind and every number is valid.
    ///
    /// # Errors
    /// [`StreamError::Config`] describing the first problem.
    pub fn validate(&self) -> Result<(), StreamError> {
        check_timebase(self.timebase)?;
        check_codec(self.codec, self.kind)?;
        match (self.kind, &self.video, &self.audio) {
            (StreamKind::Video, Some(v), None) => v.validate(),
            (StreamKind::Audio, None, Some(a)) => a.validate(),
            (StreamKind::Data, None, None) => Ok(()),
            _ => Err(StreamError::Config(format!("a {:?} stream has the wrong video/audio payload", self.kind))),
        }
    }
}

fn check_codec(codec: CodecId, kind: StreamKind) -> Result<(), StreamError> {
    if codec.kind() != kind {
        return Err(StreamError::Config(format!("{codec:?} is a {:?} codec, not {kind:?}", codec.kind())));
    }
    Ok(())
}

fn check_timebase(tb: Timebase) -> Result<(), StreamError> {
    if tb.num == 0 || tb.den == 0 {
        return Err(StreamError::Config(format!("timebase {}/{} has a zero part", tb.num, tb.den)));
    }
    Ok(())
}

/// Serde for [`Bytes`] as a byte array, so the crate needs no `bytes/serde` feature.
mod bytes_as_vec {
    use bytes::Bytes;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(bytes: &Bytes, s: S) -> Result<S::Ok, S::Error> {
        bytes.as_ref().serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Bytes, D::Error> {
        Vec::<u8>::deserialize(d).map(Bytes::from)
    }
}

/// One rendered frame, lent to an encoder or output for the duration of the call.
///
/// The frame holds borrows only; whoever keeps pixels copies them (an output copies into a pooled slot).
#[non_exhaustive]
pub struct Frame<'a> {
    /// Width and height in pixels.
    pub size: UVec2,
    /// Count of frames since the start of the take.
    pub index: u64,
    /// When the frame was captured, on the pipeline clock.
    pub capture_time: Ticks,
    /// The pixels on the CPU (`read_target`'s result), already in the alpha form the output asked for.
    pub cpu: Option<&'a Image>,
    /// The target's native GPU handles, the painters' convention (`(wgpu::Device, wgpu::Queue, wgpu::Texture)`);
    /// the consumer calls `downcast_ref`. The borrow is shared because one frame fans out to several outputs in
    /// turn. No GPU type enters this crate.
    pub gpu: Option<&'a dyn Any>,
}

impl<'a> Frame<'a> {
    /// A frame with no pixels attached yet; add them with [`Frame::with_cpu`] or [`Frame::with_gpu`].
    pub fn new(size: UVec2, index: u64, capture_time: Ticks) -> Self {
        Self { size, index, capture_time, cpu: None, gpu: None }
    }

    /// Attaches the CPU image.
    #[must_use]
    pub fn with_cpu(mut self, image: &'a Image) -> Self {
        self.cpu = Some(image);
        self
    }

    /// Attaches the native GPU handles.
    #[must_use]
    pub fn with_gpu(mut self, gpu: &'a dyn Any) -> Self {
        self.gpu = Some(gpu);
        self
    }
}

/// How to open an encoder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct EncodeConfig {
    /// The picture format fed to the encoder.
    pub video: VideoSpec,
    /// The codec to use.
    pub codec: CodecId,
    /// Target bitrate; `None` lets the encoder choose.
    pub bitrate_kbps: Option<u32>,
    /// Frames between keyframes; one second of frames by default (D3), `1` for an intra-only take.
    pub keyint: u32,
    /// Encoder thread count; `None` lets the encoder choose.
    pub threads: Option<u8>,
    /// Whether the encoder must keep up with live input (it may then trade quality for speed).
    pub realtime: bool,
}

impl EncodeConfig {
    /// A config with defaults: encoder-chosen bitrate and threads, a keyframe every second, real time.
    ///
    /// # Errors
    /// [`StreamError::Config`] if the picture format is invalid or the codec is not a video codec.
    pub fn new(video: VideoSpec, codec: CodecId) -> Result<Self, StreamError> {
        video.validate()?;
        check_codec(codec, StreamKind::Video)?;
        let keyint = video.fps.0.div_ceil(video.fps.1).max(1);
        Ok(Self { video, codec, bitrate_kbps: None, keyint, threads: None, realtime: true })
    }

    /// Checks a config that was deserialised or edited.
    ///
    /// # Errors
    /// [`StreamError::Config`] describing the first problem.
    pub fn validate(&self) -> Result<(), StreamError> {
        self.video.validate()?;
        check_codec(self.codec, StreamKind::Video)?;
        if self.keyint == 0 {
            return Err(StreamError::Config("keyint is zero".into()));
        }
        if self.bitrate_kbps == Some(0) || self.threads == Some(0) {
            return Err(StreamError::Config("bitrate and threads must be positive when set".into()));
        }
        Ok(())
    }
}

/// What a factory can build.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct EncoderInfo {
    /// The codec it produces.
    pub codec: CodecId,
    /// The implementation's name (`libvpx-vp9`, `h264_nvenc`).
    pub name: String,
    /// Whether it runs on a hardware encoder.
    pub hardware: bool,
    /// Whether it keeps the alpha channel.
    pub alpha: bool,
}

impl EncoderInfo {
    /// Describes one encoder.
    ///
    /// # Errors
    /// [`StreamError::Config`] if `codec` is not a video codec: an [`EncodeConfig`] can only select video codecs, so
    /// an encoder for another kind could never be asked for.
    pub fn new(codec: CodecId, name: impl Into<String>, hardware: bool, alpha: bool) -> Result<Self, StreamError> {
        let info = Self { codec, name: name.into(), hardware, alpha };
        info.validate()?;
        Ok(info)
    }

    /// Checks an info built field by field: its codec is a video codec, as [`EncodeConfig::validate`] requires.
    ///
    /// # Errors
    /// [`StreamError::Config`] naming the codec and its kind.
    pub fn validate(&self) -> Result<(), StreamError> {
        check_codec(self.codec, StreamKind::Video)
    }
}

/// One open encoder. Not real-time safe to create; `send` and `receive` are called from the encoder's own thread.
pub trait Encoder: Send {
    /// The stream this encoder produces, complete with extradata once open.
    fn spec(&self) -> StreamSpec;

    /// Queues a frame.
    ///
    /// # Errors
    /// [`StreamError::Full`] if the queue is full (the caller repeats the previous frame, D2), [`StreamError::Encode`]
    /// on failure, [`StreamError::Stopped`] after `flush`.
    fn send(&mut self, frame: &Frame<'_>) -> Result<(), StreamError>;

    /// Appends the packets that are ready to `out`.
    ///
    /// # Errors
    /// [`StreamError::Encode`] on failure.
    fn receive(&mut self, out: &mut Vec<Packet>) -> Result<(), StreamError>;

    /// Asks the next frame to be a keyframe (a part ends, an encoder restarts).
    fn request_keyframe(&mut self);

    /// Ends the stream and appends every remaining packet to `out`.
    ///
    /// # Errors
    /// [`StreamError::Encode`] on failure.
    fn flush(&mut self, out: &mut Vec<Packet>) -> Result<(), StreamError>;
}

/// Finds and opens encoders.
pub trait EncoderFactory: Send + Sync {
    /// The encoders available on this machine; empty if none, never an error.
    fn probe(&self) -> Vec<EncoderInfo>;

    /// Opens an encoder.
    ///
    /// # Errors
    /// [`StreamError::Unavailable`] with a hint if the codec or library is missing, [`StreamError::Config`] for a bad
    /// config.
    fn create(&self, cfg: &EncodeConfig) -> Result<Box<dyn Encoder>, StreamError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyint_defaults_to_one_second() {
        let v = VideoSpec { size: UVec2::new(8, 8), fps: (30000, 1001), alpha: AlphaKind::None };
        assert_eq!(EncodeConfig::new(v, CodecId::H264).map(|c| c.keyint), Ok(30));
    }

    fn json<T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug>(v: T, text: &str) {
        assert_eq!(serde_json::to_string(&v).ok().as_deref(), Some(text));
        assert_eq!(serde_json::from_str::<T>(text).ok(), Some(v));
    }

    #[test]
    fn codec_ids_use_the_design_names() {
        json(CodecId::ProRes4444, "\"prores_4444\"");
        json(CodecId::Vp9Alpha, "\"vp9_alpha\"");
        json(CodecId::Png, "\"png\"");
        json(CodecId::H264, "\"h264\"");
        json(CodecId::PcmS24, "\"pcm_s24\"");
        json(CodecId::PcmF32, "\"pcm_f32\"");
        json(CodecId::Opus, "\"opus\"");
        json(CodecId::InputRecords, "\"input_records\"");
    }

    #[test]
    fn stream_and_alpha_kinds_are_snake_case() {
        json(StreamKind::Video, "\"video\"");
        json(StreamKind::Audio, "\"audio\"");
        json(StreamKind::Data, "\"data\"");
        json(AlphaKind::None, "\"none\"");
        json(AlphaKind::Straight, "\"straight\"");
        json(AlphaKind::Premultiplied, "\"premultiplied\"");
    }

    #[test]
    fn encode_config_golden_json() {
        let v = VideoSpec { size: UVec2::new(8, 8), fps: (30, 1), alpha: AlphaKind::Straight };
        let Ok(cfg) = EncodeConfig::new(v, CodecId::Vp9Alpha) else { panic!("valid config") };
        let text = serde_json::to_string(&cfg).unwrap_or_default();
        assert!(text.contains("\"codec\":\"vp9_alpha\""), "{text}");
        assert!(text.contains("\"alpha\":\"straight\""), "{text}");
        assert_eq!(serde_json::from_str::<EncodeConfig>(&text).ok(), Some(cfg));
    }

    #[test]
    fn encoder_info_requires_a_video_codec() {
        assert!(EncoderInfo::new(CodecId::H264, "h264_nvenc", true, false).is_ok());
        for codec in [CodecId::Opus, CodecId::PcmS24, CodecId::PcmF32, CodecId::InputRecords] {
            assert!(matches!(EncoderInfo::new(codec, "x", false, false), Err(StreamError::Config(_))), "{codec:?}");
        }
        let Ok(mut info) = EncoderInfo::new(CodecId::Png, "png", false, true) else { panic!("valid info") };
        info.codec = CodecId::Opus;
        assert!(info.validate().is_err());
    }
}
