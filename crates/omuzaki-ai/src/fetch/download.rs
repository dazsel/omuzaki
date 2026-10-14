//! Streaming one body into a file: bounded, hashed as it goes where that is possible, with progress and cancel.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use omuzaki_ai_api::{FetchError, Progress};
use sha2::{Digest, Sha256 as Hasher};

use super::io_err;

/// Bytes pulled from the network per read. Far below 1 MiB, so memory stays flat for any file size.
const CHUNK: usize = 64 * 1024;

/// One MiB: progress is reported each time this many more bytes have arrived.
const PROGRESS_STEP: u64 = 1024 * 1024;

/// Where the stream ended up.
pub(super) enum Ended {
    /// The body ran out; `total` bytes are in the file (including any earlier part).
    Eof { total: u64 },
    /// Reading stopped because `limit` was passed: the body is longer than the pin.
    Overrun,
}

/// What a stream copy needs to know about the file it is filling.
pub(super) struct Target<'a> {
    /// The file, positioned at its end.
    pub file: &'a mut File,
    /// Its path, for errors.
    pub path: &'a Path,
    /// Bytes already in the file before this body.
    pub already: u64,
    /// The pinned length: at most `size + 1` bytes are ever kept in total.
    pub size: u64,
    /// Hashes the bytes as they are written (only for downloads that start at 0).
    pub hasher: Option<&'a mut Hasher>,
}

/// Why a copy stopped early. A read error keeps what arrived so far; the caller decides what to do with the file.
pub(super) enum Stop {
    /// The reader failed after some bytes (all of them written); the reason is for the error.
    Read(String),
    /// The caller cancelled.
    Cancelled,
    /// Writing the file failed.
    Write(FetchError),
}

/// Copies `reader` into `target` until the body ends or `size + 1` bytes are in the file, whichever is first.
pub(super) fn copy(
    mut reader: impl Read,
    mut target: Target<'_>,
    progress: &dyn Progress,
    stage: &str,
) -> Result<Ended, Stop> {
    let limit = target.size.saturating_add(1);
    let mut total = target.already;
    let mut reported = total / PROGRESS_STEP;
    let mut buf = vec![0u8; CHUNK];
    loop {
        if progress.is_cancelled() {
            return Err(Stop::Cancelled);
        }
        // Never ask for more than the pin allows, so an endless body stops exactly one byte past it.
        let want = usize::try_from(limit - total).map_or(CHUNK, |left| left.min(CHUNK));
        if want == 0 {
            return Ok(Ended::Overrun);
        }
        let n = match reader.read(&mut buf[..want]) {
            Ok(0) => return Ok(Ended::Eof { total }),
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(Stop::Read(e.to_string())),
        };
        target.file.write_all(&buf[..n]).map_err(|e| Stop::Write(io_err(target.path, &e)))?;
        if let Some(hasher) = target.hasher.as_deref_mut() {
            hasher.update(&buf[..n]);
        }
        total += n as u64;
        if total / PROGRESS_STEP > reported {
            reported = total / PROGRESS_STEP;
            progress.stage(stage, fraction(total, target.size));
        }
    }
}

/// How much of `size` is done, clamped to 0..=1.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // a progress bar needs no more precision
fn fraction(done: u64, size: u64) -> f32 {
    (done as f64 / size.max(1) as f64).min(1.0) as f32
}
