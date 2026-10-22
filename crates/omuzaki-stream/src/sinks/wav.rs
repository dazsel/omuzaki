//! The WAV sink: one audio track as a 32-bit float WAV (`docs/design/record.md` D6, D7).
//!
//! The header's sizes are rewritten once a second, so after a crash the file is whole up to the last second. A track
//! near the 4 GiB limit of a WAV file rolls to a new part (`<stem>_<track>_part2.wav`) before it reaches it.
#![allow(
    clippy::disallowed_methods,
    reason = "the recorder writes only inside the take folder under `record.dir`, a Machine setting no agent may write; no op passes a path in"
)]

use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use hound::{SampleFormat, WavSpec, WavWriter};
use omuzaki_stream_api::error::StreamError;

/// Bytes a part may reach before the sink rolls to the next: the 4 GiB a WAV header can count, less a margin for the
/// header and the samples written between checks.
pub const MAX_PART_BYTES: u64 = (4 << 30) - (64 << 20);

/// Writes one track.
pub struct WavSink {
    base: PathBuf,
    rate: u32,
    channels: u16,
    writer: Option<WavWriter<BufWriter<fs::File>>>,
    part: u32,
    part_bytes: u64,
    since_flush: u64,
    frames: u64,
    paths: Vec<PathBuf>,
}

impl WavSink {
    /// Creates `path` (without extension: parts add `_part2`, then `.wav`) for float samples at `rate` Hz with
    /// `channels` interleaved channels.
    ///
    /// # Errors
    /// [`StreamError::Config`] for a zero rate or channel count, [`StreamError::Io`] if the file cannot be created.
    pub fn create(path: &Path, rate: u32, channels: u16) -> Result<Self, StreamError> {
        if rate == 0 || channels == 0 {
            return Err(StreamError::Config(format!("audio {rate} Hz x {channels} channels has a zero part")));
        }
        let mut sink = Self {
            base: path.to_owned(),
            rate,
            channels,
            writer: None,
            part: 0,
            part_bytes: 0,
            since_flush: 0,
            frames: 0,
            paths: Vec::new(),
        };
        sink.open_part()?;
        Ok(sink)
    }

    fn open_part(&mut self) -> Result<(), StreamError> {
        self.part += 1;
        let path = if self.part == 1 {
            self.base.with_extension("wav")
        } else {
            let name = self.base.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            self.base.with_file_name(format!("{name}_part{}.wav", self.part))
        };
        let spec = WavSpec {
            channels: self.channels,
            sample_rate: self.rate,
            bits_per_sample: 32,
            sample_format: SampleFormat::Float,
        };
        let file = fs::File::create(&path)
            .map_err(|e| StreamError::Io(format!("could not create {}: {e}", path.display())))?;
        let writer = WavWriter::new(BufWriter::new(file), spec).map_err(|e| wav_error(&path, &e))?;
        self.writer = Some(writer);
        self.part_bytes = 0;
        self.paths.push(path);
        Ok(())
    }

    /// Appends interleaved samples.
    ///
    /// # Errors
    /// [`StreamError::Io`] if a write fails.
    pub fn write(&mut self, samples: &[f32]) -> Result<(), StreamError> {
        let bytes = samples.len() as u64 * 4;
        if self.part_bytes + bytes > MAX_PART_BYTES {
            self.finish_part()?;
            self.open_part()?;
        }
        let path = self.paths.last().cloned().unwrap_or_default();
        let writer = self.writer.as_mut().ok_or(StreamError::Stopped)?;
        for &s in samples {
            writer.write_sample(s).map_err(|e| wav_error(&path, &e))?;
        }
        self.part_bytes += bytes;
        self.frames += samples.len() as u64 / u64::from(self.channels);
        self.since_flush += samples.len() as u64;
        if self.since_flush >= u64::from(self.rate) * u64::from(self.channels) {
            self.since_flush = 0;
            writer.flush().map_err(|e| wav_error(&path, &e))?;
        }
        Ok(())
    }

    /// Appends `frames` frames of silence.
    ///
    /// # Errors
    /// What [`write`](Self::write) reports.
    pub fn write_silence(&mut self, frames: u64) -> Result<(), StreamError> {
        const CHUNK: usize = 4096;
        let zeros = [0.0f32; CHUNK];
        let mut left = frames * u64::from(self.channels);
        while left > 0 {
            let n = usize::try_from(left.min(CHUNK as u64)).unwrap_or(CHUNK);
            self.write(&zeros[..n])?;
            left -= n as u64;
        }
        Ok(())
    }

    /// Sample frames written so far (one frame holds a sample of every channel).
    pub fn frames(&self) -> u64 {
        self.frames
    }

    fn finish_part(&mut self) -> Result<(), StreamError> {
        if let Some(writer) = self.writer.take() {
            let path = self.paths.last().cloned().unwrap_or_default();
            writer.finalize().map_err(|e| wav_error(&path, &e))?;
        }
        Ok(())
    }

    /// Writes the final header and closes the file; returns every part's path.
    ///
    /// # Errors
    /// [`StreamError::Io`] if the header cannot be written.
    pub fn finish(mut self) -> Result<Vec<PathBuf>, StreamError> {
        self.finish_part()?;
        Ok(std::mem::take(&mut self.paths))
    }
}

fn wav_error(path: &Path, e: &hound::Error) -> StreamError {
    StreamError::Io(format!("WAV {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_and_silence_round_trip() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let mut sink = WavSink::create(&dir.path().join("take_mic"), 48_000, 2)?;
        sink.write(&[0.25, -0.5, 1.0, 0.0])?;
        sink.write_silence(3)?;
        assert_eq!(sink.frames(), 5);
        let paths = sink.finish()?;
        assert_eq!(paths, [dir.path().join("take_mic.wav")]);
        let mut reader = hound::WavReader::open(&paths[0])?;
        assert_eq!(reader.spec().sample_rate, 48_000);
        let samples: Vec<f32> = reader.samples::<f32>().collect::<Result<_, _>>()?;
        assert_eq!(samples, [0.25, -0.5, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]);
        Ok(())
    }

    #[test]
    fn the_header_is_current_once_a_second_without_a_finish() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let mut sink = WavSink::create(&dir.path().join("crash"), 100, 1)?;
        sink.write(&[0.5; 150])?;
        // No finish: the process "dies" here. The flush after the first 100 samples made those readable.
        let reader = hound::WavReader::open(dir.path().join("crash.wav"))?;
        assert!(reader.len() >= 100, "{} samples readable", reader.len());
        drop(sink);
        Ok(())
    }
}
