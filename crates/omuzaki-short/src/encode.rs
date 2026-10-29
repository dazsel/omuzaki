//! Writing a short: VP9 video in good-quality mode (exports, not live takes), with the voice as an Opus track beside it,
//! in one `WebM` file a phone, a browser and every editor plays. Both codecs load at run time (libvpx, libopus: BSD);
//! a machine without libopus still gets the video, and the op writes the voice beside it as a WAV ([`wav_bytes`]).

use std::fs::File;
use std::io::Cursor;
use std::path::Path;

use omuzaki_stream::i420::rgba_to_i420_threads;
use omuzaki_stream::opus::{OpusEncoder, OpusPacket, OpusSettings};
use omuzaki_stream::sinks::webm::{WebmAudio, WebmSink, WebmTrack};
use omuzaki_stream::vp9::{self, EncoderSettings, I420, Packet, Vp9Encoder};

use crate::error::ShortError;
use crate::voice::RATE;

/// The VP9 constant-quality level: visually clean for the hard edges of toon shading and burned-in captions.
pub const CQ_LEVEL: u32 = 18;
/// The VP9 speed in good-quality mode: 0 is slowest, 5 fastest.
pub const CPU_USED: i32 = 3;
/// A keyframe at least this often, seconds, so an editor can seek.
pub const KEY_SECONDS: u32 = 2;
/// The voice's Opus bitrate, bits per second: transparent for speech in mono.
pub const VOICE_BITRATE: u32 = 96_000;
/// Threads converting RGBA to I420.
const CONVERT_THREADS: usize = 4;

/// What the writer made.
#[derive(Clone, Debug)]
pub struct Written {
    /// Bytes of the video file.
    pub bytes: u64,
    /// Frames in it.
    pub frames: u64,
    /// Whether the voice is in the video (false for a silent short, or without libopus).
    pub audio_in_video: bool,
}

/// Encodes frames into a `WebM` as they come.
pub struct ShortWriter {
    encoder: Vp9Encoder,
    sink: WebmSink,
    colour: I420,
    alpha: I420,
    packets: Vec<Packet>,
    key_every: u64,
    since_key: u64,
    frames: u64,
    width: u32,
    height: u32,
}

impl std::fmt::Debug for ShortWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShortWriter").field("frames", &self.frames).finish_non_exhaustive()
    }
}

fn encode_error(e: impl std::fmt::Display) -> ShortError {
    ShortError::Encode(e.to_string())
}

/// The voice as Opus packets, or `None` when libopus isn't on this machine.
fn opus_packets(pcm: &[f32]) -> Result<Option<(u16, Vec<OpusPacket>)>, ShortError> {
    if omuzaki_stream::opus::available().is_err() {
        return Ok(None);
    }
    let mut encoder = OpusEncoder::new(OpusSettings { channels: 1, bitrate: VOICE_BITRATE }).map_err(encode_error)?;
    let mut packets = Vec::new();
    encoder.encode(pcm, &mut packets).map_err(encode_error)?;
    encoder.flush(&mut packets).map_err(encode_error)?;
    Ok(Some((encoder.pre_skip(), packets)))
}

/// `pcm` (mono, [`RATE`]) as the bytes of a 16-bit WAV.
///
/// # Errors
/// [`ShortError::Encode`] if the WAV writer fails (it writes to memory, so only on a sample count past its limits).
pub fn wav_bytes(pcm: &[f32]) -> Result<Vec<u8>, ShortError> {
    let spec =
        hound::WavSpec { channels: 1, sample_rate: RATE, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut bytes = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut bytes, spec).map_err(encode_error)?;
    for &s in pcm {
        #[allow(clippy::cast_possible_truncation, reason = "clamped to the i16 range")]
        writer.write_sample((s.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16).map_err(encode_error)?;
    }
    writer.finalize().map_err(encode_error)?;
    Ok(bytes.into_inner())
}

impl ShortWriter {
    /// Starts a video of `size` at `fps` in `file` (open, empty, writable and seekable; `name` is for messages), with
    /// `voice` (mono, [`RATE`]) as its sound when given and libopus is on this machine.
    ///
    /// # Errors
    /// [`ShortError::Encode`] when libvpx is missing or refuses the settings, or the file can't be written.
    pub fn create(
        file: File,
        name: &Path,
        size: [u32; 2],
        fps: u32,
        voice: Option<&[f32]>,
    ) -> Result<Self, ShortError> {
        vp9::available().map_err(|e| {
            ShortError::Encode(format!("{e}; install libvpx (BSD) to write videos, as the recorder needs it too"))
        })?;
        let [width, height] = size;
        let encoder = Vp9Encoder::new(EncoderSettings {
            width,
            height,
            fps: (fps, 1),
            cq_level: CQ_LEVEL,
            cpu_used: CPU_USED,
            threads: vp9::default_threads(),
            studio_range: true,
            realtime: false,
        })
        .map_err(encode_error)?;
        let track = WebmTrack { width, height, fps: (fps, 1), alpha: false };
        let audio = voice.map(opus_packets).transpose()?.flatten();
        let header = audio.as_ref().map(|(pre_skip, _)| WebmAudio { channels: 1, pre_skip: *pre_skip });
        let mut sink = WebmSink::from_file(file, name, track, header).map_err(encode_error)?;
        // The whole voice is known before the first frame: queue it now and let the sink interleave it by time.
        if let Some((_, packets)) = audio {
            for packet in packets {
                sink.write_audio(packet.sample, &packet.data).map_err(encode_error)?;
            }
        }
        Ok(Self {
            encoder,
            sink,
            colour: I420::new(width, height),
            alpha: I420::new(width, height),
            packets: Vec::new(),
            key_every: u64::from(KEY_SECONDS * fps).max(1),
            since_key: 0,
            frames: 0,
            width,
            height,
        })
    }

    /// Encodes the next frame: straight RGBA8, the writer's size.
    ///
    /// # Errors
    /// [`ShortError::Encode`] for a frame of the wrong size, or when libvpx or the disk fails.
    pub fn push(&mut self, rgba: &[u8]) -> Result<(), ShortError> {
        if !rgba_to_i420_threads(rgba, self.width, self.height, &mut self.colour, &mut self.alpha, CONVERT_THREADS) {
            return Err(ShortError::Encode(format!("a frame's pixels don't match {}x{}", self.width, self.height)));
        }
        let index = self.frames;
        let key = self.since_key == 0 || self.since_key >= self.key_every || self.sink.needs_key(index);
        self.packets.clear();
        let pts = i64::try_from(index).unwrap_or(i64::MAX);
        self.encoder.encode(&self.colour, pts, key, &mut self.packets).map_err(encode_error)?;
        let packet = self
            .packets
            .first()
            .ok_or_else(|| ShortError::Encode(format!("libvpx returned no packet for frame {index}")))?;
        self.sink.write_frame(index, packet.key, &packet.data, None).map_err(encode_error)?;
        self.since_key = if packet.key { 1 } else { self.since_key + 1 };
        self.frames += 1;
        Ok(())
    }

    /// Finishes the file.
    ///
    /// # Errors
    /// [`ShortError::Encode`] when the last writes fail.
    pub fn finish(mut self) -> Result<Written, ShortError> {
        self.packets.clear();
        self.encoder.flush(&mut self.packets).map_err(encode_error)?;
        let audio_in_video = self.sink.audio().is_some();
        let bytes = self.sink.finish(self.frames).map_err(encode_error)?;
        Ok(Written { bytes, frames: self.frames, audio_in_video })
    }
}
