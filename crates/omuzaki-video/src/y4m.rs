//! YUV4MPEG2 (`.y4m`): a text header, then each frame as `FRAME` and its raw planes. It is what `ffmpeg -f
//! yuv4mpegpipe` writes, so the same reader serves a `.y4m` file and every format ffmpeg decodes.
//!
//! Only 4:2:0 is read (`C420jpeg`, `C420mpeg2`, `C420paldv`, `C420`, or no `C` tag): the tracker reads NV12, and ffmpeg
//! is always asked for 4:2:0. Each frame's planes are read straight into an NV12 [`Picture`], the chroma through one
//! reused row, so reading allocates nothing once the picture has its size.

use std::io::{self, BufRead, Read};

use crate::{Picture, VideoError, VideoInfo};

/// The magic every stream starts with.
pub const MAGIC: &[u8] = b"YUV4MPEG2 ";

/// The longest header line read, in bytes: real headers are under 100.
const MAX_HEADER: usize = 1024;

/// The largest width or height accepted: far above any camera or phone, low enough that a frame's size cannot
/// overflow.
pub const MAX_SIDE: u32 = 16_384;

/// A Y4M stream over `R`.
#[derive(Debug)]
pub struct Y4mReader<R> {
    input: R,
    info: VideoInfo,
    /// One chroma plane's row, reused.
    row: Vec<u8>,
    /// One header line, reused.
    line: Vec<u8>,
}

impl<R: BufRead> Y4mReader<R> {
    /// Reads the stream header from `input`.
    ///
    /// # Errors
    /// [`VideoError::Format`] for a stream that is not 4:2:0 Y4M or has a bad header; [`VideoError::Io`] when reading
    /// fails; [`VideoError::Empty`] when `input` ends before a header.
    pub fn new(mut input: R) -> Result<Self, VideoError> {
        let mut line = Vec::new();
        if read_line(&mut input, &mut line)? == 0 {
            return Err(VideoError::Empty);
        }
        let info = parse_header(&line)?;
        Ok(Self { input, info, row: Vec::new(), line })
    }

    /// The stream's size and rate.
    pub fn info(&self) -> VideoInfo {
        self.info
    }

    /// Reads the next frame into `out`, sized to the stream. `false` at the end of the stream.
    ///
    /// # Errors
    /// [`VideoError::Format`] for a frame without its `FRAME` line; [`VideoError::Io`] when the stream breaks off
    /// inside a frame or reading fails.
    pub fn read(&mut self, out: &mut Picture) -> Result<bool, VideoError> {
        if read_line(&mut self.input, &mut self.line)? == 0 {
            return Ok(false);
        }
        if !self.line.starts_with(b"FRAME") {
            return Err(VideoError::Format("a frame does not start with FRAME".into()));
        }
        let (width, height) = (self.info.width as usize, self.info.height as usize);
        out.resize(self.info.width, self.info.height);
        let stride = out.stride;
        let (luma, chroma) = out.data.split_at_mut(stride * height);
        for y in 0..height {
            self.input.read_exact(&mut luma[y * stride..y * stride + width])?;
        }
        // The planes are U then V, each `cw` x `ch`: interleave them into NV12's one UV plane.
        let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
        self.row.resize(cw, 0);
        for offset in [0, 1] {
            for y in 0..ch {
                self.input.read_exact(&mut self.row)?;
                let dst = &mut chroma[y * stride..y * stride + 2 * cw];
                for (pair, &v) in dst.chunks_exact_mut(2).zip(&self.row) {
                    pair[offset] = v;
                }
            }
        }
        Ok(true)
    }
}

/// Reads one `\n`-ended line into `line`, without the `\n`, refusing one longer than [`MAX_HEADER`]. Returns the bytes
/// read, 0 at the end.
fn read_line(input: &mut impl BufRead, line: &mut Vec<u8>) -> Result<usize, VideoError> {
    line.clear();
    let read = input.take(MAX_HEADER as u64 + 1).read_until(b'\n', line)?;
    if line.last() == Some(&b'\n') {
        line.pop();
    } else if read > MAX_HEADER {
        return Err(VideoError::Format(format!("a header line is longer than {MAX_HEADER} bytes")));
    } else if read > 0 {
        return Err(VideoError::Io(io::Error::from(io::ErrorKind::UnexpectedEof).to_string()));
    }
    Ok(read)
}

/// Parses `YUV4MPEG2 W.. H.. F..:.. ...`.
fn parse_header(line: &[u8]) -> Result<VideoInfo, VideoError> {
    let bad = |what: &str| VideoError::Format(format!("not a Y4M video: {what}"));
    let rest = line.strip_prefix(MAGIC).ok_or_else(|| bad("no YUV4MPEG2 header"))?;
    let text = std::str::from_utf8(rest).map_err(|_| bad("the header is not text"))?;
    let (mut width, mut height, mut rate) = (None, None, None);
    for tag in text.split(' ').filter(|t| !t.is_empty()) {
        let (key, value) = tag.split_at(1);
        match key {
            "W" => width = value.parse::<u32>().ok(),
            "H" => height = value.parse::<u32>().ok(),
            "F" => {
                rate = value.split_once(':').and_then(|(n, d)| Some((n.parse::<u32>().ok()?, d.parse::<u32>().ok()?)));
            }
            "C" if !value.starts_with("420") => {
                return Err(bad(&format!("colour layout {value} (only 4:2:0 is read; ffmpeg converts the rest)")));
            }
            "I" if !matches!(value, "p" | "?") => return Err(bad("interlaced frames")),
            _ => {}
        }
    }
    let width = width.filter(|w| (1..=MAX_SIDE).contains(w)).ok_or_else(|| bad("no width, or one out of range"))?;
    let height = height.filter(|h| (1..=MAX_SIDE).contains(h)).ok_or_else(|| bad("no height, or one out of range"))?;
    let (num, den) = rate.filter(|&(n, d)| n > 0 && d > 0).ok_or_else(|| bad("no frame rate"))?;
    Ok(VideoInfo::new(width, height, num, den))
}

/// Writes `picture` as one Y4M frame, the inverse of [`Y4mReader::read`]; with `header`, the stream header first. For
/// tests and fixtures.
///
/// # Errors
/// Whatever writing to `out` returns.
pub fn write_frame(out: &mut impl io::Write, picture: &Picture, info: Option<VideoInfo>) -> io::Result<()> {
    if let Some(info) = info {
        writeln!(
            out,
            "YUV4MPEG2 W{} H{} F{}:{} Ip A1:1 C420jpeg",
            info.width, info.height, info.fps_num, info.fps_den
        )?;
    }
    out.write_all(b"FRAME\n")?;
    let (width, height, stride) = (picture.width as usize, picture.height as usize, picture.stride);
    for y in 0..height {
        out.write_all(&picture.data[y * stride..y * stride + width])?;
    }
    let (cw, ch) = (width.div_ceil(2), height.div_ceil(2));
    let chroma = &picture.data[stride * height..];
    for offset in [0, 1] {
        for y in 0..ch {
            let row: Vec<u8> =
                chroma[y * stride..y * stride + 2 * cw].iter().skip(offset).step_by(2).copied().collect();
            out.write_all(&row)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn picture(width: u32, height: u32, seed: u8) -> Picture {
        let mut p = Picture::default();
        p.resize(width, height);
        for (i, b) in p.data.iter_mut().enumerate() {
            *b = seed.wrapping_add((i % 251) as u8);
        }
        p
    }

    #[test]
    fn frames_come_back_as_written_even_at_odd_sizes() {
        for (w, h) in [(4, 2), (5, 3), (64, 36)] {
            let info = VideoInfo::new(w, h, 30_000, 1001);
            let (a, b) = (picture(w, h, 1), picture(w, h, 99));
            let mut bytes = Vec::new();
            write_frame(&mut bytes, &a, Some(info)).unwrap();
            write_frame(&mut bytes, &b, None).unwrap();
            let mut reader = Y4mReader::new(Cursor::new(bytes)).unwrap();
            assert_eq!(reader.info(), info);
            let mut out = Picture::default();
            for want in [&a, &b] {
                assert!(reader.read(&mut out).unwrap());
                assert!(out.same_pixels(want), "{w}x{h}");
            }
            assert!(!reader.read(&mut out).unwrap(), "the stream ends");
        }
    }

    #[test]
    fn a_bad_header_says_what_is_wrong() {
        let refused = |header: &str| match Y4mReader::new(Cursor::new(header.as_bytes().to_vec())) {
            Err(VideoError::Format(why)) => why,
            other => panic!("{header:?} gave {other:?}"),
        };
        assert!(refused("RIFF....\n").contains("no YUV4MPEG2"));
        assert!(refused("YUV4MPEG2 W640 H480 F30:1 C444\n").contains("4:2:0"));
        assert!(refused("YUV4MPEG2 W640 F30:1\n").contains("height"));
        assert!(refused("YUV4MPEG2 W640 H480 F30:0\n").contains("frame rate"));
        assert!(refused("YUV4MPEG2 W99999 H480 F30:1\n").contains("width"));
        assert!(matches!(Y4mReader::new(Cursor::new(Vec::new())), Err(VideoError::Empty)));
    }

    #[test]
    fn a_stream_cut_inside_a_frame_is_an_error_not_an_end() {
        let info = VideoInfo::new(8, 8, 25, 1);
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &picture(8, 8, 0), Some(info)).unwrap();
        bytes.truncate(bytes.len() - 5);
        let mut reader = Y4mReader::new(Cursor::new(bytes)).unwrap();
        assert!(matches!(reader.read(&mut Picture::default()), Err(VideoError::Io(_))));
    }
}
