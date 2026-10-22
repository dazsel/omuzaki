//! The public-domain speech the `vad@1` and `asr@1` evaluations run on: three short fables from the LibriVox recording
//! of Aesop's Fables, Volume 3 (public domain; register row "LibriVox Aesop's Fables, Volume 03"), with the Project
//! Gutenberg text of the same translation (V. S. Vernon Jones, 1912, public domain).
//!
//! Nothing here is committed audio: each clip is pinned by URL, size and SHA-256 and fetched through the evaluation's
//! [`FileSource`](omuzaki_ai_api::FileSource) like a model file, then decoded (MP3, `symphonia`) and resampled to
//! 16 kHz mono. The speech segments are Silero VAD 6.2.3's (threshold 0.5, gaps under 300 ms closed, bursts under
//! 250 ms dropped), checked against the reading; an evaluation scores frames more than [`GUARD`] seconds from a segment
//! edge only, so a model is never marked down for where exactly a word fades.

use std::f64::consts::PI;
use std::fs::File;
use std::path::Path;

use omuzaki_ai_api::{EvalEnv, EvalError, NoProgress, PinnedFile, Sha256};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;

use crate::SAMPLE_RATE;

/// Seconds either side of a segment edge that no frame is scored in.
pub const GUARD: f64 = 0.15;

/// One pinned clip with what is said in it and where.
#[derive(Clone, Copy, Debug)]
pub struct Clip {
    /// A short name for reports: `aesop-55`.
    pub name: &'static str,
    /// Where it is fetched from.
    pub url: &'static str,
    /// The SHA-256 of the file, in hex.
    pub sha256: &'static str,
    /// Its exact size in bytes.
    pub size: u64,
    /// The words read, as printed (normalised before scoring).
    pub text: &'static str,
    /// The speech, as `(start, end)` seconds, in order.
    pub speech: &'static [(f64, f64)],
}

/// The LibriVox item every clip is from.
macro_rules! librivox {
    ($file:literal) => {
        concat!("https://archive.org/download/aesop_fables_volume_three_librivox/", $file)
    };
}

/// The LibriVox preamble and close every section is read with.
macro_rules! section {
    ($n:literal, $title:literal, $body:literal) => {
        concat!(
            "Section ",
            $n,
            " of Aesop's Fables, Volume 3. This is a LibriVox recording. All LibriVox recordings are in the public \
             domain. For more information or to volunteer, please visit librivox.org. ",
            $title,
            ". ",
            $body,
            " End of section ",
            $n,
            "."
        )
    };
}

/// The three clips, about 47, 44 and 50 seconds.
pub const CLIPS: [Clip; 3] = [
    Clip {
        name: "aesop-55",
        url: librivox!("fables_03_05_aesop_64kb.mp3"),
        sha256: "12bf43a277d1da9314ab460999e9117339018b72d58f710d33eaaf92dc7f567b",
        size: 375_663,
        text: section!(
            "5",
            "The Thieves and the Cock",
            "Some Thieves broke into a house, and found nothing worth taking except a Cock, which they seized and \
             carried off with them. When they were preparing their supper, one of them caught up the Cock, and was \
             about to wring his neck, when he cried out for mercy and said, \"Pray do not kill me: you will find me a \
             most useful bird, for I rouse honest men to their work in the morning by my crowing.\" But the Thief \
             replied with some heat, \"Yes, I know you do, making it still harder for us to get a livelihood. Into the \
             pot you go!\""
        ),
        speech: &[
            (0.83, 4.99),
            (5.41, 8.83),
            (9.76, 10.91),
            (11.84, 13.15),
            (13.92, 17.89),
            (18.40, 20.26),
            (21.15, 25.31),
            (25.86, 27.52),
            (28.22, 29.34),
            (29.70, 34.53),
            (35.20, 36.90),
            (37.44, 42.69),
            (43.26, 44.74),
        ],
    },
    Clip {
        name: "aesop-68",
        url: librivox!("fables_03_18_aesop_64kb.mp3"),
        sha256: "2c905d57f55905272aea02d028bb4e26a3362b3979b79d309afe23058dfc95ba",
        size: 353_984,
        text: section!(
            "18",
            "The Boy and the Nettles",
            "A Boy was gathering berries from a hedge when his hand was stung by a Nettle. Smarting with the pain, he \
             ran to tell his mother, and said to her between his sobs, \"I only touched it ever so lightly, mother.\" \
             \"That's just why you got stung, my son,\" she said; \"if you had grasped it firmly, it wouldn't have \
             hurt you in the least.\""
        ),
        speech: &[
            (0.93, 2.88),
            (3.26, 5.79),
            (6.21, 10.34),
            (10.82, 12.54),
            (13.18, 14.34),
            (15.39, 16.80),
            (17.57, 21.38),
            (21.79, 25.95),
            (26.27, 28.61),
            (29.22, 32.03),
            (32.64, 34.30),
            (34.72, 36.22),
            (37.09, 39.20),
        ],
    },
    Clip {
        name: "aesop-73",
        url: librivox!("fables_03_23_aesop_64kb.mp3"),
        sha256: "a9fa7b93a90486fdfc6ef9668a980cbdff15c6190f4d9023ddb941365ab8b95f",
        size: 400_113,
        text: section!(
            "23",
            "The Two Bags",
            "Every man carries Two Bags about with him, one in front and one behind, and both are packed full of \
             faults. The Bag in front contains his neighbours' faults, the one behind his own. Hence it is that men do \
             not see their own faults, but never fail to see those of others."
        ),
        speech: &[
            (1.47, 3.71),
            (4.13, 6.94),
            (7.39, 12.38),
            (12.93, 16.03),
            (16.77, 18.02),
            (18.34, 19.49),
            (20.42, 23.10),
            (23.55, 25.57),
            (25.95, 28.29),
            (28.83, 33.12),
            (33.89, 37.15),
            (37.50, 38.66),
            (39.01, 40.35),
            (41.25, 42.91),
        ],
    },
];

impl Clip {
    /// The download pin.
    #[must_use]
    pub fn pin(&self) -> PinnedFile {
        let sha256 = Sha256::from_hex(self.sha256).unwrap_or_else(|| unreachable!("{} has a literal hash", self.name));
        PinnedFile::new([self.url], sha256, self.size)
    }

    /// Fetches the clip through `env` and decodes it to 16 kHz mono.
    ///
    /// # Errors
    /// [`EvalError::Fixture`] when it can't be had, [`EvalError::Output`] when it can't be decoded.
    pub fn load(&self, env: &dyn EvalEnv) -> Result<Vec<f32>, EvalError> {
        let path = env.files().get(&self.pin(), &NoProgress)?;
        decode_16k(&path).map_err(|why| EvalError::Output(format!("{}: {why}", self.name)))
    }

    /// Whether `seconds` is in a speech segment (`Some(true)`), in a pause (`Some(false)`), or too near an edge to
    /// score (`None`).
    #[must_use]
    pub fn label(&self, seconds: f64) -> Option<bool> {
        label(self.speech, seconds)
    }

    /// The audio cut into pieces of at most `max_seconds` at pauses: each piece runs from a segment's start to the end
    /// of the last segment that still fits, with [`GUARD`] seconds of padding.
    #[must_use]
    pub fn utterances<'a>(&self, audio: &'a [f32], max_seconds: f64) -> Vec<&'a [f32]> {
        utterances(self.speech, audio, max_seconds)
    }
}

/// See [`Clip::label`].
#[must_use]
pub fn label(speech: &[(f64, f64)], seconds: f64) -> Option<bool> {
    if speech.iter().any(|&(a, b)| (seconds - a).abs() < GUARD || (seconds - b).abs() < GUARD) {
        return None;
    }
    Some(speech.iter().any(|&(a, b)| (a..b).contains(&seconds)))
}

/// See [`Clip::utterances`].
#[must_use]
pub fn utterances<'a>(speech: &[(f64, f64)], audio: &'a [f32], max_seconds: f64) -> Vec<&'a [f32]> {
    let at = |seconds: f64| ((seconds.max(0.0) * f64::from(SAMPLE_RATE)) as usize).min(audio.len());
    let mut out = Vec::new();
    let mut rest = speech;
    while let Some(&(start, _)) = rest.first() {
        let fits = rest.iter().take_while(|&&(_, end)| end - start <= max_seconds - 2.0 * GUARD).count().max(1);
        let end = rest[fits - 1].1;
        out.push(&audio[at(start - GUARD)..at(end + GUARD)]);
        rest = &rest[fits..];
    }
    out
}

/// Decodes an audio file (MP3, Ogg Vorbis, WAV, FLAC) to 16 kHz mono: channels averaged, then resampled.
///
/// # Errors
/// What went wrong, in words.
pub fn decode_16k(path: &Path) -> Result<Vec<f32>, String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let stream = MediaSourceStream::new(Box::new(file), MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let mut format = symphonia::default::get_probe()
        .probe(&hint, stream, FormatOptions::default(), MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    let track = format.default_track(TrackType::Audio).ok_or("no audio track")?;
    let parameters = track.codec_params.as_ref().and_then(|p| p.audio()).ok_or("no audio parameters")?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(parameters, &AudioDecoderOptions::default())
        .map_err(|e| e.to_string())?;
    let track = track.id;
    let mut planes: Vec<Vec<f32>> = Vec::new();
    let mut mono = Vec::new();
    let mut rate = None;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) | Err(SymphoniaError::ResetRequired) => break,
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id != track {
            continue;
        }
        let buffer = match decoder.decode(&packet) {
            Ok(buffer) => buffer,
            Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let channels = buffer.num_planes();
        if channels == 0 {
            continue;
        }
        if *rate.get_or_insert(buffer.spec().rate()) != buffer.spec().rate() {
            return Err("the sample rate changes partway through".into());
        }
        buffer.copy_to_vecs_planar(&mut planes);
        let frames = planes.iter().take(channels).map(Vec::len).min().unwrap_or(0);
        mono.extend((0..frames).map(|i| planes.iter().take(channels).map(|p| p[i]).sum::<f32>() / channels as f32));
    }
    let rate = rate.ok_or("no audio")?;
    Ok(resample(&mono, rate, SAMPLE_RATE))
}

/// Resamples `input` from `from` Hz to `to` Hz with a Hann-windowed sinc (32 zero crossings a side, cut off at 95 % of
/// the lower Nyquist). For test material: it allocates, and is exact enough that speech models can't tell.
#[must_use]
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    const ZEROS: f64 = 32.0;
    if from == to || input.is_empty() || from == 0 {
        return input.to_vec();
    }
    let ratio = f64::from(to) / f64::from(from);
    let cutoff = ratio.min(1.0) * 0.95;
    let reach = (ZEROS / cutoff).ceil() as isize;
    let len = (input.len() as f64 * ratio).floor() as usize;
    (0..len)
        .map(|n| {
            let t = n as f64 / ratio;
            let center = t.floor() as isize;
            let first = (center - reach + 1).max(0);
            let last = (center + reach).min(input.len().cast_signed() - 1);
            (first..=last)
                .map(|k| {
                    let x = (t - k as f64) * cutoff;
                    let window = if x.abs() < ZEROS { f64::midpoint(1.0, (PI * x / ZEROS).cos()) } else { 0.0 };
                    let sinc = if x == 0.0 { 1.0 } else { (PI * x).sin() / (PI * x) };
                    cutoff * sinc * window * f64::from(input[k as usize])
                })
                .sum::<f64>() as f32
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_skip_the_edges_and_know_speech_from_pauses() {
        let speech = [(1.0, 2.0), (3.0, 4.0)];
        assert_eq!(label(&speech, 0.5), Some(false));
        assert_eq!(label(&speech, 1.5), Some(true));
        assert_eq!(label(&speech, 1.9), None);
        assert_eq!(label(&speech, 2.5), Some(false));
        assert_eq!(label(&speech, 3.5), Some(true));
    }

    #[test]
    fn utterances_split_at_pauses_and_never_exceed_the_limit() {
        let audio = vec![0.0; 16_000 * 60];
        let speech = [(1.0, 10.0), (11.0, 20.0), (21.0, 35.0), (36.0, 40.0), (41.0, 70.0)];
        let pieces = utterances(&speech, &audio, 25.0);
        let seconds: Vec<f64> = pieces.iter().map(|p| p.len() as f64 / 16_000.0).collect();
        // 1-20 and 21-40 fit in 25 s with their padding; 41-70 is longer than the limit on its own and is cut where
        // the audio ends (60 s).
        let expected = [19.3, 19.3, 19.15];
        assert_eq!(pieces.len(), expected.len(), "{seconds:?}");
        assert!(seconds.iter().zip(expected).all(|(a, b)| (a - b).abs() < 1e-3), "{seconds:?}");
    }

    #[test]
    fn a_tone_keeps_its_level_and_pitch_through_the_resampler() {
        let tone = |rate: u32, seconds: f64| -> Vec<f32> {
            (0..(f64::from(rate) * seconds) as usize).map(|i| (2.0 * PI * 440.0 * i as f64 / f64::from(rate)).sin() as f32).collect()
        };
        let out = resample(&tone(44_100, 1.0), 44_100, 16_000);
        assert_eq!(out.len(), 16_000);
        let reference = tone(16_000, 1.0);
        let middle = 2_000..14_000;
        let worst = out[middle.clone()].iter().zip(&reference[middle]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 1e-3, "largest error {worst}");
    }

    #[test]
    fn every_clip_is_pinned_and_its_segments_are_in_order() {
        for clip in CLIPS {
            assert_eq!(clip.pin().size, clip.size, "{}", clip.name);
            assert!(clip.speech.windows(2).all(|w| w[0].1 < w[1].0), "{}", clip.name);
            assert!(clip.speech.iter().all(|&(a, b)| a < b), "{}", clip.name);
        }
    }
}
