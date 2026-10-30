//! Video files as a tracking input (#2585): re-track a recorded webcam clip or a phone video of oneself.
//!
//! | Module | What it does |
//! |---|---|
//! | [`y4m`] | the YUV4MPEG2 reader: a `.y4m` file, and the pipe ffmpeg writes |
//! | [`ffmpeg`] | every other format, decoded by the person's own `ffmpeg` as a separate program |
//! | [`camera`] | [`VideoCamera`](camera::VideoCamera): a track-api `Camera` that plays added files at their own rate |
//!
//! [`VideoReader`] reads a file's frames as fast as they decode, for tracking a clip faster than real time; the camera
//! paces the same reader to the file's rate, so the webcam tracker, its preview, calibration and doctor track a file
//! unchanged. Frames are NV12 [`Picture`]s, what cameras send; reading allocates nothing once the picture has its
//! size. The file is only read, on this machine: nothing is uploaded.

pub mod camera;
pub mod ffmpeg;
pub mod y4m;

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::Duration;

use ffmpeg::{Decoding, Ffmpeg};
use y4m::Y4mReader;

/// Why a video could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum VideoError {
    /// No file at the path.
    #[error("no video file at {}", .0.display())]
    Missing(PathBuf),
    /// The stream's layout is not one this reader takes.
    #[error("{0}")]
    Format(String),
    /// Reading failed, or the stream broke off inside a frame.
    #[error("reading the video failed: {0}")]
    Io(String),
    /// The stream ended before its first frame.
    #[error("the video has no frames")]
    Empty,
    /// The file needs ffmpeg and it did not start.
    #[error(
        "Omuzaki reads .y4m videos itself and every other format through ffmpeg, which did not start: install it from \
         ffmpeg.org, or name the program in {}",
        ffmpeg::PROGRAM_ENV
    )]
    NoFfmpeg,
    /// ffmpeg ran and could not read the file; its own words.
    #[error("ffmpeg could not read the video: {0}")]
    Decode(String),
}

impl From<std::io::Error> for VideoError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

/// A video's picture size and frame rate.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct VideoInfo {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// The frame rate's numerator: `fps_num / fps_den` frames per second.
    pub fps_num: u32,
    /// The frame rate's denominator, never 0.
    pub fps_den: u32,
}

impl VideoInfo {
    /// `width` x `height` pixels at `fps_num / fps_den` frames per second (a 0 denominator reads as 1).
    pub const fn new(width: u32, height: u32, fps_num: u32, fps_den: u32) -> Self {
        Self { width, height, fps_num, fps_den: if fps_den == 0 { 1 } else { fps_den } }
    }

    /// Frames per second.
    pub fn fps(&self) -> f64 {
        f64::from(self.fps_num) / f64::from(self.fps_den)
    }

    /// The time from one frame to the next, at least a microsecond.
    pub fn period(&self) -> Duration {
        let nanos = u64::from(self.fps_den) * 1_000_000_000 / u64::from(self.fps_num.max(1));
        Duration::from_nanos(nanos.max(1000))
    }
}

/// One frame as NV12: a `height`-row Y plane, then `ceil(height / 2)` rows of interleaved U, V pairs, both `stride`
/// bytes apart. The stride is the width rounded up to even, so an odd width keeps its last chroma pair.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Picture {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bytes from one row to the next.
    pub stride: usize,
    /// The planes.
    pub data: Vec<u8>,
}

impl Picture {
    /// Sizes the picture for `width` x `height`, reusing its buffer.
    pub fn resize(&mut self, width: u32, height: u32) {
        let (w, h) = (width as usize, height as usize);
        self.width = width;
        self.height = height;
        self.stride = 2 * w.div_ceil(2);
        self.data.resize(self.stride * (h + h.div_ceil(2)), 0);
    }

    /// Whether `other` has the same size and visible pixels (padding aside).
    pub fn same_pixels(&self, other: &Self) -> bool {
        if (self.width, self.height) != (other.width, other.height) {
            return false;
        }
        let (w, h) = (self.width as usize, self.height as usize);
        let rows = |p: &Self| -> Vec<u8> {
            let mut out = Vec::new();
            for y in 0..h {
                out.extend_from_slice(&p.data[y * p.stride..y * p.stride + w]);
            }
            let chroma = &p.data[p.stride * h..];
            for y in 0..h.div_ceil(2) {
                out.extend_from_slice(&chroma[y * p.stride..y * p.stride + 2 * w.div_ceil(2)]);
            }
            out
        };
        rows(self) == rows(other)
    }
}

/// The frames of one video file, read as fast as they decode.
#[derive(Debug)]
pub struct VideoReader {
    source: Source,
    info: VideoInfo,
    path: PathBuf,
}

#[derive(Debug)]
enum Source {
    Y4m(Y4mReader<BufReader<File>>),
    Ffmpeg(Decoding),
}

impl VideoReader {
    /// Opens the video at `path`: a `.y4m` stream is read directly, anything else through `ffmpeg`. Cold: starting
    /// ffmpeg and reading its header takes tens of milliseconds.
    ///
    /// # Errors
    /// [`VideoError::Missing`] when `path` is no file; what [`Y4mReader::new`] and ffmpeg return otherwise.
    pub fn open(path: &Path, ffmpeg: &Ffmpeg) -> Result<Self, VideoError> {
        let missing = || VideoError::Missing(path.to_path_buf());
        let path = path.canonicalize().map_err(|_| missing())?;
        if !path.is_file() {
            return Err(missing());
        }
        let mut file = BufReader::with_capacity(1 << 20, File::open(&path)?);
        if file.fill_buf()?.starts_with(y4m::MAGIC) {
            let reader = Y4mReader::new(file)?;
            return Ok(Self { info: reader.info(), source: Source::Y4m(reader), path });
        }
        drop(file);
        let probe = ffmpeg.decode(&path, None)?;
        let found = probe.info().ok_or(VideoError::Empty)?;
        let size = ffmpeg::fit(found.width, found.height, ffmpeg::MAX_LONG_SIDE);
        let decoding = if size == (found.width, found.height) {
            probe
        } else {
            drop(probe);
            ffmpeg.decode(&path, Some(size))?
        };
        let info = decoding.info().ok_or(VideoError::Empty)?;
        Ok(Self { info, source: Source::Ffmpeg(decoding), path })
    }

    /// The decoded pictures' size and the file's frame rate.
    pub fn info(&self) -> VideoInfo {
        self.info
    }

    /// The file, as an absolute path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the next frame into `out`. `false` at the end of the file.
    ///
    /// # Errors
    /// [`VideoError::Io`] or [`VideoError::Decode`] when the file breaks off or ffmpeg fails partway.
    pub fn read(&mut self, out: &mut Picture) -> Result<bool, VideoError> {
        match &mut self.source {
            Source::Y4m(reader) => reader.read(out),
            Source::Ffmpeg(decoding) => decoding.read(out),
        }
    }
}
