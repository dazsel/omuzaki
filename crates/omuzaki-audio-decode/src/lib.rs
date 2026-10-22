//! Audio file decoding for the soundboard and music (BR-21, #1444), always off the audio thread.
//!
//! - [`decode_file`] and [`decode_bytes`] read a whole file into a [`Sound`] for the player's bank: 48 kHz planar
//!   `f32`, mono or stereo, at most [`MAX_SOUND_SECONDS`].
//! - [`stream_file`] plays long music into a feed instead: a decoder thread writes 48 kHz interleaved stereo into the
//!   [`FeedWriter`] of a feed opened with [`music_feed`], keeping it [`STREAM_LEAD_SECONDS`] ahead.
//!
//! **Formats:** WAV and AIFF (PCM and ADPCM), FLAC, Ogg Vorbis, MP3 and AAC (ADTS or M4A), decoded by Symphonia
//! (MPL-2.0, used unmodified; `docs/licensing/deps/symphonia.toml`). The container is found from the bytes, with the
//! extension as a hint.
//!
//! **Rates:** anything other than 48 kHz goes through a Kaiser-windowed sinc resampler (`resample`) with no delay and
//! a stop band below -90 dB, far better than the engine's edge resampler, which is meant for devices.
//!
//! **Channels:** one stays mono and two stay stereo; more fold down to stereo, the first two channels as left and
//! right and every other channel added to both at -3 dB.
//!
//! **Levels:** a float file's NaN or infinite samples become silence and its samples are clipped at +24 dBFS, so a
//! damaged file can't poison the mix.
//!
//! **Damaged files** return [`DecodeError`] and never panic: a malformed packet inside an otherwise good file is
//! skipped, as players do, and a panic inside the decoder is caught and reported as [`DecodeError::Malformed`].
//! Paths reach here already checked by the calling operation (`OpCtx::fs_access`).

mod resample;

use std::fs::File;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use omuzaki_audio_api::ids::FeedId;
use omuzaki_audio_api::node::SAMPLE_RATE;
use omuzaki_audio_api::player::{MAX_SOUND_SECONDS, Sound};
use omuzaki_audio_api::tap::{FeedRequest, FeedWriter};
use omuzaki_core::time::Seconds;
use symphonia::core::codecs::audio::{AudioDecoder, AudioDecoderOptions};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSource, MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

use crate::resample::Resampler;

/// How far ahead of playback a music stream keeps its feed, in seconds.
pub const STREAM_LEAD_SECONDS: f64 = 2.0;

/// The sample rates a file may have, in Hz: telephone audio to studio masters. A damaged header outside them would
/// otherwise ask the resampler for a ratio of millions.
const MIN_RATE: u32 = 4_000;
const MAX_RATE: u32 = 768_000;

/// The loudest sample a file may carry, +24 dBFS: headroom for a hot float file, a bound for a damaged one.
const MAX_LEVEL: f32 = 16.0;

/// -3 dB: how much of each channel past the second goes into left and right when folding down.
const FOLD_GAIN: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Why a file didn't decode.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The file couldn't be opened or read.
    #[error("can't read the file: {0}")]
    Io(String),
    /// Not a format or codec Omuzaki decodes.
    #[error("not an audio format Omuzaki reads: {0}")]
    Unsupported(String),
    /// The file is damaged or truncated past use.
    #[error("the file is damaged: {0}")]
    Malformed(String),
    /// The file holds no audio track, or no audio.
    #[error("the file has no audio")]
    NoAudio,
    /// Longer than the bank takes: play it with [`stream_file`].
    #[error("a sound is at most {MAX_SOUND_SECONDS} s; play longer music as a stream")]
    TooLong,
}

/// Decodes the file at `path` into a bank sound.
///
/// # Errors
/// See [`DecodeError`].
pub fn decode_file(path: &Path) -> Result<Sound, DecodeError> {
    let file = File::open(path).map_err(|error| DecodeError::Io(error.to_string()))?;
    decode_source(Box::new(file), extension(path).as_deref())
}

/// Decodes a file already in memory; `extension` (without the dot) is a hint for the container.
///
/// # Errors
/// See [`DecodeError`].
pub fn decode_bytes(bytes: Vec<u8>, extension: Option<&str>) -> Result<Sound, DecodeError> {
    decode_source(Box::new(std::io::Cursor::new(bytes)), extension)
}

/// The feed request a music stream writes into: 48 kHz stereo with [`STREAM_LEAD_SECONDS`] of room.
#[must_use]
pub fn music_feed(id: FeedId) -> FeedRequest {
    FeedRequest {
        id,
        rate: SAMPLE_RATE,
        mono: false,
        capacity: Seconds(STREAM_LEAD_SECONDS),
        consumer: "music".to_owned(),
    }
}

/// Starts streaming the file at `path` into `writer`, which must come from a feed opened with [`music_feed`]. The
/// file is opened and its format found before this returns, so an unreadable file fails here; the rest decodes on
/// its own thread until the file ends, [`Stream::stop`] is called, or the feed's reader goes away.
///
/// # Errors
/// See [`DecodeError`]; [`DecodeError::Io`] also when the thread can't start.
pub fn stream_file(path: &Path, writer: FeedWriter) -> Result<Stream, DecodeError> {
    let file = File::open(path).map_err(|error| DecodeError::Io(error.to_string()))?;
    let source = guarded(|| Source::open(Box::new(file), extension(path).as_deref()))?;
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let thread = std::thread::Builder::new()
        .name("omuzaki-music-decode".to_owned())
        .spawn(move || guarded(|| stream(source, writer, &flag)))
        .map_err(|error| DecodeError::Io(error.to_string()))?;
    Ok(Stream { stop, thread: Some(thread) })
}

/// A music stream's decoder thread. Dropping it stops the stream without waiting.
#[derive(Debug)]
pub struct Stream {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<StreamEnd, DecodeError>>>,
}

/// How a stream ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamEnd {
    /// 48 kHz frames written to the feed.
    pub frames: u64,
    /// Whether it ended early: stopped, or the feed's reader went away.
    pub stopped: bool,
}

impl Stream {
    /// Asks the decoder thread to stop; it does within one write.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release);
    }

    /// Whether the decoder thread has ended.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    /// Waits for the decoder thread to end.
    ///
    /// # Errors
    /// What ended the stream, if the file went bad partway.
    pub fn join(mut self) -> Result<StreamEnd, DecodeError> {
        let thread = self.thread.take().ok_or_else(|| DecodeError::Io("the stream was already joined".to_owned()))?;
        thread.join().unwrap_or_else(|_| Err(DecodeError::Malformed("the decoder thread failed".to_owned())))
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.stop();
    }
}

fn extension(path: &Path) -> Option<String> {
    path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase)
}

/// Runs `f`, turning a panic inside the decoder into an error.
fn guarded<T>(f: impl FnOnce() -> Result<T, DecodeError>) -> Result<T, DecodeError> {
    catch_unwind(AssertUnwindSafe(f))
        .unwrap_or_else(|_| Err(DecodeError::Malformed("the decoder could not read this file".to_owned())))
}

fn decode_source(media: Box<dyn MediaSource>, extension: Option<&str>) -> Result<Sound, DecodeError> {
    guarded(|| {
        let mut source = Source::open(media, extension)?;
        let mut planes = Vec::new();
        let mut left = Vec::new();
        let mut right = Vec::new();
        let mut layout: Option<(u32, bool)> = None;
        while let Some(rate) = source.next(&mut planes)? {
            let stereo = planes.len() > 1;
            let (first_rate, first_stereo) = *layout.get_or_insert((rate, stereo));
            if (first_rate, first_stereo) != (rate, stereo) {
                return Err(DecodeError::Malformed("the rate or channels change partway through".to_owned()));
            }
            fold_into(&planes, &mut left, stereo.then_some(&mut right));
            if left.len() as u64 > u64::from(MAX_SOUND_SECONDS) * u64::from(rate) {
                return Err(DecodeError::TooLong);
            }
        }
        let (rate, stereo) = layout.ok_or(DecodeError::NoAudio)?;
        if left.is_empty() {
            return Err(DecodeError::NoAudio);
        }
        let mut channels = vec![resample::resample(&left, rate)];
        if stereo {
            channels.push(resample::resample(&right, rate));
        }
        // Rounding up at an odd rate can make a sound at the limit one sample longer than the bank takes.
        let limit = usize::try_from(MAX_SOUND_SECONDS * SAMPLE_RATE).unwrap_or(usize::MAX);
        for channel in &mut channels {
            channel.truncate(limit);
        }
        Sound::new(channels).map_err(|error| DecodeError::Malformed(error.to_string()))
    })
}

/// Folds decoded planes into left (or mono) and, when given, right, made safe for the mixer on the way.
fn fold_into(planes: &[Vec<f32>], left: &mut Vec<f32>, right: Option<&mut Vec<f32>>) {
    let Some(first) = planes.first() else {
        return;
    };
    let frames = first.len();
    let extra = planes.get(2..).unwrap_or(&[]);
    let start = left.len();
    left.extend(first.iter().copied().map(safe));
    if let Some(right) = right {
        right.extend(planes.get(1).map_or(&[][..], |p| &p[..frames.min(p.len())]).iter().copied().map(safe));
        right.resize(start + frames, 0.0);
        for plane in extra {
            for (i, sample) in plane.iter().copied().map(safe).take(frames).enumerate() {
                left[start + i] += sample * FOLD_GAIN;
                right[start + i] += sample * FOLD_GAIN;
            }
        }
    }
}

/// A float file can carry anything: NaN or infinity becomes silence (either would poison every bus it reaches), and
/// a level past [`MAX_LEVEL`] is clipped there.
fn safe(sample: f32) -> f32 {
    if sample.is_finite() { sample.clamp(-MAX_LEVEL, MAX_LEVEL) } else { 0.0 }
}

/// An open file: its reader, the audio track's decoder, and the track.
struct Source {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn AudioDecoder>,
    track: u32,
}

impl Source {
    fn open(media: Box<dyn MediaSource>, extension: Option<&str>) -> Result<Self, DecodeError> {
        let stream = MediaSourceStream::new(media, MediaSourceStreamOptions::default());
        let mut hint = Hint::new();
        if let Some(extension) = extension {
            hint.with_extension(extension);
        }
        let format = symphonia::default::get_probe()
            .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
            .map_err(|error| opening(&error))?;
        let track = format.default_track(TrackType::Audio).ok_or(DecodeError::NoAudio)?;
        let parameters = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or(DecodeError::NoAudio)?;
        let decoder = symphonia::default::get_codecs()
            .make_audio_decoder(parameters, &AudioDecoderOptions::default())
            .map_err(|error| opening(&error))?;
        let track = track.id;
        Ok(Self { format, decoder, track })
    }

    /// Decodes the next packet of the track into `planes`, one `Vec` per channel, and returns its rate; `None` at the
    /// end. A malformed packet is skipped.
    fn next(&mut self, planes: &mut Vec<Vec<f32>>) -> Result<Option<u32>, DecodeError> {
        loop {
            let packet = match self.format.next_packet() {
                Ok(Some(packet)) => packet,
                // The end, or a new chained stream (an Ogg playlist): a sound or a song ends where its first does.
                Ok(None) | Err(SymphoniaError::ResetRequired) => return Ok(None),
                // A truncated file ends where its data does.
                Err(SymphoniaError::IoError(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(None);
                }
                Err(error) => return Err(reading(&error)),
            };
            if packet.track_id != self.track {
                continue;
            }
            match self.decoder.decode(&packet) {
                Ok(buffer) => {
                    let rate = buffer.spec().rate();
                    if buffer.num_planes() == 0 {
                        return Err(DecodeError::Malformed("a packet with no channels".to_owned()));
                    }
                    if !(MIN_RATE..=MAX_RATE).contains(&rate) {
                        return Err(DecodeError::Unsupported(format!("a sample rate of {rate} Hz")));
                    }
                    buffer.copy_to_vecs_planar(planes);
                    planes.truncate(buffer.num_planes());
                    if buffer.frames() == 0 {
                        continue;
                    }
                    return Ok(Some(rate));
                }
                Err(SymphoniaError::DecodeError(_) | SymphoniaError::IoError(_)) => {}
                Err(error) => return Err(reading(&error)),
            }
        }
    }
}

fn opening(error: &SymphoniaError) -> DecodeError {
    match error {
        SymphoniaError::Unsupported(what) => DecodeError::Unsupported((*what).to_owned()),
        SymphoniaError::IoError(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            DecodeError::Malformed("the file ends before its audio starts".to_owned())
        }
        other => reading(other),
    }
}

fn reading(error: &SymphoniaError) -> DecodeError {
    match error {
        SymphoniaError::IoError(error) => DecodeError::Io(error.to_string()),
        SymphoniaError::Unsupported(what) => DecodeError::Unsupported((*what).to_owned()),
        other => DecodeError::Malformed(other.to_string()),
    }
}

/// The decoder thread: decode, fold to stereo, resample, interleave, and write ahead of playback.
fn stream(mut source: Source, mut writer: FeedWriter, stop: &AtomicBool) -> Result<StreamEnd, DecodeError> {
    let mut planes = Vec::new();
    let (mut left, mut right) = (Vec::new(), Vec::new());
    let (mut out_left, mut out_right) = (Vec::new(), Vec::new());
    let mut interleaved = Vec::new();
    let mut resamplers: Option<(u32, [Resampler; 2])> = None;
    let mut frames = 0u64;
    loop {
        let next = source.next(&mut planes)?;
        left.clear();
        right.clear();
        out_left.clear();
        out_right.clear();
        let Some(rate) = next else {
            if let Some((_, [l, r])) = &mut resamplers {
                l.finish(&mut out_left);
                r.finish(&mut out_right);
            }
            frames += out_left.len() as u64;
            let stopped = !write_stereo(&mut writer, &out_left, &out_right, &mut interleaved, stop);
            return Ok(StreamEnd { frames, stopped });
        };
        fold_into(&planes, &mut left, Some(&mut right));
        if planes.len() == 1 {
            // Mono plays on both sides at full level, as the player's sounds do through a stereo feed.
            right.clone_from(&left);
        }
        let (first_rate, [l, r]) =
            resamplers.get_or_insert_with(|| (rate, [Resampler::new(rate), Resampler::new(rate)]));
        if *first_rate != rate {
            return Err(DecodeError::Malformed("the rate changes partway through".to_owned()));
        }
        l.push(&left, &mut out_left);
        r.push(&right, &mut out_right);
        frames += out_left.len() as u64;
        if !write_stereo(&mut writer, &out_left, &out_right, &mut interleaved, stop) {
            return Ok(StreamEnd { frames, stopped: true });
        }
    }
}

/// Writes left and right interleaved, waiting for room; `false` when stopped or the reader went away first.
fn write_stereo(
    writer: &mut FeedWriter,
    left: &[f32],
    right: &[f32],
    interleaved: &mut Vec<f32>,
    stop: &AtomicBool,
) -> bool {
    interleaved.clear();
    interleaved.extend(left.iter().zip(right).flat_map(|(l, r)| [*l, *r]));
    let mut rest = interleaved.as_slice();
    while !rest.is_empty() {
        if stop.load(Ordering::Acquire) || writer.reader_gone() {
            return false;
        }
        // Whole frames only, so left and right never swap.
        let room = (writer.free() & !1).min(rest.len());
        if room == 0 {
            // The feed is full: playback drains it at 48 kHz, so a few milliseconds frees plenty.
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let accepted = writer.push(&rest[..room]);
        rest = &rest[accepted..];
    }
    true
}
