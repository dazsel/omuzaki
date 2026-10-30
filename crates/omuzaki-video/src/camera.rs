//! [`VideoCamera`]: video files as cameras. It wraps the platform's camera, if there is one, and lists each added file
//! beside its devices as `video:<path>`; opening one plays the file at its own rate.
//!
//! A file plays as a live camera would: frame `n` is due `n` periods after the stream opened, and its device time is
//! `n` periods, so the face and body move at the speed they were filmed. When tracking falls behind, the frames it
//! missed are read and dropped (the newest frame wins, as at a camera), and the gap shows in the frame counter. At the
//! end the file starts again, its frames still counting up, so the preview keeps running until it is stopped.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use omuzaki_track_api::camera::{
    Camera, CameraFormat, CameraFormatRequest, CameraFrame, CameraId, CameraInfo, CameraStream, DeviceTime, PixelFormat,
};
use omuzaki_track_api::error::TrackError;

use crate::ffmpeg::Ffmpeg;
use crate::{Picture, VideoError, VideoInfo, VideoReader};

/// What every file camera's id starts with.
pub const ID_PREFIX: &str = "video:";

/// The most frames one call reads and drops to catch up: a stalled tracker never makes a call read a whole file.
const MAX_SKIP: u64 = 30;

/// A file added as a camera.
#[derive(Clone, Debug)]
struct VideoFile {
    info: CameraInfo,
    path: PathBuf,
}

/// The platform's cameras plus video files.
pub struct VideoCamera {
    platform: Option<Box<dyn Camera>>,
    files: Vec<VideoFile>,
    ffmpeg: Ffmpeg,
}

impl std::fmt::Debug for VideoCamera {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VideoCamera")
            .field("platform", &self.platform.is_some())
            .field("files", &self.files)
            .field("ffmpeg", &self.ffmpeg)
            .finish()
    }
}

impl VideoCamera {
    /// The cameras of `platform` (none when `None`) plus the files added later, decoded through `ffmpeg`.
    pub fn new(platform: Option<Box<dyn Camera>>, ffmpeg: Ffmpeg) -> Self {
        Self { platform, files: Vec::new(), ffmpeg }
    }

    /// Adds the video at `path` (see [`Camera::add_file`]).
    ///
    /// # Errors
    /// What [`VideoReader::open`] returns.
    pub fn add(&mut self, path: &Path) -> Result<CameraInfo, VideoError> {
        let reader = VideoReader::open(path, &self.ffmpeg)?;
        let path = reader.path().to_path_buf();
        if let Some(known) = self.files.iter().find(|f| f.path == path) {
            return Ok(known.info.clone());
        }
        let info = camera_info(&path, reader.info());
        self.files.push(VideoFile { info: info.clone(), path });
        Ok(info)
    }
}

/// The camera a file at `path` reads as.
fn camera_info(path: &Path, video: VideoInfo) -> CameraInfo {
    let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
    // The nominal rate rounded to whole frames: 29.97 lists as 30; frame times keep the exact rate.
    let fps = (video.fps_num + video.fps_den / 2) / video.fps_den;
    let fps = fps.max(1);
    let format = CameraFormat::new(video.width, video.height, fps, PixelFormat::Nv12);
    CameraInfo::new(CameraId(format!("{ID_PREFIX}{}", path.display())), format!("{name} (video)"), vec![format])
}

/// A file failure as a camera failure.
fn track_error(id: &CameraId, error: VideoError) -> TrackError {
    match error {
        VideoError::Missing(_) => TrackError::NoDevice(id.clone()),
        other => TrackError::Backend(other.to_string()),
    }
}

impl Camera for VideoCamera {
    fn enumerate(&self) -> Vec<CameraInfo> {
        let mut all = self.platform.as_ref().map(|p| p.enumerate()).unwrap_or_default();
        all.extend(self.files.iter().map(|f| f.info.clone()));
        all
    }

    fn open(&mut self, id: &CameraId, request: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
        if let Some(file) = self.files.iter().find(|f| f.info.id == *id) {
            let format = request.pick(&file.info.formats).ok_or(TrackError::FormatUnsupported(*request))?;
            let stream =
                VideoStream::open(file.path.clone(), self.ffmpeg.clone(), format).map_err(|e| track_error(id, e))?;
            return Ok(Box::new(stream));
        }
        match self.platform.as_mut() {
            Some(platform) if !id.0.starts_with(ID_PREFIX) => platform.open(id, request),
            _ => Err(TrackError::NoDevice(id.clone())),
        }
    }

    fn add_file(&mut self, path: &Path) -> Result<CameraInfo, TrackError> {
        self.add(path).map_err(|e| match e {
            VideoError::Missing(_) => TrackError::NoDevice(CameraId(format!("{ID_PREFIX}{}", path.display()))),
            other => TrackError::Backend(other.to_string()),
        })
    }
}

/// A file playing at its own rate.
#[derive(Debug)]
pub struct VideoStream {
    reader: VideoReader,
    path: PathBuf,
    ffmpeg: Ffmpeg,
    format: CameraFormat,
    picture: Picture,
    period: Duration,
    /// When frame 0 was due.
    start: Instant,
    /// The next frame's number, counted across loops.
    next: u64,
    /// Whether the last pass read any frame: a file that ends at once ends the stream rather than looping for ever.
    read_this_pass: bool,
}

impl VideoStream {
    /// Opens `path` to play in `format`, frame 0 due now.
    ///
    /// # Errors
    /// What [`VideoReader::open`] returns.
    pub fn open(path: PathBuf, ffmpeg: Ffmpeg, format: CameraFormat) -> Result<Self, VideoError> {
        let reader = VideoReader::open(&path, &ffmpeg)?;
        let period = reader.info().period();
        Ok(Self {
            reader,
            path,
            ffmpeg,
            format,
            picture: Picture::default(),
            period,
            start: Instant::now(),
            next: 0,
            read_this_pass: false,
        })
    }

    /// When frame `n` is due.
    fn due(&self, n: u64) -> Instant {
        let nanos = self.period.as_nanos().saturating_mul(u128::from(n));
        self.start + Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
    }

    /// Reads the next frame of the file into the picture, starting the file again at its end.
    fn read_one(&mut self) -> Result<(), TrackError> {
        let backend = |e: VideoError| TrackError::Backend(e.to_string());
        if self.reader.read(&mut self.picture).map_err(backend)? {
            self.read_this_pass = true;
            return Ok(());
        }
        if !self.read_this_pass {
            return Err(TrackError::Lost);
        }
        // The end: play it again. Starting ffmpeg again allocates, once a pass.
        self.reader = VideoReader::open(&self.path, &self.ffmpeg).map_err(backend)?;
        self.read_this_pass = false;
        if self.reader.read(&mut self.picture).map_err(backend)? {
            self.read_this_pass = true;
            Ok(())
        } else {
            Err(TrackError::Lost)
        }
    }
}

impl CameraStream for VideoStream {
    fn format(&self) -> CameraFormat {
        self.format
    }

    fn next_frame(&mut self, timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
        let now = Instant::now();
        let due = self.due(self.next);
        if due > now {
            let wait = due - now;
            if wait > timeout {
                std::thread::sleep(timeout);
                return Ok(None);
            }
            std::thread::sleep(wait);
        } else {
            // Behind: drop the frames whose successors are already due, as many as one call may.
            let mut skipped = 0;
            while skipped < MAX_SKIP && self.due(self.next + 1) <= now {
                self.read_one()?;
                self.next += 1;
                skipped += 1;
            }
        }
        self.read_one()?;
        let n = self.next;
        self.next += 1;
        let captured =
            DeviceTime(u64::try_from(self.period.as_nanos().saturating_mul(u128::from(n))).unwrap_or(u64::MAX));
        let p = &self.picture;
        Ok(Some(CameraFrame::new(&p.data, p.width, p.height, p.stride, PixelFormat::Nv12, captured, n)))
    }
}

#[cfg(test)]
mod tests {
    use std::fs::File;
    use std::io::BufWriter;

    use super::*;
    use crate::y4m::write_frame;

    /// A `.y4m` file of `frames` frames at `fps`, each frame's luma its own number.
    pub(crate) fn clip(dir: &Path, frames: u8, fps: u32) -> PathBuf {
        let path = dir.join("clip.y4m");
        let mut out = BufWriter::new(File::create(&path).unwrap());
        let info = VideoInfo::new(16, 8, fps, 1);
        for i in 0..frames {
            let mut p = Picture::default();
            p.resize(16, 8);
            p.data.fill(i);
            write_frame(&mut out, &p, (i == 0).then_some(info)).unwrap();
        }
        drop(out);
        path
    }

    #[test]
    fn an_added_file_is_listed_beside_the_platform_and_opens_as_nv12() {
        let dir = tempfile::tempdir().unwrap();
        let path = clip(dir.path(), 3, 30);
        let mut camera = VideoCamera::new(None, Ffmpeg::new("ffmpeg-is-not-needed-for-y4m"));
        assert_eq!(camera.enumerate(), Vec::new());
        let info = camera.add_file(&path).unwrap();
        assert!(info.id.0.starts_with(ID_PREFIX) && info.name == "clip.y4m (video)", "{info:?}");
        assert_eq!(info.formats, vec![CameraFormat::new(16, 8, 30, PixelFormat::Nv12)]);
        assert_eq!(camera.add_file(&path).unwrap(), info, "adding again returns the same camera");
        assert_eq!(camera.enumerate(), vec![info.clone()]);
        let mut stream = camera.open(&info.id, &CameraFormatRequest::default()).unwrap();
        let frame = stream.next_frame(Duration::from_secs(1)).unwrap().unwrap();
        assert_eq!((frame.width, frame.height, frame.pixel, frame.seq), (16, 8, PixelFormat::Nv12, 0));
        assert_eq!(frame.data.len(), PixelFormat::Nv12.frame_len(8, frame.stride).unwrap());
    }

    #[test]
    fn missing_files_and_unknown_ids_are_no_device() {
        let mut camera = VideoCamera::new(None, Ffmpeg::default());
        assert!(matches!(camera.add_file(Path::new("/no/such/clip.mp4")), Err(TrackError::NoDevice(_))));
        let id = CameraId(format!("{ID_PREFIX}/no/such/clip.mp4"));
        assert!(matches!(camera.open(&id, &CameraFormatRequest::default()), Err(TrackError::NoDevice(_))));
    }

    #[test]
    fn frames_play_at_the_files_rate_and_loop_with_their_times_counting_on() {
        let dir = tempfile::tempdir().unwrap();
        let path = clip(dir.path(), 3, 50);
        let mut camera = VideoCamera::new(None, Ffmpeg::default());
        let info = camera.add_file(&path).unwrap();
        let mut stream = camera.open(&info.id, &CameraFormatRequest::default()).unwrap();
        let begun = Instant::now();
        let mut seen = Vec::new();
        while seen.last().is_none_or(|&(seq, _, _)| seq < 6) {
            if let Some(frame) = stream.next_frame(Duration::from_millis(100)).unwrap() {
                seen.push((frame.seq, frame.captured.0, frame.data[0]));
            }
        }
        // Frame 6 at 50 fps is due 120 ms after frame 0.
        assert!(begun.elapsed() >= Duration::from_millis(115), "{:?}", begun.elapsed());
        // A busy machine may drop a frame; whatever arrives is that frame, at that frame's time.
        assert!(seen.windows(2).all(|w| w[0].0 < w[1].0), "{seen:?}");
        for &(seq, captured, luma) in &seen {
            assert_eq!(captured, seq * 20_000_000);
            assert_eq!(u64::from(luma), seq % 3, "the file starts again at its end: {seen:?}");
        }
    }

    #[test]
    fn a_late_reader_gets_the_newest_frame_and_the_gap_shows() {
        let dir = tempfile::tempdir().unwrap();
        let path = clip(dir.path(), 200, 100);
        let mut camera = VideoCamera::new(None, Ffmpeg::default());
        let info = camera.add_file(&path).unwrap();
        let mut stream = camera.open(&info.id, &CameraFormatRequest::default()).unwrap();
        let first = stream.next_frame(Duration::from_millis(100)).unwrap().map(|f| f.seq);
        assert_eq!(first, Some(0));
        // A tracker that took 105 ms over the first frame: frames 1 to 9 are stale by now.
        std::thread::sleep(Duration::from_millis(105));
        let late = stream.next_frame(Duration::from_millis(100)).unwrap().unwrap();
        assert!(late.seq >= 10, "the newest frame wins: {}", late.seq);
        assert_eq!(u64::from(late.data[0]), late.seq, "its pixels are that frame's");
    }
}
