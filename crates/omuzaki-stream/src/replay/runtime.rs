//! The replay buffer (`docs/design/record.md` D2 to D5, D11): an [`Output`] that keeps the last seconds of the program
//! as encoded packets and audio, and saves "the last 60 s" as a clip without stalling anything.
//!
//! - **[`ReplayCore`]** is single-threaded and deterministic: it encodes each frame, pushes the packets into the
//!   [`RollingBuffer`] (video first, then the audio that arrived), and selects a clip. Tests drive it directly.
//! - **[`ReplayBuffer`]** is the thin threaded wrapper: `submit` copies the frame into one of four pooled slots and
//!   returns; the encoder thread owns the core; a save fixes its window on that thread between two frames (payload
//!   handles cloned, no bytes copied) and a writer thread stream-copies it into the clip. Nothing is re-encoded.
//! - **Saves queue:** a second save while one runs waits; a third is `Busy`.
//! - **Audio** is timed by its samples: sample `start` of a track lines up with frame 0 (the take's [`TrackStart`]),
//!   and a clip's WAV files hold exactly the samples of its span, missing ones as silence counted in `gaps`. Private
//!   and muted audio is never stored: it is kept as silence.
//! - **An encoder error** recreates the encoder from the factory with a forced keyframe, keeps the ring and the frame
//!   timeline, and counts `encoder_restarts` (D11: the ring outlives the encoder).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError, channel, sync_channel};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{AlphaEncoding, Image};
use omuzaki_stream_api::encoder::{
    AlphaKind, CodecId, EncodeConfig, Encoder, EncoderFactory, Frame, StreamSpec, VideoSpec,
};
use omuzaki_stream_api::error::StreamError;
use omuzaki_stream_api::mux::{Container, MuxerFactory};
use omuzaki_stream_api::output::{
    AlphaNeed, Lanes, Output, OutputConfig, OutputInfo, OutputState, OutputStatus, SubmitReport,
};
use omuzaki_stream_api::packet::{Packet, StreamId};
use omuzaki_stream_api::time::{Ticks, Timebase};
use omuzaki_update_api::hold::Hold;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::ring::{ChunkKind, MemoryPayload, Payload, Push, RollingBuffer, Selection, Stats};
use super::segment::{SegmentStore, Slice};
use super::settings::{Backing, ReplaySettings};
use crate::record::runtime::{Align, AudioTrack, TrackStart};
use crate::record::take::{stem, unique_stem};
use crate::sinks::wav::WavSink;

/// The registered kind of the replay output.
pub const KIND: &str = "replay";
/// Frames the encoder thread's queue holds (D2): a full queue counts a dropped frame.
pub const QUEUE_FRAMES: usize = 4;
/// Saves running or waiting at once; one more is `Busy` (D5).
pub const SAVE_QUEUE: usize = 2;
/// The most samples read from a track at a time.
const AUDIO_CHUNK: usize = 4096;
/// How long the encoder thread waits for a frame before it looks at its save commands again.
const IDLE_POLL: Duration = Duration::from_millis(2);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A chunk's payload, in memory or in a disk segment.
#[derive(Clone, Debug)]
pub enum Stored {
    /// In memory.
    Memory(MemoryPayload),
    /// In a segment file.
    Disk(Slice),
}

impl Payload for Stored {
    fn len(&self) -> u64 {
        match self {
            Self::Memory(m) => m.len(),
            Self::Disk(s) => s.len(),
        }
    }
}

impl Stored {
    /// The packet, read back from disk when it is there.
    ///
    /// # Errors
    /// [`StreamError::Io`] when a disk segment can't be read.
    pub fn packet(&self) -> Result<Packet, StreamError> {
        match self {
            Self::Memory(m) => Ok(m.packet().clone()),
            Self::Disk(s) => s.read(),
        }
    }
}

/// Why a replay request failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReplayError {
    /// The replay buffer isn't running.
    #[error("the replay buffer is off")]
    Disabled,
    /// Nothing is buffered yet.
    #[error("the replay buffer holds nothing yet")]
    Empty,
    /// Two saves are already running or waiting.
    #[error("a save is already running")]
    Busy,
    /// Encoding, reading or writing failed.
    #[error(transparent)]
    Stream(#[from] StreamError),
}

/// One audio track of a clip: what its WAV file is called and holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackInfo {
    /// The suffix of its file (`mic` → `<stem>_mic.wav`).
    pub name: String,
    /// Samples per second per channel.
    pub rate: u32,
    /// Interleaved channels.
    pub channels: u16,
}

/// What a save wrote.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SaveReport {
    /// The clip's video file.
    pub path: PathBuf,
    /// Its audio files, one per track.
    pub tracks: Vec<PathBuf>,
    /// The length asked for, in seconds.
    pub requested_s: f64,
    /// The clip's length, in seconds.
    pub actual_s: f64,
    /// How much earlier than asked it starts (it starts on a keyframe), in seconds.
    pub early_by_s: f64,
    /// How much shorter than asked it is, when the buffer held less, in seconds.
    pub short_by_s: f64,
    /// Video frames in the clip.
    pub frames: u64,
    /// The index of its first frame since the buffer started.
    pub first_frame: u64,
    /// Bytes written, every file.
    pub bytes: u64,
    /// Seconds from the save's window being fixed to the files being complete.
    pub took_s: f64,
    /// Runs of missing audio filled with silence, all tracks.
    pub gaps: u32,
}

/// What `record.replay_status` answers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ReplayStatus {
    /// Whether the buffer runs.
    pub running: bool,
    /// Seconds held, from the oldest keyframe.
    pub buffered_s: f64,
    /// The length a save holds by default, in seconds.
    pub length_s: f64,
    /// Bytes the buffer and any save in progress keep alive.
    pub bytes: u64,
    /// The cap on those bytes.
    pub cap_bytes: u64,
    /// `memory` or `disk`.
    pub backing: Option<Backing>,
    /// Saves running or waiting.
    pub saving: u32,
    /// Frames buffered since the start.
    pub frames: u64,
    /// Frames lost because the encoder thread's queue was full.
    pub dropped_frames: u64,
    /// Times the encoder was recreated after an error.
    pub encoder_restarts: u32,
    /// Chunks refused for want of room (a save pinned the cap).
    pub refused: u64,
    /// The last save, once one completes.
    pub last_save: Option<SaveReport>,
    /// The last error, if any.
    pub error: Option<String>,
}

/// One track feeding the ring.
struct Feed {
    track: AudioTrack,
    index: u8,
    /// The stream position that lines up with frame 0, once fixed.
    start: Option<u64>,
    align: Option<Align>,
    buf: Vec<f32>,
}

/// The single-threaded replay core: the encoder, the ring and the audio feeds.
pub struct ReplayCore {
    ring: RollingBuffer<Stored>,
    store: Option<SegmentStore>,
    settings: ReplaySettings,
    encoders: Arc<dyn EncoderFactory>,
    encode: EncodeConfig,
    encoder: Box<dyn Encoder>,
    fps: (u32, u32),
    /// The indexes of frames sent to the encoder whose packets haven't come out yet.
    sent: VecDeque<u64>,
    packets: Vec<Packet>,
    feeds: Vec<Feed>,
    started: bool,
    frames: u64,
    restarts: u32,
}

#[allow(clippy::cast_precision_loss)] // Frame and sample counts far under 2^52.
fn ratio(n: u64, per: f64) -> f64 {
    n as f64 / per
}

impl ReplayCore {
    /// A core for `settings`, encoding with `encode` from `encoders`, keeping chunks on disk in `cache` when the
    /// settings say disk.
    ///
    /// # Errors
    /// [`StreamError::Config`] for settings out of range; the factory's error when the encoder won't open;
    /// [`StreamError::Io`] when the cache folder can't be made.
    pub fn new(
        settings: ReplaySettings,
        encode: EncodeConfig,
        cache: &Path,
        tracks: Vec<AudioTrack>,
        encoders: Arc<dyn EncoderFactory>,
    ) -> Result<Self, StreamError> {
        settings.check().map_err(|e| StreamError::Config(e.to_string()))?;
        let ring = RollingBuffer::new(settings.caps()).map_err(|e| StreamError::Config(e.to_string()))?;
        let store = match settings.backing {
            Backing::Disk => Some(SegmentStore::open(cache)?),
            _ => None,
        };
        let encoder = encoders.create(&encode)?;
        let mut feeds = Vec::with_capacity(tracks.len());
        for (i, mut track) in tracks.into_iter().enumerate() {
            let (start, align) = match std::mem::replace(&mut track.start, TrackStart::First) {
                TrackStart::First => (None, None),
                TrackStart::Sample(s) => (Some(s), None),
                TrackStart::FirstFrame(align) => (None, Some(align)),
            };
            let index = u8::try_from(i).map_err(|_| StreamError::Config("more than 255 audio tracks".into()))?;
            feeds.push(Feed { track, index, start, align, buf: vec![0.0; AUDIO_CHUNK] });
        }
        Ok(Self {
            ring,
            store,
            settings,
            encoders,
            fps: encode.video.fps,
            encode,
            encoder,
            sent: VecDeque::with_capacity(8),
            packets: Vec::with_capacity(8),
            feeds,
            started: false,
            frames: 0,
            restarts: 0,
        })
    }

    fn frame_seconds(&self, index: u64) -> f64 {
        ratio(index * u64::from(self.fps.1), f64::from(self.fps.0.max(1)))
    }

    /// Encodes `frame`, pushes its packets, then the audio that arrived since the previous frame.
    ///
    /// # Errors
    /// The encoder's error when a fresh encoder fails on the same frame too, or a disk segment's write error.
    pub fn push_frame(&mut self, frame: &Frame<'_>) -> Result<(), StreamError> {
        if !self.started {
            self.started = true;
            let zero = frame.capture_time.0 - seconds_to_ticks(self.frame_seconds(frame.index));
            for feed in &mut self.feeds {
                if let Some(align) = feed.align.take() {
                    feed.start = Some(align(Ticks(zero)));
                }
            }
        }
        if let Err(e) = self.encoder.send(frame) {
            // D11: a fresh encoder, a forced keyframe, the ring and the timeline kept.
            let fresh = self.encoders.create(&self.encode).map_err(|_| e)?;
            self.encoder = fresh;
            self.encoder.request_keyframe();
            self.restarts += 1;
            self.sent.clear();
            self.encoder.send(frame)?;
        }
        self.sent.push_back(frame.index);
        self.frames = self.frames.max(frame.index + 1);
        self.packets.clear();
        self.encoder.receive(&mut self.packets)?;
        let mut packets = std::mem::take(&mut self.packets);
        for packet in packets.drain(..) {
            let index = self.sent.pop_front().unwrap_or(frame.index);
            self.push_video(packet, index)?;
        }
        self.packets = packets;
        self.pump_audio()
    }

    fn push_video(&mut self, mut packet: Packet, index: u64) -> Result<(), StreamError> {
        let pts_frames = i64::try_from(index).unwrap_or(i64::MAX);
        (packet.pts, packet.dts, packet.duration, packet.stream) = (pts_frames, pts_frames, 1, StreamId(0));
        let kind = ChunkKind::Video { keyframe: packet.keyframe };
        let pts = Seconds(self.frame_seconds(index));
        let duration = Seconds(self.frame_seconds(1));
        if self.push(kind, pts, duration, packet)? == Push::Refused {
            self.encoder.request_keyframe();
        }
        Ok(())
    }

    fn push(&mut self, kind: ChunkKind, pts: Seconds, duration: Seconds, packet: Packet) -> Result<Push, StreamError> {
        let key = matches!(kind, ChunkKind::Video { keyframe: true });
        let pushed = match &self.store {
            None => {
                let len = MemoryPayload::stored_len(&packet);
                self.ring.push_with(kind, pts, duration, len, |charge| {
                    Ok::<_, StreamError>(Stored::Memory(MemoryPayload::new(packet, charge)))
                })?
            }
            Some(store) => {
                let len = SegmentStore::record_len(&packet);
                self.ring
                    .push_with(kind, pts, duration, len, |charge| store.put(&packet, charge, key).map(Stored::Disk))?
            }
        };
        if pushed == Push::Refused
            && let Some(store) = &self.store
        {
            store.close();
        }
        Ok(pushed)
    }

    /// Reads every track's new samples into audio chunks.
    fn pump_audio(&mut self) -> Result<(), StreamError> {
        for f in 0..self.feeds.len() {
            loop {
                let feed = &mut self.feeds[f];
                let run = feed.track.source.read(&mut feed.buf);
                if run.len == 0 {
                    break;
                }
                let s0 = *feed.start.get_or_insert(run.start_sample);
                let end = run.start_sample + run.len as u64;
                if end <= s0 {
                    continue;
                }
                let skip = usize::try_from(s0.saturating_sub(run.start_sample)).unwrap_or(run.len);
                let position = run.start_sample.max(s0) - s0;
                let samples = &feed.buf[skip..run.len];
                let mut data = Vec::with_capacity(samples.len() * 4);
                if run.private || run.muted {
                    // Never stored: the clip gets silence where the person was private or muted.
                    data.resize(samples.len() * 4, 0);
                } else {
                    for s in samples {
                        data.extend_from_slice(&s.to_le_bytes());
                    }
                }
                let (rate, channels, index) = (feed.track.rate, feed.track.channels, feed.index);
                let per_second = f64::from(rate.max(1)) * f64::from(channels.max(1));
                let count = samples.len() as u64;
                let packet = Packet {
                    stream: StreamId(index.saturating_add(1)),
                    pts: i64::try_from(position).unwrap_or(i64::MAX),
                    dts: i64::try_from(position).unwrap_or(i64::MAX),
                    duration: i64::try_from(count).unwrap_or(i64::MAX),
                    keyframe: true,
                    data: Bytes::from(data),
                    side_data: Vec::new(),
                };
                let pts = Seconds(ratio(position, per_second));
                let duration = Seconds(ratio(count, per_second));
                self.push(ChunkKind::Audio { track: index }, pts, duration, packet)?;
            }
        }
        Ok(())
    }

    /// The newest `length` seconds as a clip's chunks.
    pub fn select(&self, length: Seconds) -> Selection<Stored> {
        self.ring.select(length)
    }

    /// The clip's video stream: the encoder's, timed in frames.
    pub fn spec(&self) -> StreamSpec {
        let mut spec = self.encoder.spec();
        spec.timebase = Timebase { num: self.fps.1, den: self.fps.0 };
        spec
    }

    /// The audio tracks a clip writes.
    pub fn tracks(&self) -> Vec<TrackInfo> {
        self.feeds
            .iter()
            .map(|f| TrackInfo { name: f.track.name.clone(), rate: f.track.rate, channels: f.track.channels })
            .collect()
    }

    /// What the ring holds and has done.
    pub fn stats(&self) -> Stats {
        self.ring.stats()
    }

    /// Bytes kept alive by the ring and by saves in progress.
    pub fn bytes(&self) -> u64 {
        self.ring.meter().get()
    }

    /// The settings it runs with.
    pub fn settings(&self) -> ReplaySettings {
        self.settings
    }

    /// Whether nothing is buffered.
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// Frames pushed: one past the highest index.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Times the encoder was recreated.
    pub fn restarts(&self) -> u32 {
        self.restarts
    }
}

#[allow(clippy::cast_possible_truncation)] // Seconds of a run: far below 2^63 ns.
fn seconds_to_ticks(seconds: f64) -> i64 {
    (seconds * 1e9).round() as i64
}

/// Where and how a clip is written.
#[derive(Clone)]
pub struct ClipTarget {
    /// The folder clips go into.
    pub dir: PathBuf,
    /// The container of the video file.
    pub container: Container,
    /// Its extension, without the dot.
    pub extension: &'static str,
    /// What writes it.
    pub muxers: Arc<dyn MuxerFactory>,
}

/// Writes `sel` as a clip in `target`: the video by stream copy (temporary name, then rename) and one WAV file per
/// track holding exactly the samples of the clip's span.
///
/// # Errors
/// [`ReplayError::Empty`] for a selection without video; [`ReplayError::Stream`] when reading or writing fails.
pub fn write_clip(
    sel: &Selection<Stored>,
    spec: &StreamSpec,
    tracks: &[TrackInfo],
    target: &ClipTarget,
) -> Result<SaveReport, ReplayError> {
    let started = Instant::now();
    let video: Vec<_> = sel.chunks.iter().filter(|c| matches!(c.kind, ChunkKind::Video { .. })).collect();
    let Some(first) = video.first() else { return Err(ReplayError::Empty) };
    let first_frame = first.data.packet()?.pts;
    #[allow(clippy::disallowed_methods, reason = "the person's replay folder (`replay.dir`), not an op's file access")]
    std::fs::create_dir_all(&target.dir)
        .map_err(|e| StreamError::Io(format!("could not create {}: {e}", target.dir.display())))?;
    let stem = unique_stem(&target.dir, &stem(SystemTime::now(), Some("replay")));
    let path = target.dir.join(format!("{stem}.{}", target.extension));
    let temporary = target.dir.join(format!("{stem}.{}.part", target.extension));
    let mut muxer = target.muxers.create(&temporary, target.container)?;
    let stream = muxer.add_stream(spec)?;
    for chunk in &video {
        let mut packet = chunk.data.packet()?;
        packet.stream = stream;
        packet.pts -= first_frame;
        packet.dts -= first_frame;
        muxer.write(&packet)?;
    }
    let mut bytes = muxer.finish()?.bytes;
    #[allow(clippy::disallowed_methods, reason = "the clip this save just wrote, not an op's file access")]
    std::fs::rename(&temporary, &path).map_err(|e| StreamError::Io(format!("{}: {e}", path.display())))?;
    let mut files = Vec::with_capacity(tracks.len());
    let mut gaps = 0;
    for (i, track) in tracks.iter().enumerate() {
        let (written, track_gaps) = write_track(sel, i, track, &target.dir.join(format!("{stem}_{}", track.name)))?;
        gaps += track_gaps;
        for file in written {
            #[allow(clippy::disallowed_methods, reason = "the clip's own file")]
            let len = std::fs::metadata(&file).map_or(0, |m| m.len());
            bytes += len;
            files.push(file);
        }
    }
    Ok(SaveReport {
        path,
        tracks: files,
        requested_s: sel.requested.0,
        actual_s: sel.actual.0,
        early_by_s: sel.early_by.0,
        short_by_s: sel.short_by.0,
        frames: video.len() as u64,
        first_frame: u64::try_from(first_frame).unwrap_or(0),
        bytes,
        took_s: started.elapsed().as_secs_f64(),
        gaps,
    })
}

/// Writes track `index` of `sel` to `base` (`.wav` added): exactly the samples from the clip's start to its end,
/// silence where samples are missing. Returns the files and the number of silent runs.
fn write_track(
    sel: &Selection<Stored>,
    index: usize,
    track: &TrackInfo,
    base: &Path,
) -> Result<(Vec<PathBuf>, u32), StreamError> {
    let channels = u64::from(track.channels.max(1));
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // Seconds of a clip at an audio rate.
    let to_frames = |s: f64| (s * f64::from(track.rate)).round().max(0.0) as u64;
    let clip_start = to_frames(sel.start.0);
    let total = to_frames(sel.end.0).saturating_sub(clip_start);
    let mut sink = WavSink::create(base, track.rate, track.channels)?;
    let (mut cursor, mut gaps) = (0u64, 0u32);
    let mut samples = Vec::new();
    for chunk in &sel.chunks {
        if chunk.kind != (ChunkKind::Audio { track: u8::try_from(index).unwrap_or(u8::MAX) }) || cursor >= total {
            continue;
        }
        let packet = chunk.data.packet()?;
        let at = u64::try_from(packet.pts).unwrap_or(0) / channels;
        let count = packet.data.len() as u64 / 4 / channels;
        if at + count <= clip_start + cursor {
            continue;
        }
        let offset = at.saturating_sub(clip_start);
        if offset > cursor {
            let missing = (offset - cursor).min(total - cursor);
            sink.write_silence(missing)?;
            cursor += missing;
            gaps += 1;
        }
        let skip = (clip_start + cursor).saturating_sub(at);
        let take = count.saturating_sub(skip).min(total - cursor);
        samples.clear();
        let from = usize::try_from(skip * channels * 4).unwrap_or(usize::MAX);
        let to = usize::try_from((skip + take) * channels * 4).unwrap_or(usize::MAX);
        if let Some(bytes) = packet.data.get(from..to) {
            samples.extend(bytes.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
        }
        sink.write(&samples)?;
        cursor += take;
    }
    if cursor < total {
        sink.write_silence(total - cursor)?;
        gaps += 1;
    }
    Ok((sink.finish()?, gaps))
}

/// What a replay buffer needs to start, with its encoder and writer chosen.
pub struct ReplayPlan {
    /// Length, cap and backing.
    pub settings: ReplaySettings,
    /// Where clips go, and how they are written.
    pub target: ClipTarget,
    /// The folder a disk-backed buffer keeps its segments in.
    pub cache: PathBuf,
    /// The codec the ring stores.
    pub codec: CodecId,
    /// What makes encoders.
    pub encoders: Arc<dyn EncoderFactory>,
    /// The audio tracks.
    pub tracks: Vec<AudioTrack>,
    /// The update hold the buffer keeps while it captures (`Capturing`), so the app never restarts into an update
    /// with a clip still to save.
    pub hold: Option<Hold>,
}

impl ReplayPlan {
    /// The default plan (D3): VP9 with alpha in `WebM` clips, WAV tracks beside them.
    pub fn vp9(settings: ReplaySettings, dir: PathBuf, cache: PathBuf, tracks: Vec<AudioTrack>) -> Self {
        Self {
            settings,
            target: ClipTarget {
                dir,
                container: Container::Webm,
                extension: "webm",
                muxers: Arc::new(crate::sinks::webm_mux::WebmMuxerFactory),
            },
            cache,
            codec: CodecId::Vp9Alpha,
            encoders: Arc::new(crate::vp9::alpha::Vp9AlphaFactory),
            tracks,
            hold: None,
        }
    }
}

/// A save command for the encoder thread.
struct SaveCommand {
    length: Option<Seconds>,
    reply: SyncSender<Result<SaveReport, ReplayError>>,
}

/// A fixed window for the writer thread.
struct WriteJob {
    sel: Selection<Stored>,
    spec: StreamSpec,
    tracks: Vec<TrackInfo>,
    reply: SyncSender<Result<SaveReport, ReplayError>>,
}

/// One frame for the encoder thread.
struct FrameMsg {
    index: u64,
    capture: Ticks,
    image: Image,
}

/// State the threads and the handles share.
#[derive(Default)]
struct Shared {
    status: Mutex<ReplayStatus>,
    running: AtomicBool,
    pending: AtomicUsize,
    dropped: AtomicU64,
    queued: AtomicU64,
}

impl Shared {
    fn end_save(&self, result: &Result<SaveReport, ReplayError>) {
        let mut status = lock(&self.status);
        match result {
            Ok(report) => status.last_save = Some(report.clone()),
            Err(ReplayError::Empty) => {}
            Err(e) => status.error = Some(e.to_string()),
        }
        // Taken before the save was queued, so it never underflows.
        let left = self.pending.fetch_sub(1, Ordering::AcqRel).saturating_sub(1);
        status.saving = u32::try_from(left).unwrap_or(u32::MAX);
    }
}

/// A save in progress: wait for its report.
#[derive(Debug)]
pub struct SaveTicket(Receiver<Result<SaveReport, ReplayError>>);

impl SaveTicket {
    /// Waits for the clip to be complete.
    ///
    /// # Errors
    /// The save's error, or [`ReplayError::Disabled`] if the buffer stopped first.
    pub fn wait(self) -> Result<SaveReport, ReplayError> {
        self.0.recv().unwrap_or(Err(ReplayError::Disabled))
    }

    /// The report if the save has finished.
    pub fn try_wait(&self) -> Option<Result<SaveReport, ReplayError>> {
        match self.0.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Err(ReplayError::Disabled)),
        }
    }
}

/// Saves and reads the status of a running replay buffer from any thread, while the stream service owns the output.
#[derive(Clone)]
pub struct ReplayHandle {
    control: Sender<SaveCommand>,
    shared: Arc<Shared>,
}

impl std::fmt::Debug for ReplayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplayHandle").finish_non_exhaustive()
    }
}

impl ReplayHandle {
    /// Saves the last `length` seconds (the buffer's length when `None`). Returns at once; the window is fixed on the
    /// encoder thread within a frame, and the ticket gives the report when the files are complete.
    ///
    /// # Errors
    /// [`ReplayError::Disabled`] when the buffer isn't running, [`ReplayError::Busy`] when two saves already run or
    /// wait.
    pub fn save(&self, length: Option<Seconds>) -> Result<SaveTicket, ReplayError> {
        if !self.shared.running.load(Ordering::Acquire) {
            return Err(ReplayError::Disabled);
        }
        let claimed =
            self.shared.pending.try_update(Ordering::AcqRel, Ordering::Acquire, |n| (n < SAVE_QUEUE).then_some(n + 1));
        let Ok(before) = claimed else { return Err(ReplayError::Busy) };
        lock(&self.shared.status).saving = u32::try_from(before + 1).unwrap_or(u32::MAX);
        let (reply, ticket) = sync_channel(1);
        if self.control.send(SaveCommand { length, reply }).is_err() {
            self.shared.pending.fetch_sub(1, Ordering::AcqRel);
            return Err(ReplayError::Disabled);
        }
        Ok(SaveTicket(ticket))
    }

    /// The buffer's status.
    pub fn status(&self) -> ReplayStatus {
        let mut status = lock(&self.shared.status).clone();
        status.running = self.shared.running.load(Ordering::Acquire);
        status.dropped_frames = self.shared.dropped.load(Ordering::Relaxed);
        status
    }
}

/// The replay buffer output. See the module docs.
pub struct ReplayBuffer {
    plan: Option<ReplayPlan>,
    state: OutputState,
    control: Option<Receiver<SaveCommand>>,
    handle: ReplayHandle,
    frames: Option<SyncSender<FrameMsg>>,
    free: Option<Receiver<Image>>,
    worker: Option<JoinHandle<()>>,
    writer: Option<JoinHandle<()>>,
    submitted: u64,
    last_error: Option<StreamError>,
    /// Released when the buffer stops or is dropped.
    hold: Option<Hold>,
}

impl ReplayBuffer {
    /// A replay buffer for `plan`, not started yet.
    pub fn new(plan: ReplayPlan) -> Self {
        let (control_tx, control_rx) = channel();
        Self {
            plan: Some(plan),
            state: OutputState::Idle,
            control: Some(control_rx),
            handle: ReplayHandle { control: control_tx, shared: Arc::new(Shared::default()) },
            frames: None,
            free: None,
            worker: None,
            writer: None,
            submitted: 0,
            last_error: None,
            hold: None,
        }
    }

    /// A handle that saves and reads the status while the output runs.
    pub fn handle(&self) -> ReplayHandle {
        self.handle.clone()
    }

    fn launch(&mut self, cfg: &OutputConfig) -> Result<(), StreamError> {
        let mut plan = self.plan.take().ok_or(StreamError::Busy)?;
        let control = self.control.take().ok_or(StreamError::Busy)?;
        self.hold = plan.hold.take();
        // Only the VP9 alpha codec keeps alpha; any other ring codec gets the picture as it is (D9).
        let alpha = if plan.codec == CodecId::Vp9Alpha { AlphaKind::Straight } else { AlphaKind::None };
        let video = VideoSpec { size: cfg.size, fps: cfg.fps, alpha };
        let encode = EncodeConfig::new(video, plan.codec)?;
        let core = ReplayCore::new(plan.settings, encode, &plan.cache, plan.tracks, plan.encoders)?;
        let shared = Arc::clone(&self.handle.shared);
        {
            let mut status = lock(&shared.status);
            status.length_s = plan.settings.length.0;
            status.cap_bytes = plan.settings.max_bytes;
            status.backing = Some(plan.settings.backing);
        }
        let (frames_tx, frames_rx) = sync_channel::<FrameMsg>(QUEUE_FRAMES);
        let (free_tx, free_rx) = sync_channel::<Image>(QUEUE_FRAMES);
        for _ in 0..QUEUE_FRAMES {
            let empty = Image { size: UVec2::ZERO, alpha: AlphaEncoding::Straight, rgba8: Vec::new() };
            free_tx.try_send(empty).map_err(|_| StreamError::Io("the frame slots could not be made".into()))?;
        }
        let (jobs_tx, jobs_rx) = sync_channel::<WriteJob>(SAVE_QUEUE);
        let target = plan.target;
        let writer_shared = Arc::clone(&shared);
        let writer = std::thread::Builder::new()
            .name("omuzaki-replay-save".into())
            .spawn(move || {
                for job in jobs_rx {
                    let result = write_clip(&job.sel, &job.spec, &job.tracks, &target);
                    // The window's payloads go before the reply, so the meter is down when the caller hears back.
                    drop(job.sel);
                    writer_shared.end_save(&result);
                    let _ = job.reply.send(result);
                }
            })
            .map_err(|e| StreamError::Io(format!("the replay save thread could not start: {e}")))?;
        let worker_shared = Arc::clone(&shared);
        let worker = std::thread::Builder::new()
            .name("omuzaki-replay-encode".into())
            .spawn(move || encode_loop(core, &frames_rx, &control, &free_tx, &jobs_tx, &worker_shared))
            .map_err(|e| StreamError::Io(format!("the replay encoder thread could not start: {e}")))?;
        shared.running.store(true, Ordering::Release);
        self.frames = Some(frames_tx);
        self.free = Some(free_rx);
        self.worker = Some(worker);
        self.writer = Some(writer);
        Ok(())
    }
}

/// The encoder thread: frames into the core, saves fixed between two frames.
fn encode_loop(
    mut core: ReplayCore,
    frames: &Receiver<FrameMsg>,
    control: &Receiver<SaveCommand>,
    free: &SyncSender<Image>,
    jobs: &SyncSender<WriteJob>,
    shared: &Shared,
) {
    loop {
        while let Ok(save) = control.try_recv() {
            fix_window(&core, save, jobs, shared);
        }
        match frames.recv_timeout(IDLE_POLL) {
            Ok(msg) => {
                shared.queued.fetch_sub(1, Ordering::Relaxed);
                let frame = Frame::new(msg.image.size, msg.index, msg.capture).with_cpu(&msg.image);
                let pushed = core.push_frame(&frame);
                // The slot channel has room for every slot, so returning one never fails while the output lives.
                let _ = free.try_send(msg.image);
                let ring = core.stats();
                let mut status = lock(&shared.status);
                status.buffered_s = ring.span.0;
                status.bytes = core.bytes();
                status.frames = core.frames();
                status.encoder_restarts = core.restarts();
                status.refused = ring.refused;
                if let Err(e) = pushed {
                    status.error = Some(e.to_string());
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    shared.running.store(false, Ordering::Release);
    while let Ok(save) = control.try_recv() {
        fix_window(&core, save, jobs, shared);
    }
}

/// Fixes a save's window now and hands it to the writer.
fn fix_window(core: &ReplayCore, save: SaveCommand, jobs: &SyncSender<WriteJob>, shared: &Shared) {
    if core.is_empty() {
        let result = Err(ReplayError::Empty);
        shared.end_save(&result);
        let _ = save.reply.send(result);
        return;
    }
    let sel = core.select(save.length.unwrap_or(core.settings().length));
    let job = WriteJob { sel, spec: core.spec(), tracks: core.tracks(), reply: save.reply };
    // At most `SAVE_QUEUE` saves are outstanding and the queue holds that many, so this never waits.
    if let Err(e) = jobs.send(job) {
        let job = e.0;
        let result = Err(ReplayError::Disabled);
        shared.end_save(&result);
        let _ = job.reply.send(result);
    }
}

impl Output for ReplayBuffer {
    fn info(&self) -> OutputInfo {
        OutputInfo::new(KIND, AlphaNeed::Straight, Lanes { gpu: None, cpu: true })
    }

    fn start(&mut self, cfg: &OutputConfig) -> Result<(), StreamError> {
        if self.state == OutputState::Running {
            return Err(StreamError::Busy);
        }
        if let Err(e) = self.launch(cfg) {
            self.state = OutputState::Failed;
            self.last_error = Some(e.clone());
            self.hold = None;
            return Err(e);
        }
        self.state = OutputState::Running;
        Ok(())
    }

    fn submit(&mut self, frame: &Frame<'_>) -> Result<SubmitReport, StreamError> {
        if self.state != OutputState::Running {
            return Err(StreamError::Stopped);
        }
        let started = Instant::now();
        let image = frame.cpu.ok_or_else(|| StreamError::Config("the replay buffer reads the CPU image".into()))?;
        let (Some(free), Some(frames)) = (self.free.as_ref(), self.frames.as_ref()) else {
            return Err(StreamError::Stopped);
        };
        self.submitted += 1;
        let shared = &self.handle.shared;
        let Ok(mut slot) = free.try_recv() else {
            shared.dropped.fetch_add(1, Ordering::Relaxed);
            return Ok(SubmitReport::new(started.elapsed().as_secs_f32() * 1000.0, 0.0, 1));
        };
        slot.size = image.size;
        slot.alpha = image.alpha;
        slot.rgba8.clear();
        slot.rgba8.extend_from_slice(&image.rgba8);
        shared.queued.fetch_add(1, Ordering::Relaxed);
        if frames.try_send(FrameMsg { index: frame.index, capture: frame.capture_time, image: slot }).is_err() {
            shared.queued.fetch_sub(1, Ordering::Relaxed);
            let e = StreamError::Encode("the replay encoder thread stopped".into());
            self.last_error = Some(e.clone());
            self.state = OutputState::Failed;
            return Err(e);
        }
        Ok(SubmitReport::new(started.elapsed().as_secs_f32() * 1000.0, 0.0, 0))
    }

    fn status(&self) -> OutputStatus {
        let shared = &self.handle.shared;
        let mut status = OutputStatus::default();
        status.state = self.state;
        status.frames = lock(&shared.status).frames;
        status.dropped = shared.dropped.load(Ordering::Relaxed);
        #[allow(clippy::cast_precision_loss)] // At most `QUEUE_FRAMES`.
        let fill = shared.queued.load(Ordering::Relaxed) as f32 / QUEUE_FRAMES as f32;
        status.queue_fill = fill;
        status.last_error.clone_from(&self.last_error);
        status
    }

    fn stop(&mut self) {
        // Closing the frame queue ends the encoder thread, which fixes any save still asked and closes the writer's
        // queue; the writer finishes the saves it holds.
        self.frames = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
        self.free = None;
        self.hold = None;
        self.handle.shared.running.store(false, Ordering::Release);
        if matches!(self.state, OutputState::Running) {
            self.state = OutputState::Stopped;
        }
    }
}

impl Drop for ReplayBuffer {
    fn drop(&mut self) {
        self.stop();
    }
}
