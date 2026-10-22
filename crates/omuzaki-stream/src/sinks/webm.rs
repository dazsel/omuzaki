//! A `WebM` (Matroska) writer for one VP9 video track, with alpha carried the way `FFmpeg`, Chrome and OBS read it:
//! `AlphaMode` 1 on the track and each frame's alpha packet in `BlockAdditional` id 1 (`docs/design/record.md` D6).
//!
//! Crash-safe by construction: the segment starts with an unknown size and each cluster (one keyframe interval) is
//! written whole as it closes, so a file cut off by a crash plays up to its last cluster. [`WebmSink::finish`] then
//! writes the cues, fills the seek head and the duration into space reserved for them, and sets the segment's size.
//! Timestamps are milliseconds; a frame's is its index on the take's frame grid, so a frame the encoder never saw
//! shows as the previous one held longer, and audio stays in sync.
//!
//! [`read`] parses what this writer writes, for checks and for reading a take back.
#![allow(
    clippy::disallowed_methods,
    reason = "the sink writes one file inside the take folder the recorder chose, under the Machine setting `record.dir`"
)]

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

/// The `WebM` writer.
#[derive(Debug)]
pub struct WebmSink {
    path: PathBuf,
    file: File,
    track: WebmTrack,
    segment_size_at: u64,
    segment_start: u64,
    seek_head_at: u64,
    duration_at: u64,
    info_at: u64,
    tracks_at: u64,
    cluster: Vec<u8>,
    cluster_ts: Option<i64>,
    cluster_key: bool,
    previous_ts: Option<i64>,
    cues: Vec<(i64, u64)>,
    written: u64,
}

fn io(path: &Path, e: &std::io::Error) -> StreamError {
    StreamError::Io(format!("{}: {e}", path.display()))
}

impl WebmSink {
    /// Creates `path` (refusing to replace a file) and writes the header for `track`.
    ///
    /// # Errors
    /// [`StreamError::Io`] when the file can't be created or written; [`StreamError::Config`] for a zero size or rate.
    pub fn create(path: &Path, track: WebmTrack) -> Result<Self, StreamError> {
        if track.width == 0 || track.height == 0 || track.fps.0 == 0 || track.fps.1 == 0 {
            return Err(StreamError::Config("a WebM track needs a size and a frame rate above zero".into()));
        }
        let file = File::options().write(true).create_new(true).open(path).map_err(|e| io(path, &e))?;
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
        let frame_ns = u64::from(track.fps.1) * 1_000_000_000 / u64::from(track.fps.0);
        put_master(&mut head, TRACKS, |b| {
            put_master(b, TRACK_ENTRY, |t| {
                put_uint(t, TRACK_NUMBER, 1);
                put_uint(t, TRACK_UID, 1);
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
        });
        let mut sink = Self {
            path: path.to_owned(),
            file,
            track,
            segment_size_at,
            segment_start,
            seek_head_at,
            duration_at,
            info_at,
            tracks_at,
            cluster: Vec::new(),
            cluster_ts: None,
            cluster_key: false,
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

    /// The timestamp, in milliseconds, of frame `index`.
    #[must_use]
    pub fn timestamp_ms(&self, index: u64) -> i64 {
        let (num, den) = (u128::from(self.track.fps.0), u128::from(self.track.fps.1));
        i64::try_from((u128::from(index) * 1000 * den + num / 2) / num).unwrap_or(i64::MAX)
    }

    /// Whether frame `index` must be a keyframe for its cluster to stay within [`MAX_CLUSTER_MS`].
    #[must_use]
    pub fn needs_key(&self, index: u64) -> bool {
        self.cluster_ts.is_none_or(|start| self.timestamp_ms(index) - start > MAX_CLUSTER_MS)
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
    /// [`StreamError::Mux`] for a frame out of order, a missing alpha packet, or a delta frame where a cluster must
    /// start; [`StreamError::Io`] when the disk write fails.
    pub fn write_frame(&mut self, index: u64, key: bool, data: &[u8], alpha: Option<&[u8]>) -> Result<(), StreamError> {
        let ts = self.timestamp_ms(index);
        if self.previous_ts.is_some_and(|p| ts <= p) {
            return Err(StreamError::Mux(format!("frame {index} comes after a later one")));
        }
        if self.track.alpha != alpha.is_some() {
            return Err(StreamError::Mux("every frame of an alpha track carries alpha, and only those".into()));
        }
        if key || self.cluster_ts.is_none() {
            self.close_cluster()?;
            self.cluster_ts = Some(ts);
            self.cluster_key = key;
            put_uint(&mut self.cluster, TIMESTAMP, u64::try_from(ts).unwrap_or(0));
        }
        let start = self.cluster_ts.unwrap_or(ts);
        let relative = i16::try_from(ts - start)
            .map_err(|_| StreamError::Mux(format!("frame {index} is too far from its cluster's keyframe")))?;
        let mut header = Vec::with_capacity(4);
        header.push(0x81); // track 1
        header.extend_from_slice(&relative.to_be_bytes());
        match alpha {
            None => {
                header.push(if key { 0x80 } else { 0x00 });
                put_id(&mut self.cluster, SIMPLE_BLOCK);
                put_size(&mut self.cluster, (header.len() + data.len()) as u64);
                self.cluster.extend_from_slice(&header);
                self.cluster.extend_from_slice(data);
            }
            Some(alpha) => {
                header.push(0x00);
                let previous = self.previous_ts;
                put_master(&mut self.cluster, BLOCK_GROUP, |g| {
                    put_id(g, BLOCK);
                    put_size(g, (header.len() + data.len()) as u64);
                    g.extend_from_slice(&header);
                    g.extend_from_slice(data);
                    // A block group with no reference is a keyframe.
                    if let (false, Some(p)) = (key, previous) {
                        put_int(g, REFERENCE_BLOCK, p - ts);
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
        self.previous_ts = Some(ts);
        Ok(())
    }

    fn close_cluster(&mut self) -> Result<(), StreamError> {
        let Some(ts) = self.cluster_ts.take() else { return Ok(()) };
        let position = self.written - self.segment_start;
        if self.cluster_key {
            self.cues.push((ts, position));
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

    /// Writes the last cluster and the cues, fills in the seek head and the duration (`frames` frames long), and sets
    /// the segment's size. Returns the file's size in bytes.
    ///
    /// # Errors
    /// [`StreamError::Io`] when a write or seek fails.
    pub fn finish(mut self, frames: u64) -> Result<u64, StreamError> {
        self.close_cluster()?;
        let cues_at = self.written - self.segment_start;
        let mut cues = Vec::new();
        put_master(&mut cues, CUES, |c| {
            for (ts, position) in &self.cues {
                put_master(c, CUE_POINT, |p| {
                    put_uint(p, CUE_TIME, u64::try_from(*ts).unwrap_or(0));
                    put_master(p, CUE_TRACK_POSITIONS, |t| {
                        put_uint(t, CUE_TRACK, 1);
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
        let duration_ms =
            (u128::from(frames) * 1000 * u128::from(self.track.fps.1)) as f64 / f64::from(self.track.fps.0);
        let mut duration = Vec::with_capacity(DURATION_RESERVE);
        put_float(&mut duration, DURATION, duration_ms);

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

/// What [`read`] found.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WebmInfo {
    /// The document type.
    pub doc_type: String,
    /// The track's codec.
    pub codec: String,
    /// Width in pixels.
    pub width: u64,
    /// Height in pixels.
    pub height: u64,
    /// The track's `AlphaMode`.
    pub alpha_mode: u64,
    /// The duration in milliseconds, once the file was finished.
    pub duration_ms: Option<f64>,
    /// Cue points.
    pub cues: usize,
    /// Whether the segment's size was set.
    pub sized: bool,
    /// The frames, in order.
    pub frames: Vec<WebmFrame>,
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

/// Parses a `WebM` file this module wrote (or any single-track VP9 `WebM` without lacing).
///
/// # Errors
/// [`StreamError::Mux`] for bytes that are not such a file.
pub fn read(bytes: &[u8]) -> Result<WebmInfo, StreamError> {
    let mut info = WebmInfo::default();
    walk(bytes, 0, bytes.len(), &mut info, &mut State::default())?;
    Ok(info)
}

#[derive(Default)]
struct State {
    cluster_ts: i64,
    block: Option<(i64, Vec<u8>)>,
    referenced: bool,
    alpha: Option<Vec<u8>>,
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
            EBML | SEGMENT | TRACKS | TRACK_ENTRY | VIDEO | CLUSTER | BLOCK_ADDITIONS | BLOCK_MORE | CUES => {
                if id == SEGMENT {
                    info.sized = !unknown;
                }
                walk(bytes, start, end, info, state)?;
            }
            INFO => walk(bytes, start, end, info, state)?,
            CUE_POINT => info.cues += 1,
            DOC_TYPE => info.doc_type = String::from_utf8_lossy(body).into_owned(),
            CODEC_ID => info.codec = String::from_utf8_lossy(body).into_owned(),
            PIXEL_WIDTH => info.width = uint(body),
            PIXEL_HEIGHT => info.height = uint(body),
            ALPHA_MODE => info.alpha_mode = uint(body),
            DURATION => {
                let raw: [u8; 8] = body.try_into().map_err(|_| bad("a short duration"))?;
                info.duration_ms = Some(f64::from_be_bytes(raw));
            }
            TIMESTAMP => state.cluster_ts = i64::try_from(uint(body)).unwrap_or(0),
            SIMPLE_BLOCK => {
                let (ts, flags, data) = block(body, state.cluster_ts)?;
                info.frames.push(WebmFrame { ts_ms: ts, key: flags & 0x80 != 0, data, alpha: None });
            }
            BLOCK_GROUP => {
                state.block = None;
                state.referenced = false;
                state.alpha = None;
                walk(bytes, start, end, info, state)?;
                let (ts, data) = state.block.take().ok_or_else(|| bad("a block group without a block"))?;
                info.frames.push(WebmFrame { ts_ms: ts, key: !state.referenced, data, alpha: state.alpha.take() });
            }
            BLOCK => {
                let (ts, _, data) = block(body, state.cluster_ts)?;
                state.block = Some((ts, data));
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

fn block(body: &[u8], cluster_ts: i64) -> Result<(i64, u8, Vec<u8>), StreamError> {
    let mut r = Reader { bytes: body, at: 0 };
    let (track, _) = r.vint(false)?;
    if track != 1 {
        return Err(bad("a second track"));
    }
    let head = body.get(r.at..r.at + 3).ok_or_else(|| bad("a short block"))?;
    let relative = i16::from_be_bytes([head[0], head[1]]);
    let flags = head[2];
    if flags & 0x06 != 0 {
        return Err(bad("laced blocks"));
    }
    Ok((cluster_ts + i64::from(relative), flags, body[r.at + 3..].to_vec()))
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
        let bytes = sink.finish(6).unwrap_or_else(|e| panic!("{e}"));
        let file = std::fs::read(&path).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(bytes, file.len() as u64);
        let info = read(&file).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!((info.doc_type.as_str(), info.codec.as_str()), ("webm", "V_VP9"));
        assert_eq!((info.width, info.height, info.alpha_mode), (64, 32, 1));
        assert_eq!(info.duration_ms, Some(200.0));
        assert!(info.sized);
        assert_eq!(info.cues, 2);
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
}
