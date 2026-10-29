//! Opus through libopus (BSD-3-Clause), loaded at run time: the voice of a `WebM` take, in the same file as its VP9
//! picture (`sinks::webm`).
//!
//! The library is the system's own (`libopus.so.0` on Linux, `libopus*.dylib` on macOS, `opus.dll` on Windows) or the
//! file `OMUZAKI_LIBOPUS` names. Nothing links it at build time, so a machine without it still runs the app: the take
//! keeps its audio in a WAV file beside the video and says why.
//!
//! The encoder takes 48 kHz float PCM in chunks of any length and returns 20 ms packets, each with the position of its
//! first input sample, which is the packet's timestamp in the container. The decoder's output runs
//! [`OpusEncoder::pre_skip`] samples behind that, which the container says through the `OpusHead` pre-skip and
//! Matroska's `CodecDelay`.
#![allow(unsafe_code)] // loading libopus and calling its C functions; each call says why it is sound

pub mod ffi;

use std::ffi::{CStr, c_char, c_int};
use std::ptr::NonNull;
use std::sync::OnceLock;

use libloading::Library;
use omuzaki_stream_api::error::StreamError;

use ffi::Api;

/// The environment variable that names a libopus file to load instead of the system's.
pub const LIBRARY_ENV: &str = "OMUZAKI_LIBOPUS";
/// The sample rate the encoder takes and the decoder returns, in Hz: Opus's own.
pub const SAMPLE_RATE: u32 = 48_000;
/// Samples per channel in one packet: 20 ms at [`SAMPLE_RATE`].
pub const FRAME_SAMPLES: usize = 960;
/// The lowest bitrate the encoder takes, in bits per second (libopus's floor).
pub const MIN_BITRATE: u32 = 6_000;
/// The highest bitrate the encoder takes, in bits per second (libopus's ceiling for stereo).
pub const MAX_BITRATE: u32 = 510_000;
/// The most channels the encoder takes (mapping family 0: mono or stereo).
pub const MAX_CHANNELS: u8 = 2;
/// The most bytes one packet can need: libopus's recommended output buffer.
const MAX_PACKET: usize = 4000;
/// The most samples per channel one packet can decode to: 120 ms.
const MAX_DECODE_SAMPLES: usize = 5760;
/// The encoder's complexity, 0 to 10: the best, which costs well under a millisecond per 20 ms frame.
const COMPLEXITY: c_int = 10;

/// The file names tried, newest soname first.
#[cfg(target_os = "linux")]
const CANDIDATES: &[&str] = &["libopus.so.0", "libopus.so"];
#[cfg(target_os = "macos")]
const CANDIDATES: &[&str] =
    &["libopus.0.dylib", "libopus.dylib", "/opt/homebrew/lib/libopus.dylib", "/usr/local/lib/libopus.dylib"];
#[cfg(target_os = "windows")]
const CANDIDATES: &[&str] = &["opus.dll", "libopus-0.dll", "libopus.dll"];
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
const CANDIDATES: &[&str] = &[];

/// The loaded library: its handle, kept for the life of the process, and its functions.
struct Loaded {
    _library: Library,
    api: Api,
    version: String,
}

static LIBRARY: OnceLock<Result<Loaded, String>> = OnceLock::new();

fn symbol<F: Copy>(library: &Library, name: &str) -> Result<F, String> {
    // SAFETY: each `F` is the signature `name` has in libopus's public headers (the table in `ffi`); `Option<F>` reads
    // a null address as `None`.
    let symbol = unsafe { library.get::<Option<F>>(name.as_bytes()) }.map_err(|e| format!("no `{name}`: {e}"))?;
    (*symbol).ok_or_else(|| format!("`{name}` is null"))
}

fn open(path: &str) -> Result<Loaded, String> {
    // SAFETY: loading libopus runs no initialiser beyond its own tables; the handle is kept for the life of the
    // process, so no function pointer taken from it outlives it.
    let library = unsafe { Library::new(path) }.map_err(|e| e.to_string())?;
    let api = Api {
        encoder_create: symbol(&library, "opus_encoder_create")?,
        encode_float: symbol(&library, "opus_encode_float")?,
        encoder_ctl: symbol(&library, "opus_encoder_ctl")?,
        encoder_destroy: symbol(&library, "opus_encoder_destroy")?,
        decoder_create: symbol(&library, "opus_decoder_create")?,
        decode_float: symbol(&library, "opus_decode_float")?,
        decoder_destroy: symbol(&library, "opus_decoder_destroy")?,
        strerror: symbol(&library, "opus_strerror")?,
        version_string: symbol(&library, "opus_get_version_string")?,
    };
    // SAFETY: `opus_get_version_string` takes nothing and returns a static NUL-terminated string.
    let version = unsafe { c_string((api.version_string)()) };
    Ok(Loaded { _library: library, api, version })
}

fn load() -> Result<&'static Loaded, StreamError> {
    let loaded = LIBRARY.get_or_init(|| {
        if let Ok(path) = std::env::var(LIBRARY_ENV) {
            return open(&path).map_err(|e| format!("{LIBRARY_ENV}: {e}"));
        }
        let mut last = String::from("no candidate names for this system");
        for name in CANDIDATES {
            match open(name) {
                Ok(loaded) => return Ok(loaded),
                Err(e) => last = e,
            }
        }
        Err(last)
    });
    loaded.as_ref().map_err(|e| StreamError::Unavailable {
        what: "libopus (Opus)".into(),
        hint: format!(
            "install libopus (Debian and Ubuntu: libopus0; Fedora: opus; Homebrew: opus) or set {LIBRARY_ENV} to the library file; the take's audio stays in a WAV file beside the video ({e})"
        ),
    })
}

/// Whether libopus loads on this machine.
///
/// # Errors
/// [`StreamError::Unavailable`] with what a person can do about it.
pub fn available() -> Result<(), StreamError> {
    load().map(|_| ())
}

/// The loaded library's version string, such as `libopus 1.4`.
///
/// # Errors
/// [`StreamError::Unavailable`] when libopus does not load.
pub fn version() -> Result<String, StreamError> {
    load().map(|l| l.version.clone())
}

/// # Safety
/// `p` is null or points to a NUL-terminated string that lives for this call.
unsafe fn c_string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: non-null and NUL-terminated by the caller's contract.
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

/// libopus's text for error code `code`.
fn opus_error(api: &Api, what: &str, code: c_int) -> StreamError {
    // SAFETY: `opus_strerror` takes any int and returns a static string.
    let text = unsafe { c_string((api.strerror)(code)) };
    StreamError::Encode(format!("Opus {what}: {text} ({code})"))
}

fn check_channels(channels: u8) -> Result<(), StreamError> {
    if channels == 0 || channels > MAX_CHANNELS {
        return Err(StreamError::Config(format!("Opus takes 1 to {MAX_CHANNELS} channels; got {channels}")));
    }
    Ok(())
}

/// What an encoder makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OpusSettings {
    /// Channels: 1 (mono) or 2 (stereo, interleaved left then right).
    pub channels: u8,
    /// Target bitrate in bits per second, [`MIN_BITRATE`] to [`MAX_BITRATE`].
    pub bitrate: u32,
}

/// One compressed 20 ms packet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpusPacket {
    /// The position, in samples per channel at [`SAMPLE_RATE`], of the packet's first input sample: a multiple of
    /// [`FRAME_SAMPLES`] counted from the first sample fed.
    pub sample: u64,
    /// The Opus packet.
    pub data: Vec<u8>,
}

/// An Opus encoder for 48 kHz float PCM, producing one packet per 20 ms.
pub struct OpusEncoder {
    api: Api,
    state: NonNull<ffi::Encoder>,
    settings: OpusSettings,
    pre_skip: u16,
    /// Interleaved samples short of a whole frame, kept for the next call.
    pending: Vec<f32>,
    /// Samples per channel fed so far.
    fed: u64,
    /// Samples per channel encoded so far: a multiple of [`FRAME_SAMPLES`].
    encoded: u64,
    scratch: Vec<u8>,
}

// SAFETY: the state is used from one thread at a time (`&mut self` on every call that touches it); libopus keeps no
// thread-local state, so moving it between threads is sound.
unsafe impl Send for OpusEncoder {}

impl std::fmt::Debug for OpusEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusEncoder")
            .field("settings", &self.settings)
            .field("pre_skip", &self.pre_skip)
            .field("fed", &self.fed)
            .finish_non_exhaustive()
    }
}

impl OpusEncoder {
    /// An encoder for `settings`.
    ///
    /// # Errors
    /// [`StreamError::Unavailable`] when libopus does not load; [`StreamError::Config`] for a channel count or bitrate
    /// out of range; [`StreamError::Encode`] when libopus fails to start.
    pub fn new(settings: OpusSettings) -> Result<Self, StreamError> {
        check_channels(settings.channels)?;
        if !(MIN_BITRATE..=MAX_BITRATE).contains(&settings.bitrate) {
            return Err(StreamError::Config(format!(
                "Opus takes {MIN_BITRATE} to {MAX_BITRATE} bits per second; got {}",
                settings.bitrate
            )));
        }
        let api = load()?.api;
        let rate = i32::try_from(SAMPLE_RATE).map_err(|_| StreamError::Config("sample rate out of range".into()))?;
        let bitrate =
            i32::try_from(settings.bitrate).map_err(|_| StreamError::Config("bitrate out of range".into()))?;
        let mut err: c_int = 0;
        // SAFETY: the rate, channel count and application are values `opus.h` names; `err` is a valid out pointer.
        let state =
            unsafe { (api.encoder_create)(rate, c_int::from(settings.channels), ffi::APPLICATION_AUDIO, &raw mut err) };
        let state = match NonNull::new(state) {
            Some(state) if err == ffi::OK => state,
            Some(state) => {
                // SAFETY: libopus returned a state it allocated; it is freed once and never used again.
                unsafe { (api.encoder_destroy)(state.as_ptr()) };
                return Err(opus_error(&api, "encoder", err));
            }
            None => return Err(opus_error(&api, "encoder", err)),
        };
        let mut encoder = Self {
            api,
            state,
            settings,
            pre_skip: 0,
            pending: Vec::with_capacity(FRAME_SAMPLES * usize::from(settings.channels)),
            fed: 0,
            encoded: 0,
            scratch: vec![0; MAX_PACKET],
        };
        encoder.set(ffi::SET_BITRATE, bitrate)?;
        encoder.set(ffi::SET_COMPLEXITY, COMPLEXITY)?;
        let mut lookahead: i32 = 0;
        // SAFETY: `OPUS_GET_LOOKAHEAD` takes one `opus_int32 *`, which points at a live `i32` for the call.
        let code = unsafe { (api.encoder_ctl)(encoder.state.as_ptr(), ffi::GET_LOOKAHEAD, &raw mut lookahead) };
        if code != ffi::OK {
            return Err(opus_error(&api, "lookahead", code));
        }
        encoder.pre_skip = u16::try_from(lookahead)
            .map_err(|_| StreamError::Encode(format!("Opus reported a lookahead of {lookahead} samples")))?;
        Ok(encoder)
    }

    fn set(&mut self, request: c_int, value: i32) -> Result<(), StreamError> {
        // SAFETY: every set request used here takes one `opus_int32`, passed as is through C's variadic call; the
        // state is live and only this call touches it.
        let code = unsafe { (self.api.encoder_ctl)(self.state.as_ptr(), request, value) };
        if code != ffi::OK {
            return Err(opus_error(&self.api, &format!("control {request}"), code));
        }
        Ok(())
    }

    /// The settings it was made with.
    #[must_use]
    pub fn settings(&self) -> OpusSettings {
        self.settings
    }

    /// The samples per channel at [`SAMPLE_RATE`] a decoder drops from the start: the `OpusHead` pre-skip.
    #[must_use]
    pub fn pre_skip(&self) -> u16 {
        self.pre_skip
    }

    /// Samples per channel fed so far.
    #[must_use]
    pub fn samples_fed(&self) -> u64 {
        self.fed
    }

    /// Encodes `pcm`, interleaved samples at [`SAMPLE_RATE`] in -1 to 1, and appends every whole 20 ms packet to
    /// `out`; samples short of a packet wait for the next call.
    ///
    /// # Errors
    /// [`StreamError::Config`] when `pcm` does not hold whole samples for every channel; [`StreamError::Encode`] when
    /// libopus fails.
    pub fn encode(&mut self, pcm: &[f32], out: &mut Vec<OpusPacket>) -> Result<(), StreamError> {
        let channels = usize::from(self.settings.channels);
        if !pcm.len().is_multiple_of(channels) {
            return Err(StreamError::Config(format!(
                "{} samples are not whole frames of {channels} channels",
                pcm.len()
            )));
        }
        let frame = FRAME_SAMPLES * channels;
        let mut rest = pcm;
        if !self.pending.is_empty() {
            let take = (frame - self.pending.len()).min(rest.len());
            self.pending.extend_from_slice(&rest[..take]);
            rest = &rest[take..];
            if self.pending.len() == frame {
                self.encode_pending(out)?;
            }
        }
        while rest.len() >= frame {
            self.encode_frame(&rest[..frame], out)?;
            rest = &rest[frame..];
        }
        self.pending.extend_from_slice(rest);
        self.fed += (pcm.len() / channels) as u64;
        Ok(())
    }

    /// Ends the stream: pads the last frame with silence and encodes until the [`pre_skip`](Self::pre_skip) samples
    /// still inside the encoder are out too, appending the packets to `out`. Call it once, after the last
    /// [`encode`](Self::encode).
    ///
    /// # Errors
    /// [`StreamError::Encode`] when libopus fails.
    pub fn flush(&mut self, out: &mut Vec<OpusPacket>) -> Result<(), StreamError> {
        if self.fed == 0 {
            return Ok(());
        }
        let frame = FRAME_SAMPLES * usize::from(self.settings.channels);
        let end = self.fed + u64::from(self.pre_skip);
        while self.encoded < end {
            self.pending.resize(frame, 0.0);
            self.encode_pending(out)?;
        }
        Ok(())
    }

    fn encode_pending(&mut self, out: &mut Vec<OpusPacket>) -> Result<(), StreamError> {
        let pending = std::mem::take(&mut self.pending);
        let result = self.encode_frame(&pending, out);
        self.pending = pending;
        self.pending.clear();
        result
    }

    /// Encodes one whole frame of interleaved samples.
    fn encode_frame(&mut self, frame: &[f32], out: &mut Vec<OpusPacket>) -> Result<(), StreamError> {
        debug_assert_eq!(frame.len(), FRAME_SAMPLES * usize::from(self.settings.channels), "one whole frame");
        let samples = c_int::try_from(FRAME_SAMPLES).map_err(|_| StreamError::Encode("frame too long".into()))?;
        let room = i32::try_from(self.scratch.len()).map_err(|_| StreamError::Encode("buffer too long".into()))?;
        // SAFETY: `frame` holds `FRAME_SAMPLES` samples for each channel the state was made with, `scratch` holds
        // `room` writable bytes, and both live across the call.
        let written = unsafe {
            (self.api.encode_float)(self.state.as_ptr(), frame.as_ptr(), samples, self.scratch.as_mut_ptr(), room)
        };
        let len = usize::try_from(written).map_err(|_| opus_error(&self.api, "encode", written))?;
        let data = self.scratch.get(..len).ok_or_else(|| StreamError::Encode("Opus overran its buffer".into()))?;
        out.push(OpusPacket { sample: self.encoded, data: data.to_vec() });
        self.encoded += FRAME_SAMPLES as u64;
        Ok(())
    }
}

impl Drop for OpusEncoder {
    fn drop(&mut self) {
        // SAFETY: the state was made in `new` and is destroyed once.
        unsafe { (self.api.encoder_destroy)(self.state.as_ptr()) };
    }
}

/// An Opus decoder at [`SAMPLE_RATE`], for reading takes back.
pub struct OpusDecoder {
    api: Api,
    state: NonNull<ffi::Decoder>,
    channels: u8,
}

// SAFETY: as for the encoder: one thread at a time through `&mut self`.
unsafe impl Send for OpusDecoder {}

impl std::fmt::Debug for OpusDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpusDecoder").field("channels", &self.channels).finish_non_exhaustive()
    }
}

impl OpusDecoder {
    /// A decoder for `channels` channels.
    ///
    /// # Errors
    /// [`StreamError::Unavailable`] when libopus does not load; [`StreamError::Config`] for a channel count out of
    /// range; [`StreamError::Encode`] when libopus fails to start.
    pub fn new(channels: u8) -> Result<Self, StreamError> {
        check_channels(channels)?;
        let api = load()?.api;
        let rate = i32::try_from(SAMPLE_RATE).map_err(|_| StreamError::Config("sample rate out of range".into()))?;
        let mut err: c_int = 0;
        // SAFETY: the rate and channel count are values `opus.h` names; `err` is a valid out pointer.
        let state = unsafe { (api.decoder_create)(rate, c_int::from(channels), &raw mut err) };
        match NonNull::new(state) {
            Some(state) if err == ffi::OK => Ok(Self { api, state, channels }),
            Some(state) => {
                // SAFETY: libopus returned a state it allocated; it is freed once and never used again.
                unsafe { (api.decoder_destroy)(state.as_ptr()) };
                Err(opus_error(&api, "decoder", err))
            }
            None => Err(opus_error(&api, "decoder", err)),
        }
    }

    /// Decodes one packet and appends its interleaved samples to `out`. Returns the samples per channel it added.
    ///
    /// # Errors
    /// [`StreamError::Encode`] when the packet does not decode.
    pub fn decode(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, StreamError> {
        let channels = usize::from(self.channels);
        let len = i32::try_from(packet.len()).map_err(|_| StreamError::Encode("Opus packet too large".into()))?;
        let room = c_int::try_from(MAX_DECODE_SAMPLES).map_err(|_| StreamError::Encode("buffer too long".into()))?;
        let start = out.len();
        out.resize(start + MAX_DECODE_SAMPLES * channels, 0.0);
        // SAFETY: `packet` is `len` readable bytes and `out[start..]` holds `MAX_DECODE_SAMPLES` samples for each
        // channel, the most one packet decodes to; both live across the call.
        let decoded = unsafe {
            (self.api.decode_float)(self.state.as_ptr(), packet.as_ptr(), len, out[start..].as_mut_ptr(), room, 0)
        };
        let Ok(samples) = usize::try_from(decoded) else {
            out.truncate(start);
            return Err(opus_error(&self.api, "decode", decoded));
        };
        out.truncate(start + samples * channels);
        Ok(samples)
    }
}

impl Drop for OpusDecoder {
    fn drop(&mut self) {
        // SAFETY: the state was made in `new` and is destroyed once.
        unsafe { (self.api.decoder_destroy)(self.state.as_ptr()) };
    }
}

/// The samples per channel at [`SAMPLE_RATE`] one Opus packet decodes to, read from its table-of-contents byte
/// (RFC 6716 §3.1), or `None` for a packet too short or malformed to say.
#[must_use]
pub fn packet_samples(packet: &[u8]) -> Option<u32> {
    let toc = *packet.first()?;
    let config = u32::from(toc >> 3);
    // Frame length in 48 kHz samples: SILK 10/20/40/60 ms, hybrid 10/20 ms, CELT 2.5/5/10/20 ms.
    let frame = match config {
        0..=11 => [480, 960, 1920, 2880][(config % 4) as usize],
        12..=15 => [480, 960][(config % 2) as usize],
        _ => [120, 240, 480, 960][(config % 4) as usize],
    };
    let frames = match toc & 0x03 {
        0 => 1,
        1 | 2 => 2,
        _ => u32::from(*packet.get(1)? & 0x3F),
    };
    let samples = frame * frames;
    // A packet holds at most 120 ms (RFC 6716 §3.2.5).
    (frames > 0 && samples <= 5760).then_some(samples)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packet_lengths_come_from_the_toc() {
        // CELT fullband 20 ms (config 31), one frame.
        assert_eq!(packet_samples(&[31 << 3, 0]), Some(960));
        // SILK wideband 60 ms (config 11), two frames.
        assert_eq!(packet_samples(&[(11 << 3) | 1, 0]), Some(5760));
        // CELT 2.5 ms (config 16), code 3 with 4 frames.
        assert_eq!(packet_samples(&[(16 << 3) | 3, 4]), Some(480));
        // Hybrid 10 ms (config 12), code 3 with 0 frames: malformed.
        assert_eq!(packet_samples(&[(12 << 3) | 3, 0]), None);
        assert_eq!(packet_samples(&[]), None);
        // 60 ms × 3 frames is past the 120 ms limit.
        assert_eq!(packet_samples(&[(3 << 3) | 3, 3]), None);
    }

    #[test]
    fn settings_out_of_range_are_refused_before_loading() {
        let refused = |channels, bitrate| {
            matches!(OpusEncoder::new(OpusSettings { channels, bitrate }), Err(StreamError::Config(_)))
        };
        assert!(refused(0, 64_000));
        assert!(refused(3, 64_000));
        assert!(refused(1, MIN_BITRATE - 1));
        assert!(refused(2, MAX_BITRATE + 1));
        assert!(matches!(OpusDecoder::new(0), Err(StreamError::Config(_))));
    }

    #[test]
    fn chunks_of_any_length_make_whole_packets_and_flush_covers_the_lookahead() {
        if let Err(e) = available() {
            eprintln!("skipped: {e}");
            return;
        }
        let mut opus =
            OpusEncoder::new(OpusSettings { channels: 2, bitrate: 96_000 }).unwrap_or_else(|e| panic!("{e}"));
        let pre_skip = u64::from(opus.pre_skip());
        assert!(pre_skip > 0 && pre_skip < FRAME_SAMPLES as u64, "{pre_skip}");
        assert!(opus.encode(&[0.0; 3], &mut Vec::new()).is_err(), "half a stereo sample");
        let mut packets = Vec::new();
        let mut fed = 0usize;
        for chunk in [1usize, 959, 1000, 3, 2000, 517] {
            let pcm: Vec<f32> = (0..chunk * 2)
                .map(|i| (f32::from(u16::try_from(fed * 2 + i).unwrap_or(0)) * 0.01).sin() * 0.3)
                .collect();
            opus.encode(&pcm, &mut packets).unwrap_or_else(|e| panic!("{e}"));
            fed += chunk;
            assert_eq!(packets.len(), fed / FRAME_SAMPLES, "after {fed} samples");
        }
        opus.flush(&mut packets).unwrap_or_else(|e| panic!("{e}"));
        let total = packets.len() as u64 * FRAME_SAMPLES as u64;
        assert!(total >= fed as u64 + pre_skip && total < fed as u64 + pre_skip + FRAME_SAMPLES as u64);
        for (k, p) in packets.iter().enumerate() {
            assert_eq!(p.sample, (k * FRAME_SAMPLES) as u64);
            assert_eq!(packet_samples(&p.data), Some(960));
        }
        let mut decoder = OpusDecoder::new(2).unwrap_or_else(|e| panic!("{e}"));
        let mut pcm = Vec::new();
        for p in &packets {
            assert_eq!(decoder.decode(&p.data, &mut pcm).unwrap_or_else(|e| panic!("{e}")), FRAME_SAMPLES);
        }
        assert_eq!(pcm.len() as u64, total * 2);
    }
}
