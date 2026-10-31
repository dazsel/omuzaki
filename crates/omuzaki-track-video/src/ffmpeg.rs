//! Decoding through the person's own `ffmpeg`, run as a separate program (like `git`, ADR 0045): nothing links or
//! ships it, so any build the person has works and no licence of it reaches Omuzaki (`docs/licensing/deps/
//! ffmpeg-own-install.toml`).
//!
//! ffmpeg reads the file and writes Y4M to a pipe, which [`Y4mReader`] reads. It is told to read local files only
//! (`-protocol_whitelist file` and a `file:` input, so a playlist cannot reach the network and a path cannot pass for an
//! option), to take the first video stream alone, and to convert to 4:2:0 in BT.601 limited range, what the tracker's
//! NV12 reader assumes. The picture is scaled down to [`MAX_LONG_SIDE`] when larger: the face and body models read
//! far smaller crops, and a 4K phone video would cost six times the bytes for nothing.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, ChildStdout, Command, Stdio};
use std::thread::JoinHandle;

use crate::y4m::Y4mReader;
use crate::{Picture, VideoError, VideoInfo};

/// The environment variable that names an ffmpeg program to run instead of the `ffmpeg` on the `PATH`.
pub const PROGRAM_ENV: &str = "OMUZAKI_FFMPEG";

/// The longest side of a decoded picture, in pixels.
pub const MAX_LONG_SIDE: u32 = 1280;

/// How much of ffmpeg's error output is kept for the error message, in bytes.
const STDERR_TAIL: usize = 600;

/// The ffmpeg program to run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ffmpeg {
    program: OsString,
}

impl Default for Ffmpeg {
    /// The program [`PROGRAM_ENV`] names, or `ffmpeg` on the `PATH`.
    fn default() -> Self {
        Self::new(std::env::var_os(PROGRAM_ENV).filter(|p| !p.is_empty()).unwrap_or_else(|| "ffmpeg".into()))
    }
}

impl Ffmpeg {
    /// Runs `program`.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self { program: program.into() }
    }

    /// Starts decoding the file at the absolute `path`, scaled to `size` when given, and reads the stream header.
    ///
    /// # Errors
    /// [`VideoError::NoFfmpeg`] when the program does not start; [`VideoError::Decode`] with ffmpeg's own words when
    /// it reads no video from the file.
    pub(crate) fn decode(&self, path: &Path, size: Option<(u32, u32)>) -> Result<Decoding, VideoError> {
        let scale = match size {
            Some((w, h)) => format!("scale={w}:{h}:"),
            None => "scale=".to_owned(),
        };
        let mut input = OsString::from("file:");
        input.push(path.as_os_str());
        let mut command = Command::new(&self.program);
        command
            .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-protocol_whitelist", "file", "-i"])
            .arg(input)
            .args(["-map", "0:v:0", "-an", "-sn", "-dn", "-vf"])
            .arg(format!("{scale}out_color_matrix=bt601:out_range=tv,format=yuv420p"))
            .args(["-f", "yuv4mpegpipe", "-"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            /// `CREATE_NO_WINDOW`: no console window flashes up.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|_| VideoError::NoFfmpeg)?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(VideoError::Decode("ffmpeg's output could not be read".into()));
        };
        // Drained on a thread of its own, so a file full of decode warnings cannot fill the pipe and stall ffmpeg.
        let tail = std::thread::Builder::new()
            .name("ffmpeg-stderr".into())
            .spawn(move || keep_tail(stderr))
            .map_err(|e| VideoError::Io(e.to_string()));
        let mut decoding = Decoding { child, reader: None, stderr: None };
        decoding.stderr = Some(tail?);
        match Y4mReader::new(BufReader::with_capacity(1 << 20, stdout)) {
            Ok(reader) => {
                decoding.reader = Some(reader);
                Ok(decoding)
            }
            Err(VideoError::Empty) => Err(VideoError::Decode(decoding.finish())),
            Err(other) => Err(other),
        }
    }
}

/// The last [`STDERR_TAIL`] bytes of `stderr`, as text.
fn keep_tail(stderr: impl Read) -> String {
    let mut tail = String::new();
    for line in BufReader::new(stderr).lines() {
        let Ok(line) = line else { break };
        tail.push_str(line.trim());
        tail.push(' ');
        if tail.len() > STDERR_TAIL {
            let cut = tail.len() - STDERR_TAIL;
            let cut = (cut..tail.len()).find(|&i| tail.is_char_boundary(i)).unwrap_or(tail.len());
            tail.drain(..cut);
        }
    }
    tail.trim().to_owned()
}

/// A running ffmpeg and the Y4M it writes. Dropping it stops ffmpeg.
#[derive(Debug)]
pub(crate) struct Decoding {
    child: Child,
    /// Set once the header is read.
    reader: Option<Y4mReader<BufReader<ChildStdout>>>,
    stderr: Option<JoinHandle<String>>,
}

impl Decoding {
    /// The stream's size and rate.
    pub(crate) fn info(&self) -> Option<VideoInfo> {
        self.reader.as_ref().map(Y4mReader::info)
    }

    /// The next frame into `out`; `false` once ffmpeg has written them all.
    ///
    /// # Errors
    /// [`VideoError::Decode`] when ffmpeg stops partway with an error; what [`Y4mReader::read`] returns otherwise.
    pub(crate) fn read(&mut self, out: &mut Picture) -> Result<bool, VideoError> {
        let Some(reader) = self.reader.as_mut() else { return Ok(false) };
        match reader.read(out) {
            Ok(true) => Ok(true),
            Ok(false) => {
                self.reader = None;
                let status = self.child.wait().map_err(|e| VideoError::Io(e.to_string()))?;
                let said = self.stderr.take().and_then(|t| t.join().ok()).unwrap_or_default();
                if status.success() { Ok(false) } else { Err(VideoError::Decode(said)) }
            }
            Err(e) => Err(e),
        }
    }

    /// Waits for ffmpeg to end and returns what it said.
    fn finish(&mut self) -> String {
        self.reader = None;
        let _ = self.child.wait();
        let said = self.stderr.take().and_then(|t| t.join().ok()).unwrap_or_default();
        if said.is_empty() { "it read no video from the file".to_owned() } else { said }
    }
}

impl Drop for Decoding {
    fn drop(&mut self) {
        // Closing the pipe first makes a running ffmpeg fail its next write and exit; the kill covers one that is
        // still reading the file. Either way `wait` reaps it, so no process outlives the reader.
        self.reader = None;
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(tail) = self.stderr.take() {
            let _ = tail.join();
        }
    }
}

/// The size a `width` x `height` video is decoded at: scaled down, keeping its shape, so its longer side is at most
/// `max`; unchanged when it already fits.
pub fn fit(width: u32, height: u32, max: u32) -> (u32, u32) {
    let long = width.max(height);
    if long <= max || long == 0 {
        return (width, height);
    }
    let scale = |side: u32| u32::try_from(u64::from(side) * u64::from(max) / u64::from(long)).unwrap_or(max).max(2);
    // Even sides keep 4:2:0 chroma whole.
    (scale(width) & !1, scale(height) & !1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn large_videos_scale_down_to_the_long_side_and_small_ones_stay() {
        assert_eq!(fit(3840, 2160, 1280), (1280, 720));
        assert_eq!(fit(1080, 1920, 1280), (720, 1280));
        assert_eq!(fit(640, 480, 1280), (640, 480));
        assert_eq!(fit(1281, 721, 1280), (1280, 720));
        assert_eq!(fit(2001, 999, 1000), (1000, 498), "even sides");
    }

    #[test]
    fn the_tail_keeps_the_last_words() {
        let long = "x".repeat(2000) + "\nthe real error\n";
        let tail = keep_tail(long.as_bytes());
        assert!(tail.ends_with("the real error") && tail.len() <= STDERR_TAIL + 20, "{tail}");
    }
}
