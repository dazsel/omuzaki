//! A `WebM` (Matroska) writer for one VP9 video track and, optionally, one Opus audio track. Alpha is carried the way
//! `FFmpeg`, Chrome and OBS read it: `AlphaMode` 1 on the track and each frame's alpha packet in `BlockAdditional` id 1
//! (`docs/design/record.md` D6). The audio track follows the Matroska Opus mapping: an `OpusHead` as `CodecPrivate`,
//! the encoder's pre-skip as `CodecDelay` and 80 ms of `SeekPreRoll`.
//!
//! Crash-safe by construction: the segment starts with an unknown size and each cluster (one keyframe interval) is
//! written whole as it closes, so a file cut off by a crash plays up to its last cluster. With an audio track a closed
//! interval waits until the audio covering it has arrived (at most [`MAX_AUDIO_WAIT_MS`] of video), so its cluster holds
//! the video and the audio of the same span, every block in timestamp order. [`WebmSink::finish`] then writes the cues,
//! fills the seek head and the duration into space reserved for them, and sets the segment's size. Timestamps are
//! milliseconds; a frame's is its index on the take's frame grid, so a frame the encoder never saw shows as the previous
//! one held longer, and audio stays in sync. An audio packet's is its sample position at 48 kHz.
//!
//! [`read`] parses what this writer writes, for checks and for reading a take back.
#![allow(
    clippy::disallowed_methods,
    reason = "the sink writes one file inside the take folder the recorder chose, under the Machine setting `record.dir`"
)]

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use omuzaki_stream_api::error::StreamError;

const EBML: u32 = 0x1A45_DFA3;
const EBML_VERSION: u32 = 0x4286;
const EBML_READ_VERSION: u32 = 0x42F7;
const EBML_MAX_ID_LENGTH: u32 = 0x42F2;
const EBML_MAX_SIZE_LENGTH: u32 = 0x42F3;
const DOC_TYPE: u32 = 0x4282;
const DOC_TYPE_VERSION: u32 = 0x4287;
const DOC_TYPE_READ_VERSION: u32 = 0x4285;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
const VOID: u32 = 0xEC;
const INFO: u32 = 0x1549_A966;
const TIMESTAMP_SCALE: u32 = 0x2A_D7B1;
const DURATION: u32 = 0x4489;
const MUXING_APP: u32 = 0x4D80;
const WRITING_APP: u32 = 0x5741;
const TRACKS: u32 = 0x1654_AE6B;
const TRACK_ENTRY: u32 = 0xAE;
const TRACK_NUMBER: u32 = 0xD7;
const TRACK_UID: u32 = 0x73C5;
const TRACK_TYPE: u32 = 0x83;
const FLAG_LACING: u32 = 0x9C;
const CODEC_ID: u32 = 0x86;
const DEFAULT_DURATION: u32 = 0x23_E383;
const MAX_BLOCK_ADDITION_ID: u32 = 0x55EE;
const VIDEO: u32 = 0xE0;
const PIXEL_WIDTH: u32 = 0xB0;
const PIXEL_HEIGHT: u32 = 0xBA;
const ALPHA_MODE: u32 = 0x53C0;
const COLOUR: u32 = 0x55B0;
const MATRIX_COEFFICIENTS: u32 = 0x55B1;
const RANGE: u32 = 0x55B9;
const TRANSFER_CHARACTERISTICS: u32 = 0x55BA;
const PRIMARIES: u32 = 0x55BB;
const CLUSTER: u32 = 0x1F43_B675;
const TIMESTAMP: u32 = 0xE7;
const SIMPLE_BLOCK: u32 = 0xA3;
const BLOCK_GROUP: u32 = 0xA0;
const BLOCK: u32 = 0xA1;
const REFERENCE_BLOCK: u32 = 0xFB;
const BLOCK_ADDITIONS: u32 = 0x75A1;
const BLOCK_MORE: u32 = 0xA6;
const BLOCK_ADD_ID: u32 = 0xEE;
const BLOCK_ADDITIONAL: u32 = 0xA5;
const CUES: u32 = 0x1C53_BB6B;
const CUE_POINT: u32 = 0xBB;
const CUE_TIME: u32 = 0xB3;
const CUE_TRACK_POSITIONS: u32 = 0xB7;
const CUE_TRACK: u32 = 0xF7;
const CUE_CLUSTER_POSITION: u32 = 0xF1;
const CODEC_PRIVATE: u32 = 0x63A2;
const CODEC_DELAY: u32 = 0x56AA;
const SEEK_PRE_ROLL: u32 = 0x56BB;
const AUDIO: u32 = 0xE1;
const SAMPLING_FREQUENCY: u32 = 0xB5;
const CHANNELS: u32 = 0x9F;

/// Bytes reserved after the segment's start for the seek head (68 bytes with three entries).
const SEEK_HEAD_RESERVE: usize = 100;
/// Bytes reserved in the info for the duration: a 2-byte id, a 1-byte size and an 8-byte float.
const DURATION_RESERVE: usize = 11;
/// The longest a cluster may run, in milliseconds, before the writer must start another (block times are 16-bit).
pub const MAX_CLUSTER_MS: i64 = 30_000;
/// What the files say wrote them.
const APP: &str = concat!("Omuzaki ", env!("CARGO_PKG_VERSION"));

fn put_id(out: &mut Vec<u8>, id: u32) {
    let bytes = id.to_be_bytes();
    let skip = bytes.iter().take_while(|b| **b == 0).count().min(3);
    out.extend_from_slice(&bytes[skip..]);
}

fn put_size(out: &mut Vec<u8>, size: u64) {
    let mut len = 1;
    while len < 8 && size >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    let marked = size | (1u64 << (7 * len));
    out.extend_from_slice(&marked.to_be_bytes()[8 - len..]);
}

fn put_uint(out: &mut Vec<u8>, id: u32, v: u64) {
    let bytes = v.to_be_bytes();
    let skip = bytes.iter().take_while(|b| **b == 0).count().min(7);
    put_id(out, id);
    put_size(out, (8 - skip) as u64);
    out.extend_from_slice(&bytes[skip..]);
}

fn put_uint_fixed(out: &mut Vec<u8>, id: u32, v: u64) {
    put_id(out, id);
    put_size(out, 8);
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_int(out: &mut Vec<u8>, id: u32, v: i64) {
    let bytes = v.to_be_bytes();
    let mut skip = 0;
    // Drop leading bytes that only repeat the sign.
    while skip < 7 && ((bytes[skip] == 0 && bytes[skip + 1] < 0x80) || (bytes[skip] == 0xFF && bytes[skip + 1] >= 0x80))
    {
        skip += 1;
    }
    put_id(out, id);
    put_size(out, (8 - skip) as u64);
    out.extend_from_slice(&bytes[skip..]);
}

fn put_float(out: &mut Vec<u8>, id: u32, v: f64) {
    put_id(out, id);
    put_size(out, 8);
    out.extend_from_slice(&v.to_be_bytes());
}

fn put_bytes(out: &mut Vec<u8>, id: u32, v: &[u8]) {
    put_id(out, id);
    put_size(out, v.len() as u64);
    out.extend_from_slice(v);
}

fn put_master(out: &mut Vec<u8>, id: u32, body: impl FnOnce(&mut Vec<u8>)) {
    let mut inner = Vec::new();
    body(&mut inner);
    put_bytes(out, id, &inner);
}

/// A Void element of exactly `len` bytes (at least 2).
fn put_void(out: &mut Vec<u8>, len: usize) {
    debug_assert!(len >= 2, "a Void needs an id and a size");
    put_id(out, VOID);
    // A one-byte size covers up to 126 bytes of content, which every reserve here fits.
    let content = len.saturating_sub(2);
    put_size(out, content as u64);
    out.resize(out.len() + content, 0);
}

/// The video track a [`WebmSink`] writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebmTrack {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Frames per second as numerator and denominator.
    pub fps: (u32, u32),
    /// Whether frames carry an alpha packet.
    pub alpha: bool,
}

/// The Opus audio track a [`WebmSink`] can write beside the video, at 48 kHz.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebmAudio {
    /// Channels: 1 or 2 (Opus channel mapping family 0).
    pub channels: u8,
    /// The samples at 48 kHz a decoder drops from the start: the encoder's lookahead
    /// ([`OpusEncoder::pre_skip`](crate::opus::OpusEncoder::pre_skip)).
    pub pre_skip: u16,
}

/// The track number of the video.
pub const VIDEO_TRACK: u64 = 1;
/// The track number of the audio, when there is one.
pub const AUDIO_TRACK: u64 = 2;
/// The audio's sample rate in Hz: Opus's own.
const AUDIO_RATE: u64 = 48_000;
/// The audio a player decodes before a seek point so the Opus decoder has settled: 80 ms, as the Matroska Opus
/// mapping asks, in nanoseconds.
pub const SEEK_PRE_ROLL_NS: u64 = 80_000_000;
/// The most video, in milliseconds past the end of a closed keyframe interval, the writer holds while it waits for the
/// audio of that interval. Past it the interval is written without that audio, which then goes in the next cluster
/// (still in order within it), so a stalled audio source never holds a take's video in memory.
pub const MAX_AUDIO_WAIT_MS: i64 = 5_000;

/// The Opus identification header (RFC 7845 §5.1) for `audio`: the track's `CodecPrivate`.
#[must_use]
pub fn opus_head(audio: WebmAudio) -> [u8; 19] {
    let mut head = [0u8; 19];
    head[..8].copy_from_slice(b"OpusHead");
    head[8] = 1; // version
    head[9] = audio.channels;
    head[10..12].copy_from_slice(&audio.pre_skip.to_le_bytes());
    head[12..16].copy_from_slice(&48_000u32.to_le_bytes()); // the input's sample rate, for information only
    // Output gain 0 dB (bytes 16 and 17) and channel mapping family 0 (byte 18) are zero.
    head
}

/// One video frame waiting for its cluster.
#[derive(Debug)]
struct VideoBlock {
    ts: i64,
    key: bool,
    /// The previous frame's timestamp, which a delta frame in a block group references.
    previous: Option<i64>,
    data: Vec<u8>,
    alpha: Option<Vec<u8>>,
}

/// One audio packet waiting for its cluster.
#[derive(Debug)]
struct AudioBlock {
    ts: i64,
    data: Vec<u8>,
}

/// One keyframe interval of video: the frames of one cluster.
#[derive(Debug)]
struct Group {
    start: i64,
    key: bool,
    frames: Vec<VideoBlock>,
}

/// The `WebM` writer.
#[derive(Debug)]
pub struct WebmSink {
    path: PathBuf,
    file: File,
    track: WebmTrack,
    audio: Option<WebmAudio>,
    segment_size_at: u64,
    segment_start: u64,
    seek_head_at: u64,
    duration_at: u64,
    info_at: u64,
    tracks_at: u64,
    /// The interval receiving frames.
    open: Option<Group>,
    /// Closed intervals waiting for their audio.
    closed: VecDeque<Group>,
    /// Audio packets not yet in a cluster, in order.
    queued_audio: VecDeque<AudioBlock>,
    /// The sample position just past the last audio packet.
    audio_next: Option<u64>,
    /// The cluster being assembled: its body, timestamp and position in the segment.
    cluster: Vec<u8>,
    cluster_ts: Option<i64>,
    cluster_at: u64,
    previous_ts: Option<i64>,
    cues: Vec<(i64, u64)>,
    written: u64,
}

fn io(path: &Path, e: &std::io::Error) -> StreamError {
    StreamError::Io(format!("{}: {e}", path.display()))
}

/// The `Tracks` element: the VP9 video and, when given, the Opus audio.
fn put_tracks(out: &mut Vec<u8>, track: WebmTrack, audio: Option<WebmAudio>) {
    let frame_ns = u64::from(track.fps.1) * 1_000_000_000 / u64::from(track.fps.0);
    put_master(out, TRACKS, |b| {
        put_master(b, TRACK_ENTRY, |t| {
            put_uint(t, TRACK_NUMBER, VIDEO_TRACK);
            put_uint(t, TRACK_UID, VIDEO_TRACK);
            put_uint(t, TRACK_TYPE, 1);
            put_uint(t, FLAG_LACING, 0);
            put_bytes(t, CODEC_ID, b"V_VP9");
            put_uint(t, DEFAULT_DURATION, frame_ns);
            if track.alpha {
                put_uint(t, MAX_BLOCK_ADDITION_ID, 1);
            }
            put_master(t, VIDEO, |v| {
                put_uint(v, PIXEL_WIDTH, u64::from(track.width));
                put_uint(v, PIXEL_HEIGHT, u64::from(track.height));
                if track.alpha {
                    put_uint(v, ALPHA_MODE, 1);
                }
                put_master(v, COLOUR, |c| {
                    // BT.709 matrix, studio range, sRGB transfer, BT.709 primaries: what `i420` writes.
                    put_uint(c, MATRIX_COEFFICIENTS, 1);
                    put_uint(c, RANGE, 1);
                    put_uint(c, TRANSFER_CHARACTERISTICS, 13);
                    put_uint(c, PRIMARIES, 1);
                });
            });
        });
        if let Some(audio) = audio {
            put_master(b, TRACK_ENTRY, |t| {
                put_uint(t, TRACK_NUMBER, AUDIO_TRACK);
                put_uint(t, TRACK_UID, AUDIO_TRACK);
                put_uint(t, TRACK_TYPE, 2);
                put_uint(t, FLAG_LACING, 0);
                put_bytes(t, CODEC_ID, b"A_OPUS");
                put_bytes(t, CODEC_PRIVATE, &opus_head(audio));
                put_uint(t, CODEC_DELAY, u64::from(audio.pre_skip) * 1_000_000_000 / AUDIO_RATE);
                put_uint(t, SEEK_PRE_ROLL, SEEK_PRE_ROLL_NS);
                put_master(t, AUDIO, |a| {
                    #[allow(clippy::cast_precision_loss)] // 48000 is exact in an f64.
                    put_float(a, SAMPLING_FREQUENCY, AUDIO_RATE as f64);
                    put_uint(a, CHANNELS, u64::from(audio.channels));
                });
            });
        }
    });
}

/// Refuses tracks the writer can't describe, before any file is made.
fn check(track: WebmTrack, audio: Option<WebmAudio>) -> Result<(), StreamError> {
    if track.width == 0 || track.height == 0 || track.fps.0 == 0 || track.fps.1 == 0 {
        return Err(StreamError::Config("a WebM track needs a size and a frame rate above zero".into()));
    }
    if let Some(a) = audio
        && !(1..=crate::opus::MAX_CHANNELS).contains(&a.channels)
    {
        return Err(StreamError::Config(format!(
            "a WebM Opus track takes 1 to {} channels; got {}",
            crate::opus::MAX_CHANNELS,
            a.channels
        )));
    }
    Ok(())
}

/// Milliseconds at sample position `sample` of 48 kHz audio, rounded to the nearest.
fn sample_ms(sample: u64) -> i64 {
    i64::try_from((u128::from(sample) * 1000 + u128::from(AUDIO_RATE / 2)) / u128::from(AUDIO_RATE)).unwrap_or(i64::MAX)
}

impl WebmSink {
    /// Creates `path` (refusing to replace a file) and writes the header for the video `track` alone.
    ///
    /// # Errors
    /// [`StreamError::Io`] when the file can't be created or written; [`StreamError::Config`] for a zero size or rate.
    pub fn create(path: &Path, track: WebmTrack) -> Result<Self, StreamError> {
        Self::create_with_audio(path, track, None)
    }

    /// Creates `path` (refusing to replace a file) and writes the header for the video `track` and, when `audio` is
    /// given, an Opus track beside it.
    ///
    /// # Errors
    /// [`StreamError::Io`] when the file can't be created or written; [`StreamError::Config`] for a zero size or rate
    /// or an audio channel count other than 1 or 2.
    pub fn create_with_audio(path: &Path, track: WebmTrack, audio: Option<WebmAudio>) -> Result<Self, StreamError> {
        check(track, audio)?;
        let file = File::options().write(true).create_new(true).open(path).map_err(|e| io(path, &e))?;
        Self::from_file(file, path, track, audio)
    }

    /// Writes the header for the video `track` and, when `audio` is given, an Opus track beside it into `file`, which
    /// is open for writing and seeking and empty, such as a project sandbox's replacement file that is committed by
    /// rename once [`finish`](Self::finish) returns. `name` is the path errors show; the sink never opens it.
    ///
    /// # Errors
    /// [`StreamError::Io`] when the file can't be read, sought or written; [`StreamError::Config`] for a file that is
    /// not empty, a zero size or rate, or an audio channel count other than 1 or 2.
    pub fn from_file(
        mut file: File,
        name: &Path,
        track: WebmTrack,
        audio: Option<WebmAudio>,
    ) -> Result<Self, StreamError> {
        let path = name;
        check(track, audio)?;
        // The writer seeks back to absolute offsets in `finish`, so the file must start empty at offset zero.
        if file.metadata().map_err(|e| io(path, &e))?.len() != 0 {
            return Err(StreamError::Config(format!("{}: a WebM sink takes an empty file", path.display())));
        }
        file.seek(SeekFrom::Start(0)).map_err(|e| io(path, &e))?;
        let mut head = Vec::with_capacity(512);
        put_master(&mut head, EBML, |b| {
            put_uint(b, EBML_VERSION, 1);
            put_uint(b, EBML_READ_VERSION, 1);
            put_uint(b, EBML_MAX_ID_LENGTH, 4);
            put_uint(b, EBML_MAX_SIZE_LENGTH, 8);
            put_bytes(b, DOC_TYPE, b"webm");
            put_uint(b, DOC_TYPE_VERSION, 4);
            put_uint(b, DOC_TYPE_READ_VERSION, 2);
        });
        put_id(&mut head, SEGMENT);
        let segment_size_at = head.len() as u64;
        // An unknown size until `finish` sets it: a reader takes the segment to the end of the file.
        head.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
        let segment_start = head.len() as u64;
        let seek_head_at = segment_start;
        put_void(&mut head, SEEK_HEAD_RESERVE);

        let info_at = head.len() as u64 - segment_start;
        let mut info = Vec::new();
        put_uint(&mut info, TIMESTAMP_SCALE, 1_000_000);
        put_bytes(&mut info, MUXING_APP, APP.as_bytes());
        put_bytes(&mut info, WRITING_APP, APP.as_bytes());
        let duration_in_info = info.len();
        put_void(&mut info, DURATION_RESERVE);
        put_id(&mut head, INFO);
        put_size(&mut head, info.len() as u64);
        let duration_at = head.len() as u64 + duration_in_info as u64;
        head.extend_from_slice(&info);

        let tracks_at = head.len() as u64 - segment_start;
        put_tracks(&mut head, track, audio);
        let mut sink = Self {
            path: path.to_owned(),
            file,
            track,
            audio,
            segment_size_at,
            segment_start,
            seek_head_at,
            duration_at,
            info_at,
            tracks_at,
            open: None,
            closed: VecDeque::new(),
            queued_audio: VecDeque::new(),
            audio_next: None,
            cluster: Vec::new(),
            cluster_ts: None,
            cluster_at: 0,
            previous_ts: None,
            cues: Vec::new(),
            written: 0,
        };
        sink.write_all(&head)?;
        Ok(sink)
    }

    /// The file's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The audio track, when the file has one.
    #[must_use]
    pub fn audio(&self) -> Option<WebmAudio> {
        self.audio
    }

    /// The timestamp, in milliseconds, of frame `index`.
    #[must_use]
    pub fn timestamp_ms(&self, index: u64) -> i64 {
        let (num, den) = (u128::from(self.track.fps.0), u128::from(self.track.fps.1));
        i64::try_from((u128::from(index) * 1000 * den + num / 2) / num).unwrap_or(i64::MAX)
    }

    /// Whether frame `index` must be a keyframe for its cluster to stay within [`MAX_CLUSTER_MS`].
    #[must_use]
    pub fn needs_key(&self, index: u64) -> bool {
        self.open.as_ref().is_none_or(|group| self.timestamp_ms(index) - group.start > MAX_CLUSTER_MS)
    }

    fn write_all(&mut self, bytes: &[u8]) -> Result<(), StreamError> {
        self.file.write_all(bytes).map_err(|e| io(&self.path, &e))?;
        self.written += bytes.len() as u64;
        Ok(())
    }

    /// Writes frame `index`: a VP9 frame, a keyframe when `key`, with its alpha frame when the track has alpha.
    /// Frames come in index order; a keyframe starts a new cluster.
    ///
    /// # Errors
    /// [`StreamError::Mux`] for a frame out of order or a missing alpha packet; [`StreamError::Io`] when the disk
    /// write fails.
    pub fn write_frame(&mut self, index: u64, key: bool, data: &[u8], alpha: Option<&[u8]>) -> Result<(), StreamError> {
        let ts = self.timestamp_ms(index);
        if self.previous_ts.is_some_and(|p| ts <= p) {
            return Err(StreamError::Mux(format!("frame {index} comes after a later one")));
        }
        if self.track.alpha != alpha.is_some() {
            return Err(StreamError::Mux("every frame of an alpha track carries alpha, and only those".into()));
        }
        if key || self.open.is_none() {
            if let Some(group) = self.open.take() {
                self.closed.push_back(group);
            }
            self.open = Some(Group { start: ts, key, frames: Vec::new() });
        }
        let block =
            VideoBlock { ts, key, previous: self.previous_ts, data: data.to_vec(), alpha: alpha.map(<[u8]>::to_vec) };
        if let Some(group) = self.open.as_mut() {
            group.frames.push(block);
        }
        self.previous_ts = Some(ts);
        self.drain(false)
    }

    /// Writes one Opus packet whose first sample is at `sample` (48 kHz samples per channel from the take's start,
    /// as [`OpusPacket::sample`](crate::opus::OpusPacket::sample) gives it). Packets come in order; the writer places
    /// each in the cluster that covers its time, interleaved with the video.
    ///
    /// # Errors
    /// [`StreamError::Mux`] when the file has no audio track, the bytes are not an Opus packet, or the packet starts
    /// before the previous one ends; [`StreamError::Io`] when the disk write fails.
    pub fn write_audio(&mut self, sample: u64, data: &[u8]) -> Result<(), StreamError> {
        if self.audio.is_none() {
            return Err(StreamError::Mux("this WebM was made without an audio track".into()));
        }
        let samples = crate::opus::packet_samples(data)
            .ok_or_else(|| StreamError::Mux(format!("the audio at sample {sample} is not an Opus packet")))?;
        if let Some(next) = self.audio_next
            && sample < next
        {
            return Err(StreamError::Mux(format!(
                "the audio packet at sample {sample} starts before the previous one ends ({next})"
            )));
        }
        self.queued_audio.push_back(AudioBlock { ts: sample_ms(sample), data: data.to_vec() });
        self.audio_next = Some(sample + u64::from(samples));
        self.drain(false)
    }

    /// Where the audio that has arrived reaches, in milliseconds.
    fn audio_end_ms(&self) -> Option<i64> {
        self.audio_next.map(sample_ms)
    }

    /// Writes every closed interval whose audio has arrived (all of them when `finishing`, or with no audio track).
    fn drain(&mut self, finishing: bool) -> Result<(), StreamError> {
        while let Some(front) = self.closed.front() {
            let end = self.closed.get(1).or(self.open.as_ref()).map_or(i64::MAX, |g| g.start);
            let waited_enough = self.previous_ts.is_some_and(|latest| latest.saturating_sub(end) >= MAX_AUDIO_WAIT_MS);
            let ready = finishing
                || self.audio.is_none()
                || self.audio_end_ms().is_some_and(|reach| reach >= end)
                || waited_enough;
            if !ready {
                break;
            }
            debug_assert!(front.start <= end, "intervals are in order");
            let Some(group) = self.closed.pop_front() else { break };
            self.write_group(Some(group), end)?;
        }
        Ok(())
    }

    /// Writes `group`'s frames and every queued audio packet before `end` as one cluster (more, should the audio
    /// before the group stretch a cluster past its 16-bit block times), blocks in timestamp order, video first at a
    /// tie so a keyframe opens its cluster.
    fn write_group(&mut self, group: Option<Group>, end: i64) -> Result<(), StreamError> {
        let (key, frames) = group.map_or((false, Vec::new()), |g| (g.key, g.frames));
        let mut frames = frames.into_iter().peekable();
        let mut first_frame = true;
        loop {
            let audio_next = self.queued_audio.front().map(|a| a.ts).filter(|ts| *ts < end);
            let video_next = frames.peek().map(|f| f.ts);
            let take_video = match (video_next, audio_next) {
                (None, None) => break,
                (Some(v), Some(a)) => v <= a,
                (Some(_), None) => true,
                (None, Some(_)) => false,
            };
            if take_video {
                let Some(frame) = frames.next() else { break };
                let relative = self.place(frame.ts)?;
                if first_frame && key {
                    self.cues.push((frame.ts, self.cluster_at));
                }
                first_frame = false;
                self.put_video(relative, &frame);
            } else {
                let Some(packet) = self.queued_audio.pop_front() else { break };
                let relative = self.place(packet.ts)?;
                put_id(&mut self.cluster, SIMPLE_BLOCK);
                put_size(&mut self.cluster, (4 + packet.data.len()) as u64);
                // Track 2, the relative time, and the keyframe flag: every Opus packet decodes on its own.
                self.cluster.push(0x82);
                self.cluster.extend_from_slice(&relative.to_be_bytes());
                self.cluster.push(0x80);
                self.cluster.extend_from_slice(&packet.data);
            }
        }
        self.close_cluster()
    }

    /// Makes room for a block at `ts` in the cluster being assembled, starting one when there is none or `ts` is out
    /// of its 16-bit reach, and returns the block's time relative to it.
    fn place(&mut self, ts: i64) -> Result<i16, StreamError> {
        let start = match self.cluster_ts {
            Some(start) if ts >= start && i16::try_from(ts - start).is_ok() => start,
            _ => {
                self.close_cluster()?;
                self.cluster.clear();
                self.cluster_ts = Some(ts);
                self.cluster_at = self.written - self.segment_start;
                put_uint(&mut self.cluster, TIMESTAMP, u64::try_from(ts).unwrap_or(0));
                ts
            }
        };
        i16::try_from(ts - start).map_err(|_| StreamError::Mux(format!("a block at {ts} ms is out of its cluster")))
    }

    fn put_video(&mut self, relative: i16, frame: &VideoBlock) {
        let mut header = [0u8; 4];
        header[0] = 0x81; // track 1
        header[1..3].copy_from_slice(&relative.to_be_bytes());
        match &frame.alpha {
            None => {
                header[3] = if frame.key { 0x80 } else { 0x00 };
                put_id(&mut self.cluster, SIMPLE_BLOCK);
                put_size(&mut self.cluster, (header.len() + frame.data.len()) as u64);
                self.cluster.extend_from_slice(&header);
                self.cluster.extend_from_slice(&frame.data);
            }
            Some(alpha) => {
                put_master(&mut self.cluster, BLOCK_GROUP, |g| {
                    put_id(g, BLOCK);
                    put_size(g, (header.len() + frame.data.len()) as u64);
                    g.extend_from_slice(&header);
                    g.extend_from_slice(&frame.data);
                    // A block group with no reference is a keyframe.
                    if let (false, Some(p)) = (frame.key, frame.previous) {
                        put_int(g, REFERENCE_BLOCK, p - frame.ts);
                    }
                    put_master(g, BLOCK_ADDITIONS, |a| {
                        put_master(a, BLOCK_MORE, |m| {
                            put_uint(m, BLOCK_ADD_ID, 1);
                            put_bytes(m, BLOCK_ADDITIONAL, alpha);
                        });
                    });
                });
            }
        }
    }

    fn close_cluster(&mut self) -> Result<(), StreamError> {
        if self.cluster_ts.take().is_none() {
            return Ok(());
        }
        let body = std::mem::take(&mut self.cluster);
        let mut head = Vec::with_capacity(12);
        put_id(&mut head, CLUSTER);
        put_size(&mut head, body.len() as u64);
        self.write_all(&head)?;
        self.write_all(&body)?;
        self.cluster = body;
        self.cluster.clear();
        Ok(())
    }

    /// Writes the last clusters and the cues, fills in the seek head and the duration (`frames` frames long, or the
    /// audio's length when that is longer), and sets the segment's size. Returns the file's size in bytes.
    ///
    /// # Errors
    /// [`StreamError::Io`] when a write or seek fails.
    pub fn finish(mut self, frames: u64) -> Result<u64, StreamError> {
        if let Some(group) = self.open.take() {
            self.closed.push_back(group);
        }
        self.drain(true)?;
        if !self.queued_audio.is_empty() {
            self.write_group(None, i64::MAX)?;
        }
        let cues_at = self.written - self.segment_start;
        let mut cues = Vec::new();
        put_master(&mut cues, CUES, |c| {
            for (ts, position) in &self.cues {
                put_master(c, CUE_POINT, |p| {
                    put_uint(p, CUE_TIME, u64::try_from(*ts).unwrap_or(0));
                    put_master(p, CUE_TRACK_POSITIONS, |t| {
                        put_uint(t, CUE_TRACK, VIDEO_TRACK);
                        put_uint(t, CUE_CLUSTER_POSITION, *position);
                    });
                });
            }
        });
        if !self.cues.is_empty() {
            self.write_all(&cues)?;
        }

        let mut seek_head = Vec::with_capacity(SEEK_HEAD_RESERVE);
        put_master(&mut seek_head, SEEK_HEAD, |s| {
            let mut entries = vec![(INFO, self.info_at), (TRACKS, self.tracks_at)];
            if !self.cues.is_empty() {
                entries.push((CUES, cues_at));
            }
            for (id, position) in entries {
                put_master(s, SEEK, |e| {
                    put_bytes(e, SEEK_ID, &id.to_be_bytes());
                    put_uint_fixed(e, SEEK_POSITION, position);
                });
            }
        });
        let rest = SEEK_HEAD_RESERVE - seek_head.len();
        put_void(&mut seek_head, rest);

        #[allow(clippy::cast_precision_loss)] // A duration in milliseconds: far below 2^52.
        let video_ms = (u128::from(frames) * 1000 * u128::from(self.track.fps.1)) as f64 / f64::from(self.track.fps.0);
        // The audio plays from its first packet's time to its last packet's end, less the pre-skip a decoder drops.
        #[allow(clippy::cast_precision_loss)] // As above.
        let audio_ms = match (self.audio, self.audio_next) {
            (Some(audio), Some(next)) => {
                next.saturating_sub(u64::from(audio.pre_skip)) as f64 * 1000.0 / AUDIO_RATE as f64
            }
            _ => 0.0,
        };
        let mut duration = Vec::with_capacity(DURATION_RESERVE);
        put_float(&mut duration, DURATION, video_ms.max(audio_ms));

        let segment_size = self.written - self.segment_start;
        let mut size = [0u8; 8];
        size.copy_from_slice(&(segment_size | (1u64 << 56)).to_be_bytes());

        for (at, bytes) in
            [(self.seek_head_at, &seek_head[..]), (self.duration_at, &duration[..]), (self.segment_size_at, &size[..])]
        {
            self.file.seek(SeekFrom::Start(at)).map_err(|e| io(&self.path, &e))?;
            self.file.write_all(bytes).map_err(|e| io(&self.path, &e))?;
        }
        self.file.sync_all().map_err(|e| io(&self.path, &e))?;
        Ok(self.written)
    }
}

/// One frame read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebmFrame {
    /// Its timestamp in milliseconds.
    pub ts_ms: i64,
    /// Whether it is marked as a keyframe.
    pub key: bool,
    /// The codec frame.
    pub data: Vec<u8>,
    /// The alpha frame, if any.
    pub alpha: Option<Vec<u8>>,
}

/// The audio track read back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WebmAudioTrack {
    /// Its track number.
    pub number: u64,
    /// Its codec, `A_OPUS` for this writer's.
    pub codec: String,
    /// Its `CodecPrivate`: the `OpusHead` for Opus.
    pub codec_private: Vec<u8>,
    /// Its `CodecDelay` in nanoseconds.
    pub codec_delay_ns: u64,
    /// Its `SeekPreRoll` in nanoseconds.
    pub seek_pre_roll_ns: u64,
    /// Its sampling frequency in Hz.
    pub sampling_frequency: f64,
    /// Its channel count.
    pub channels: u64,
}

/// One audio packet read back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebmAudioBlock {
    /// Its timestamp in milliseconds.
    pub ts_ms: i64,
    /// The codec packet.
    pub data: Vec<u8>,
}

/// What [`read`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WebmInfo {
    /// The document type.
    pub doc_type: String,
    /// The video track's codec.
    pub codec: String,
    /// Width in pixels.
    pub width: u64,
    /// Height in pixels.
    pub height: u64,
    /// The video track's `AlphaMode`.
    pub alpha_mode: u64,
    /// The duration in milliseconds, once the file was finished.
    pub duration_ms: Option<f64>,
    /// Cue points.
    pub cues: usize,
    /// Whether the segment's size was set.
    pub sized: bool,
    /// The video frames, in order.
    pub frames: Vec<WebmFrame>,
    /// The audio track, when there is one.
    pub audio: Option<WebmAudioTrack>,
    /// The audio packets, in order.
    pub audio_blocks: Vec<WebmAudioBlock>,
    /// Each cluster's timestamp in milliseconds, in file order.
    pub clusters: Vec<i64>,
    /// Every block's track number and timestamp in milliseconds, in file order: how the tracks interleave.
    pub blocks: Vec<(u64, i64)>,
}

fn bad(what: &str) -> StreamError {
    StreamError::Mux(format!("not a WebM this writer reads: {what}"))
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn vint(&mut self, keep_marker: bool) -> Result<(u64, bool), StreamError> {
        let first = *self.bytes.get(self.at).ok_or_else(|| bad("cut short"))?;
        let len = first.leading_zeros() as usize + 1;
        if len > 8 {
            return Err(bad("an invalid length"));
        }
        let slice = self.bytes.get(self.at..self.at + len).ok_or_else(|| bad("cut short"))?;
        let mut v = if keep_marker { u64::from(first) } else { u64::from(first) & (0xFF >> len) };
        for b in &slice[1..] {
            v = (v << 8) | u64::from(*b);
        }
        let unknown = !keep_marker && v == (1u64 << (7 * len)) - 1;
        self.at += len;
        Ok((v, unknown))
    }
}

fn uint(body: &[u8]) -> u64 {
    body.iter().fold(0, |v, b| (v << 8) | u64::from(*b))
}

fn int(body: &[u8]) -> i64 {
    let fill = if body.first().is_some_and(|b| *b >= 0x80) { -1i64 } else { 0 };
    body.iter().fold(fill, |v, b| (v << 8) | i64::from(*b))
}

fn float(body: &[u8]) -> Result<f64, StreamError> {
    match body.len() {
        4 => Ok(f64::from(f32::from_be_bytes(body.try_into().map_err(|_| bad("a short float"))?))),
        8 => Ok(f64::from_be_bytes(body.try_into().map_err(|_| bad("a short float"))?)),
        _ => Err(bad("a float of neither 4 nor 8 bytes")),
    }
}

/// Parses a `WebM` file this module wrote (or any `WebM` with one VP9 track and at most one audio track, without
/// lacing).
///
/// # Errors
/// [`StreamError::Mux`] for bytes that are not such a file.
pub fn read(bytes: &[u8]) -> Result<WebmInfo, StreamError> {
    let mut info = WebmInfo::default();
    let mut state = State { video_track: VIDEO_TRACK, ..State::default() };
    walk(bytes, 0, bytes.len(), &mut info, &mut state)?;
    Ok(info)
}

/// One track entry as it is read.
#[derive(Default)]
struct TrackRead {
    number: u64,
    kind: u64,
    codec: String,
    width: u64,
    height: u64,
    alpha_mode: u64,
    codec_private: Vec<u8>,
    codec_delay_ns: u64,
    seek_pre_roll_ns: u64,
    sampling_frequency: f64,
    channels: u64,
}

#[derive(Default)]
struct State {
    cluster_ts: i64,
    track: TrackRead,
    video_track: u64,
    audio_track: Option<u64>,
    block: Option<(u64, i64, Vec<u8>)>,
    referenced: bool,
    alpha: Option<Vec<u8>>,
}

impl State {
    /// Files a finished track entry as the video or the audio track.
    fn file_track(&mut self, info: &mut WebmInfo) -> Result<(), StreamError> {
        let t = std::mem::take(&mut self.track);
        if t.kind != 2 {
            self.video_track = t.number;
            info.codec = t.codec;
            info.width = t.width;
            info.height = t.height;
            info.alpha_mode = t.alpha_mode;
            return Ok(());
        }
        if self.audio_track.is_some() {
            return Err(bad("a second audio track"));
        }
        self.audio_track = Some(t.number);
        info.audio = Some(WebmAudioTrack {
            number: t.number,
            codec: t.codec,
            codec_private: t.codec_private,
            codec_delay_ns: t.codec_delay_ns,
            seek_pre_roll_ns: t.seek_pre_roll_ns,
            sampling_frequency: t.sampling_frequency,
            channels: t.channels,
        });
        Ok(())
    }

    fn push(&self, info: &mut WebmInfo, track: u64, ts: i64, key: bool, data: Vec<u8>, alpha: Option<Vec<u8>>) {
        info.blocks.push((track, ts));
        if Some(track) == self.audio_track {
            info.audio_blocks.push(WebmAudioBlock { ts_ms: ts, data });
        } else {
            info.frames.push(WebmFrame { ts_ms: ts, key, data, alpha });
        }
    }
}

fn walk(bytes: &[u8], from: usize, to: usize, info: &mut WebmInfo, state: &mut State) -> Result<(), StreamError> {
    let mut r = Reader { bytes, at: from };
    while r.at < to {
        let (id, _) = r.vint(true)?;
        let (size, unknown) = r.vint(false)?;
        let start = r.at;
        let end = if unknown {
            to
        } else {
            start
                .checked_add(usize::try_from(size).map_err(|_| bad("a huge element"))?)
                .ok_or_else(|| bad("a huge element"))?
        };
        if end > to {
            return Err(bad("an element runs past its parent"));
        }
        let body = &bytes[start..end];
        let id = u32::try_from(id).map_err(|_| bad("a long id"))?;
        match id {
            EBML | SEGMENT | TRACKS | VIDEO | AUDIO | BLOCK_ADDITIONS | BLOCK_MORE | CUES | INFO => {
                if id == SEGMENT {
                    info.sized = !unknown;
                }
                walk(bytes, start, end, info, state)?;
            }
            TRACK_ENTRY => {
                state.track = TrackRead { number: VIDEO_TRACK, kind: 1, ..TrackRead::default() };
                walk(bytes, start, end, info, state)?;
                state.file_track(info)?;
            }
            CLUSTER => {
                info.clusters.push(-1);
                walk(bytes, start, end, info, state)?;
            }
            CUE_POINT => info.cues += 1,
            DOC_TYPE => info.doc_type = String::from_utf8_lossy(body).into_owned(),
            TRACK_NUMBER => state.track.number = uint(body),
            TRACK_TYPE => state.track.kind = uint(body),
            CODEC_ID => state.track.codec = String::from_utf8_lossy(body).into_owned(),
            CODEC_PRIVATE => state.track.codec_private = body.to_vec(),
            CODEC_DELAY => state.track.codec_delay_ns = uint(body),
            SEEK_PRE_ROLL => state.track.seek_pre_roll_ns = uint(body),
            SAMPLING_FREQUENCY => state.track.sampling_frequency = float(body)?,
            CHANNELS => state.track.channels = uint(body),
            PIXEL_WIDTH => state.track.width = uint(body),
            PIXEL_HEIGHT => state.track.height = uint(body),
            ALPHA_MODE => state.track.alpha_mode = uint(body),
            DURATION => info.duration_ms = Some(float(body)?),
            TIMESTAMP => {
                state.cluster_ts = i64::try_from(uint(body)).unwrap_or(0);
                if let Some(last) = info.clusters.last_mut() {
                    *last = state.cluster_ts;
                }
            }
            SIMPLE_BLOCK => {
                let (track, ts, flags, data) = block(body, state)?;
                state.push(info, track, ts, flags & 0x80 != 0, data, None);
            }
            BLOCK_GROUP => {
                state.block = None;
                state.referenced = false;
                state.alpha = None;
                walk(bytes, start, end, info, state)?;
                let (track, ts, data) = state.block.take().ok_or_else(|| bad("a block group without a block"))?;
                let alpha = state.alpha.take();
                state.push(info, track, ts, !state.referenced, data, alpha);
            }
            BLOCK => {
                let (track, ts, _, data) = block(body, state)?;
                state.block = Some((track, ts, data));
            }
            REFERENCE_BLOCK => {
                let _ = int(body);
                state.referenced = true;
            }
            BLOCK_ADDITIONAL => state.alpha = Some(body.to_vec()),
            _ => {}
        }
        r.at = end;
    }
    Ok(())
}

fn block(body: &[u8], state: &State) -> Result<(u64, i64, u8, Vec<u8>), StreamError> {
    let mut r = Reader { bytes: body, at: 0 };
    let (track, _) = r.vint(false)?;
    if track != state.video_track && Some(track) != state.audio_track {
        return Err(bad("a block on a track the file does not declare"));
    }
    let head = body.get(r.at..r.at + 3).ok_or_else(|| bad("a short block"))?;
    let relative = i16::from_be_bytes([head[0], head[1]]);
    let flags = head[2];
    if flags & 0x06 != 0 {
        return Err(bad("laced blocks"));
    }
    Ok((track, state.cluster_ts + i64::from(relative), flags, body[r.at + 3..].to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_and_ints_use_the_shortest_form() {
        let mut out = Vec::new();
        put_size(&mut out, 0);
        put_size(&mut out, 126);
        put_size(&mut out, 127);
        assert_eq!(out, [0x80, 0xFE, 0x40, 0x7F], "127 is the one-byte unknown marker, so it takes two bytes");
        for v in [0i64, 1, -1, 127, 128, -128, -129, 40_000, -40_000] {
            let mut out = Vec::new();
            put_int(&mut out, REFERENCE_BLOCK, v);
            assert_eq!(int(&out[2..]), v, "{v}");
        }
    }

    #[test]
    fn a_written_file_reads_back() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().join("t.webm");
        let track = WebmTrack { width: 64, height: 32, fps: (30, 1), alpha: true };
        let mut sink = WebmSink::create(&path, track).unwrap_or_else(|e| panic!("{e}"));
        assert!(sink.needs_key(0));
        for (index, key) in [(0, true), (1, false), (3, false), (4, true), (5, false)] {
            let data = [u8::try_from(index).unwrap_or(0); 5];
            sink.write_frame(index, key, &data, Some(&[7, 7])).unwrap_or_else(|e| panic!("{e}"));
        }
        assert!(sink.write_frame(5, false, &[0], Some(&[0])).is_err(), "out of order");
        assert!(sink.write_frame(6, false, &[0], None).is_err(), "alpha missing");
        assert!(sink.write_audio(0, &[31 << 3, 0]).is_err(), "no audio track");
        let bytes = sink.finish(6).unwrap_or_else(|e| panic!("{e}"));
        let file = std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bytes, file.len() as u64);
        let info = read(&file).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((info.doc_type.as_str(), info.codec.as_str()), ("webm", "V_VP9"));
        assert_eq!((info.width, info.height, info.alpha_mode), (64, 32, 1));
        assert_eq!(info.duration_ms, Some(200.0));
        assert!(info.sized);
        assert_eq!(info.cues, 2);
        assert_eq!(info.audio, None);
        let ts: Vec<_> = info.frames.iter().map(|f| (f.ts_ms, f.key)).collect();
        assert_eq!(ts, [(0, true), (33, false), (100, false), (133, true), (167, false)]);
        assert!(info.frames.iter().all(|f| f.alpha.as_deref() == Some(&[7, 7][..])));
        assert_eq!(info.frames[2].data, [3; 5]);
    }

    #[test]
    fn a_file_cut_off_before_finish_still_reads() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().join("cut.webm");
        let track = WebmTrack { width: 8, height: 8, fps: (25, 1), alpha: false };
        let mut sink = WebmSink::create(&path, track).unwrap_or_else(|e| panic!("{e}"));
        sink.write_frame(0, true, &[1], None).unwrap_or_else(|e| panic!("{e}"));
        sink.write_frame(1, false, &[2], None).unwrap_or_else(|e| panic!("{e}"));
        sink.write_frame(2, true, &[3], None).unwrap_or_else(|e| panic!("{e}"));
        // Dropped without `finish`, as a crash would leave it: the first cluster is on disk.
        drop(sink);
        let info = read(&std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"))).unwrap_or_else(|e| panic!("{e}"));
        assert!(!info.sized);
        assert_eq!(info.duration_ms, None);
        assert_eq!(info.frames.len(), 2);
        assert!(info.frames[0].key && !info.frames[1].key);
    }

    #[test]
    fn a_sink_writes_into_an_open_file_and_refuses_one_with_bytes() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().join("open.webm");
        let file = File::create(&path).unwrap_or_else(|e| panic!("{e}"));
        let clone = file.try_clone().unwrap_or_else(|e| panic!("{e}"));
        let track = WebmTrack { width: 8, height: 8, fps: (25, 1), alpha: false };
        let mut sink =
            WebmSink::from_file(clone, Path::new("shown.webm"), track, None).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(sink.path(), Path::new("shown.webm"));
        sink.write_frame(0, true, &[1], None).unwrap_or_else(|e| panic!("{e}"));
        sink.finish(1).unwrap_or_else(|e| panic!("{e}"));
        let info = read(&std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"))).unwrap_or_else(|e| panic!("{e}"));
        assert!(info.sized && info.frames.len() == 1);
        let used = File::options().write(true).open(&path).unwrap_or_else(|e| panic!("{e}"));
        assert!(matches!(WebmSink::from_file(used, &path, track, None), Err(StreamError::Config(_))));
    }

    #[test]
    fn the_opus_head_follows_rfc_7845() {
        let head = opus_head(WebmAudio { channels: 2, pre_skip: 312 });
        assert_eq!(
            head,
            [
                b'O', b'p', b'u', b's', b'H', b'e', b'a', b'd', // magic
                1,    // version
                2,    // channels
                0x38, 0x01, // pre-skip 312, little-endian
                0x80, 0xBB, 0x00, 0x00, // 48000 Hz, little-endian
                0x00, 0x00, // output gain
                0,    // mapping family
            ]
        );
    }

    /// A 20 ms CELT packet (config 31, one frame) carrying `tag`.
    fn opus_packet(tag: u8) -> [u8; 2] {
        [31 << 3, tag]
    }

    #[test]
    fn audio_and_video_interleave_by_time_within_their_clusters() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().join("av.webm");
        let track = WebmTrack { width: 8, height: 8, fps: (25, 1), alpha: false };
        let audio = WebmAudio { channels: 1, pre_skip: 312 };
        let mut sink = WebmSink::create_with_audio(&path, track, Some(audio)).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(sink.audio(), Some(audio));
        assert!(sink.write_audio(0, &[]).is_err(), "not an Opus packet");
        // The first 100 ms of audio arrive ahead of the video.
        for k in 0..5u8 {
            sink.write_audio(u64::from(k) * 960, &opus_packet(k)).unwrap_or_else(|e| panic!("{e}"));
        }
        assert!(sink.write_audio(4 * 960, &opus_packet(0)).is_err(), "overlaps the packet before");
        // Video at 0, 40, 80 (key at 0), then a keyframe at 120 closes the first interval.
        for (index, key) in [(0u8, true), (1, false), (2, false), (3, true)] {
            sink.write_frame(u64::from(index), key, &[index], None).unwrap_or_else(|e| panic!("{e}"));
        }
        // The first interval waits for audio up to 120 ms: nothing is on disk past the header yet.
        let header_only =
            read(&std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"))).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(header_only.blocks, []);
        assert_eq!(header_only.audio.as_ref().map(|a| a.codec.as_str()), Some("A_OPUS"));
        // Audio at 100 ms reaches 120 ms: the first cluster is written, with its audio interleaved.
        sink.write_audio(5 * 960, &opus_packet(5)).unwrap_or_else(|e| panic!("{e}"));
        let cut = read(&std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"))).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cut.clusters, [0]);
        assert_eq!(cut.blocks, [(1, 0), (2, 0), (2, 20), (1, 40), (2, 40), (2, 60), (1, 80), (2, 80), (2, 100)]);
        // More audio, then the end: the rest goes in the last cluster.
        for k in 6..9u8 {
            sink.write_audio(u64::from(k) * 960, &opus_packet(k)).unwrap_or_else(|e| panic!("{e}"));
        }
        sink.write_frame(4, false, &[4], None).unwrap_or_else(|e| panic!("{e}"));
        // Four frames' worth of video is shorter than the audio, so the audio sets the duration.
        sink.finish(4).unwrap_or_else(|e| panic!("{e}"));
        let info = read(&std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"))).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(info.clusters, [0, 120]);
        assert_eq!(info.cues, 2);
        assert_eq!(&info.blocks[9..], [(1, 120), (2, 120), (2, 140), (1, 160), (2, 160)]);
        let a = info.audio.unwrap_or_default();
        assert_eq!((a.number, a.channels, a.sampling_frequency), (AUDIO_TRACK, 1, 48_000.0));
        assert_eq!((a.codec_delay_ns, a.seek_pre_roll_ns), (6_500_000, SEEK_PRE_ROLL_NS));
        assert_eq!(a.codec_private, opus_head(audio));
        let tags: Vec<_> = info.audio_blocks.iter().map(|b| b.data[1]).collect();
        assert_eq!(tags, (0..9).collect::<Vec<u8>>());
        assert_eq!(info.frames.len(), 5);
        // 9 packets of 20 ms less the 312-sample pre-skip outlast 4 frames at 25 fps.
        assert_eq!(info.duration_ms, Some((9.0 * 960.0 - 312.0) * 1000.0 / 48_000.0));
    }

    #[test]
    fn late_audio_waits_only_so_long_and_early_audio_opens_the_first_cluster() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let path = dir.path().join("late.webm");
        let track = WebmTrack { width: 8, height: 8, fps: (10, 1), alpha: false };
        let mut sink = WebmSink::create_with_audio(&path, track, Some(WebmAudio { channels: 2, pre_skip: 312 }))
            .unwrap_or_else(|e| panic!("{e}"));
        // Video starts at 200 ms; audio from 0 belongs before it, in the same cluster.
        sink.write_audio(0, &opus_packet(0)).unwrap_or_else(|e| panic!("{e}"));
        sink.write_frame(2, true, &[2], None).unwrap_or_else(|e| panic!("{e}"));
        sink.write_frame(3, true, &[3], None).unwrap_or_else(|e| panic!("{e}"));
        // No more audio: the video waits until it is MAX_AUDIO_WAIT_MS past the interval, then goes out alone.
        let wait_frames = u64::try_from(MAX_AUDIO_WAIT_MS / 100).unwrap_or(0);
        for index in 4..4 + wait_frames {
            sink.write_frame(index, false, &[0], None).unwrap_or_else(|e| panic!("{e}"));
        }
        let cut = read(&std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"))).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(cut.clusters, [0]);
        assert_eq!(cut.blocks, [(2, 0), (1, 200)]);
        assert!(cut.cues == 0 && !cut.sized);
        // Audio for 250 ms arrives after its interval went out: it goes in the next cluster, which opens at its time.
        sink.write_audio(12_000, &opus_packet(1)).unwrap_or_else(|e| panic!("{e}"));
        sink.finish(4 + wait_frames).unwrap_or_else(|e| panic!("{e}"));
        let info = read(&std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"))).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(info.clusters, [0, 250]);
        assert_eq!(info.blocks[2..4], [(2, 250), (1, 300)]);
        assert_eq!(info.cues, 2);
        assert_eq!(info.audio_blocks.len(), 2);
    }
}
