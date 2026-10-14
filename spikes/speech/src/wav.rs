//! 16 kHz mono WAV in and out. Everything else in the spike works on `&[f32]` in -1.0..=1.0.

use std::path::Path;

use hound::{SampleFormat, WavReader, WavSpec, WavWriter};

use crate::{SAMPLE_RATE, SpeechError};

/// Reads a 16 kHz mono WAV (16-bit integer or 32-bit float) as `f32` samples in -1.0..=1.0.
///
/// # Errors
/// [`SpeechError::Wav`] when the file cannot be decoded, [`SpeechError::WavFormat`] when it is not 16 kHz mono.
pub fn read_mono_16k(path: &Path) -> Result<Vec<f32>, SpeechError> {
    let wav_err = |source| SpeechError::Wav { path: path.to_path_buf(), source };
    let reader = WavReader::open(path).map_err(wav_err)?;
    let spec = reader.spec();
    if spec.sample_rate != SAMPLE_RATE || spec.channels != 1 {
        return Err(SpeechError::WavFormat {
            path: path.to_path_buf(),
            rate: spec.sample_rate,
            channels: spec.channels,
        });
    }
    match spec.sample_format {
        SampleFormat::Float => reader.into_samples::<f32>().collect::<Result<_, _>>().map_err(wav_err),
        SampleFormat::Int => {
            // Scale by the format's full range, so 8/16/24-bit files all land in -1.0..=1.0.
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader.into_samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>().map_err(wav_err)
        }
    }
}

/// Writes `samples` as a 16-bit 16 kHz mono WAV, clipping anything outside -1.0..=1.0.
///
/// # Errors
/// [`SpeechError::Wav`] when the file cannot be written.
pub fn write_mono_16k(path: &Path, samples: &[f32]) -> Result<(), SpeechError> {
    let wav_err = |source| SpeechError::Wav { path: path.to_path_buf(), source };
    let spec = WavSpec { channels: 1, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: SampleFormat::Int };
    let mut writer = WavWriter::create(path, spec).map_err(wav_err)?;
    for &s in samples {
        // Same scale as the reader (full range = 2^15), so a write/read round trip is within half a quantisation step.
        writer.write_sample((s * 32768.0).round().clamp(-32768.0, 32767.0) as i16).map_err(wav_err)?;
    }
    writer.finalize().map_err(wav_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn round_trip_is_within_one_quantisation_step() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("rt.wav");
        let samples: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.05).sin() * 0.8).collect();
        write_mono_16k(&path, &samples)?;
        let back = read_mono_16k(&path)?;
        assert_eq!(back.len(), samples.len());
        for (a, b) in samples.iter().zip(&back) {
            assert!((a - b).abs() <= 1.0 / 32768.0, "{a} vs {b}");
        }
        Ok(())
    }

    #[test]
    fn rejects_stereo() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("stereo.wav");
        let spec =
            WavSpec { channels: 2, sample_rate: SAMPLE_RATE, bits_per_sample: 16, sample_format: SampleFormat::Int };
        let mut w = WavWriter::create(&path, spec)?;
        w.write_sample(0i16)?;
        w.write_sample(0i16)?;
        w.finalize()?;
        assert!(matches!(read_mono_16k(&path), Err(SpeechError::WavFormat { channels: 2, .. })));
        Ok(())
    }
}
