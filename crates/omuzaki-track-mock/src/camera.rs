//! `FakeCamera`: one camera whose frames and timestamps are known exactly.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use omuzaki_track_api::{
    Camera, CameraFormat, CameraFormatRequest, CameraFrame, CameraId, CameraInfo, CameraStream, DeviceTime,
    PixelFormat, TrackError,
};

#[derive(Clone)]
enum Script {
    /// Given bytes and timestamps, served in order.
    Frames(Arc<[(DeviceTime, Vec<u8>)]>),
    /// So many generated test-pattern frames at the format's rate.
    Synthetic(u64),
}

/// A camera with one fixed format. A stream serves its frames in order without waiting (`timeout` is ignored), then
/// fails with [`TrackError::Lost`], as an unplugged camera does. Only one stream may be open at a time
/// ([`TrackError::Busy`] otherwise).
pub struct FakeCamera {
    info: CameraInfo,
    script: Script,
    in_use: Arc<AtomicBool>,
}

impl FakeCamera {
    /// The id it enumerates under.
    pub const ID: &'static str = "fake:0";

    /// [`Self::ID`] as a [`CameraId`].
    pub fn id() -> CameraId {
        CameraId(Self::ID.to_owned())
    }

    /// Serves `frames` (capture time, bytes laid out tightly in `format`) in order.
    pub fn from_frames(format: CameraFormat, frames: Vec<(DeviceTime, Vec<u8>)>) -> Self {
        Self::new(format, Script::Frames(frames.into()))
    }

    /// Serves `frames` test-pattern frames at `format.fps`, frame `i` captured at exactly `i / fps` seconds. Every
    /// layout but [`PixelFormat::Mjpg`] (which fails to open with [`TrackError::FormatUnsupported`]); the luma of
    /// each pixel is [`synthetic_luma`](Self::synthetic_luma), chroma is neutral.
    pub fn synthetic(format: CameraFormat, frames: u64) -> Self {
        Self::new(format, Script::Synthetic(frames))
    }

    /// The test pattern's value at `(x, y)` in frame `i`: a diagonal ramp that moves one step per frame.
    #[allow(clippy::cast_possible_truncation)] // the ramp wraps by design
    pub fn synthetic_luma(x: u32, y: u32, i: u64) -> u8 {
        (u64::from(x) + u64::from(y) + i) as u8
    }

    fn new(format: CameraFormat, script: Script) -> Self {
        let info = CameraInfo { id: Self::id(), name: "Fake camera".to_owned(), formats: vec![format] };
        Self { info, script, in_use: Arc::new(AtomicBool::new(false)) }
    }
}

impl Camera for FakeCamera {
    fn enumerate(&self) -> Vec<CameraInfo> {
        vec![self.info.clone()]
    }

    fn open(&mut self, id: &CameraId, request: &CameraFormatRequest) -> Result<Box<dyn CameraStream>, TrackError> {
        if *id != self.info.id {
            return Err(TrackError::NoDevice(id.clone()));
        }
        let format = request.pick(&self.info.formats).ok_or(TrackError::FormatUnsupported(*request))?;
        let synthetic = matches!(self.script, Script::Synthetic(_));
        if synthetic && format.pixel == PixelFormat::Mjpg {
            return Err(TrackError::FormatUnsupported(*request));
        }
        if self.in_use.swap(true, Ordering::AcqRel) {
            return Err(TrackError::Busy(id.clone()));
        }
        let stride = format.pixel.tight_stride(format.width).unwrap_or(0);
        let len = format.pixel.frame_len(format.height, stride).unwrap_or(0);
        let buffer = if synthetic { vec![0; len] } else { Vec::new() };
        Ok(Box::new(FakeStream {
            format,
            stride,
            len,
            script: self.script.clone(),
            next: 0,
            buffer,
            in_use: Arc::clone(&self.in_use),
        }))
    }
}

struct FakeStream {
    format: CameraFormat,
    stride: usize,
    len: usize,
    script: Script,
    next: u64,
    /// The one buffer synthetic frames are drawn into, so the stream allocates nothing per frame.
    buffer: Vec<u8>,
    in_use: Arc<AtomicBool>,
}

impl CameraStream for FakeStream {
    fn format(&self) -> CameraFormat {
        self.format
    }

    fn next_frame(&mut self, _timeout: Duration) -> Result<Option<CameraFrame<'_>>, TrackError> {
        let i = self.next;
        let (captured, data) = match &self.script {
            Script::Frames(frames) => {
                let (time, data) = usize::try_from(i).ok().and_then(|i| frames.get(i)).ok_or(TrackError::Lost)?;
                if data.len() < self.len {
                    return Err(TrackError::Backend(format!("frame {i} has {} bytes, not {}", data.len(), self.len)));
                }
                (*time, data.as_slice())
            }
            Script::Synthetic(count) => {
                if i >= *count {
                    return Err(TrackError::Lost);
                }
                draw(&mut self.buffer, self.format, self.stride, i);
                let nanos = u128::from(i) * 1_000_000_000 / u128::from(self.format.fps.max(1));
                (DeviceTime(u64::try_from(nanos).unwrap_or(u64::MAX)), self.buffer.as_slice())
            }
        };
        self.next += 1;
        let f = self.format;
        Ok(Some(CameraFrame {
            data,
            width: f.width,
            height: f.height,
            stride: self.stride,
            pixel: f.pixel,
            captured,
            seq: i,
        }))
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.in_use.store(false, Ordering::Release);
    }
}

/// Draws test-pattern frame `i` into `buf` (sized by [`PixelFormat::frame_len`]).
fn draw(buf: &mut [u8], format: CameraFormat, stride: usize, i: u64) {
    let (w, h) = (format.width as usize, format.height as usize);
    for (y, row) in buf.chunks_mut(stride.max(1)).take(h).enumerate() {
        for x in 0..w {
            #[allow(clippy::cast_possible_truncation)] // x, y < u32::MAX by construction
            let v = FakeCamera::synthetic_luma(x as u32, y as u32, i);
            match format.pixel {
                PixelFormat::Rgb8 => row[3 * x..3 * x + 3].copy_from_slice(&[v, v, v]),
                PixelFormat::Bgra8 => row[4 * x..4 * x + 4].copy_from_slice(&[v, v, v, 255]),
                PixelFormat::Yuy2 => row[2 * x..2 * x + 2].copy_from_slice(&[v, 128]),
                PixelFormat::Nv12 => row[x] = v,
                PixelFormat::Mjpg => {}
            }
        }
    }
    if format.pixel == PixelFormat::Nv12 {
        buf[h * stride..].fill(128);
    }
}
