//! Decoding round trips through files the tests write themselves from synthetic tones (nothing is committed), and a
//! seeded mutation run over damaged files.
#![allow(clippy::unwrap_used, clippy::cast_possible_truncation, clippy::cast_precision_loss, clippy::cast_sign_loss)]

use std::f64::consts::TAU;

use omuzaki_audio_api::feed::feed_channel;
use omuzaki_audio_api::ring::ring;
use omuzaki_audio_api::tap::DropPolicy;
use omuzaki_audio_decode::{DecodeError, decode_bytes, decode_file, stream_file};

const LEFT_HZ: f64 = 440.0;
const RIGHT_HZ: f64 = 1_250.0;
const LEVEL: f64 = 0.5;

/// One second of the test tones at `rate`: 440 Hz on the left (or mono), 1,250 Hz on the right.
fn tones(rate: u32, channels: usize, frames: usize) -> Vec<Vec<f64>> {
    [LEFT_HZ, RIGHT_HZ]
        .iter()
        .take(channels)
        .map(|f| (0..frames).map(|i| LEVEL * (TAU * f * i as f64 / f64::from(rate)).sin()).collect())
        .collect()
}

/// The error of a decoded channel against the ideal 48 kHz tone, in dB below the tone, away from the abrupt edges.
fn error_db(decoded: &[f32], frequency: f64) -> f64 {
    let middle = 1_000..decoded.len() - 1_000;
    let (mut error, mut signal) = (0.0, 0.0);
    for i in middle {
        let ideal = LEVEL * (TAU * frequency * i as f64 / 48_000.0).sin();
        error += (f64::from(decoded[i]) - ideal).powi(2);
        signal += ideal.powi(2);
    }
    10.0 * (error / signal).log10()
}

fn interleave(channels: &[Vec<f64>], mut sample: impl FnMut(f64, &mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::new();
    for i in 0..channels[0].len() {
        for channel in channels {
            sample(channel[i], &mut out);
        }
    }
    out
}

fn to_i16(x: f64) -> i16 {
    (x * 32_767.0).round() as i16
}

#[derive(Clone, Copy)]
enum Pcm {
    S16,
    S24,
    F32,
}

fn wav(rate: u32, channels: &[Vec<f64>], pcm: Pcm) -> Vec<u8> {
    let (format, bits) = match pcm {
        Pcm::S16 => (1u16, 16u16),
        Pcm::S24 => (1, 24),
        Pcm::F32 => (3, 32),
    };
    let data = interleave(channels, |x, out| match pcm {
        Pcm::S16 => out.extend_from_slice(&to_i16(x).to_le_bytes()),
        Pcm::S24 => out.extend_from_slice(&((x * 8_388_607.0).round() as i32).to_le_bytes()[..3]),
        Pcm::F32 => out.extend_from_slice(&(x as f32).to_le_bytes()),
    });
    let count = channels.len() as u16;
    let block = count * bits / 8;
    let mut file = b"RIFF".to_vec();
    file.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
    file.extend_from_slice(b"WAVEfmt ");
    file.extend_from_slice(&16u32.to_le_bytes());
    file.extend_from_slice(&format.to_le_bytes());
    file.extend_from_slice(&count.to_le_bytes());
    file.extend_from_slice(&rate.to_le_bytes());
    file.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
    file.extend_from_slice(&block.to_le_bytes());
    file.extend_from_slice(&bits.to_le_bytes());
    file.extend_from_slice(b"data");
    file.extend_from_slice(&(data.len() as u32).to_le_bytes());
    file.extend_from_slice(&data);
    file
}

/// 16-bit AIFF: big-endian samples and an 80-bit extended sample rate.
fn aiff(rate: u32, channels: &[Vec<f64>]) -> Vec<u8> {
    let data = interleave(channels, |x, out| out.extend_from_slice(&to_i16(x).to_be_bytes()));
    let exponent = rate.ilog2();
    let mantissa = u64::from(rate) << (63 - exponent);
    let mut comm = (channels.len() as u16).to_be_bytes().to_vec();
    comm.extend_from_slice(&(channels[0].len() as u32).to_be_bytes());
    comm.extend_from_slice(&16u16.to_be_bytes());
    comm.extend_from_slice(&(16_383 + exponent as u16).to_be_bytes());
    comm.extend_from_slice(&mantissa.to_be_bytes());
    let mut body = b"AIFF".to_vec();
    body.extend_from_slice(b"COMM");
    body.extend_from_slice(&(comm.len() as u32).to_be_bytes());
    body.extend_from_slice(&comm);
    body.extend_from_slice(b"SSND");
    body.extend_from_slice(&(8 + data.len() as u32).to_be_bytes());
    body.extend_from_slice(&[0; 8]);
    body.extend_from_slice(&data);
    let mut file = b"FORM".to_vec();
    file.extend_from_slice(&(body.len() as u32).to_be_bytes());
    file.extend_from_slice(&body);
    file
}

/// An MSB-first bit writer for the FLAC encoder.
#[derive(Default)]
struct Bits {
    bytes: Vec<u8>,
    acc: u8,
    used: u32,
}

impl Bits {
    fn put(&mut self, value: u64, bits: u32) {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | ((value >> i) & 1) as u8;
            self.used += 1;
            if self.used == 8 {
                self.bytes.push(self.acc);
                (self.acc, self.used) = (0, 0);
            }
        }
    }

    fn align(&mut self) {
        while self.used != 0 {
            self.put(0, 1);
        }
    }
}

fn crc8(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0u8, |mut crc, byte| {
        crc ^= byte;
        for _ in 0..8 {
            crc = if crc & 0x80 != 0 { (crc << 1) ^ 0x07 } else { crc << 1 };
        }
        crc
    })
}

fn crc16(bytes: &[u8]) -> u16 {
    bytes.iter().fold(0u16, |mut crc, byte| {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
        }
        crc
    })
}

/// 16-bit FLAC with verbatim subframes: the simplest valid encoder, enough to exercise the reader and the decoder.
fn flac(rate: u32, channels: &[Vec<f64>]) -> Vec<u8> {
    const BLOCK: usize = 4_096;
    let frames = channels[0].len();
    let mut file = b"fLaC".to_vec();
    let mut info = Bits::default();
    info.put(1, 1); // the last metadata block
    info.put(0, 7); // STREAMINFO
    info.put(34, 24);
    info.put(BLOCK as u64, 16);
    info.put(BLOCK as u64, 16);
    info.put(0, 24);
    info.put(0, 24);
    info.put(u64::from(rate), 20);
    info.put(channels.len() as u64 - 1, 3);
    info.put(15, 5);
    info.put(frames as u64, 36);
    info.put(0, 64);
    info.put(0, 64);
    file.extend_from_slice(&info.bytes);
    for (number, start) in (0..frames).step_by(BLOCK).enumerate() {
        let size = BLOCK.min(frames - start);
        let mut frame = Bits::default();
        frame.put(0b11_1111_1111_1110, 14);
        frame.put(0, 2); // reserved, fixed block size
        frame.put(0b0111, 4); // the block size follows as 16 bits
        frame.put(0, 4); // the rate from STREAMINFO
        frame.put(channels.len() as u64 - 1, 4); // independent channels
        frame.put(0b100, 3); // 16 bits per sample
        frame.put(0, 1);
        assert!(number < 128, "the frame number is one byte here");
        frame.put(number as u64, 8);
        frame.put(size as u64 - 1, 16);
        let crc = crc8(&frame.bytes);
        frame.put(u64::from(crc), 8);
        for channel in channels {
            frame.put(0, 1);
            frame.put(0b00_0001, 6); // VERBATIM
            frame.put(0, 1);
            for x in &channel[start..start + size] {
                frame.put(u64::from(to_i16(*x) as u16), 16);
            }
        }
        frame.align();
        let crc = crc16(&frame.bytes);
        frame.put(u64::from(crc), 16);
        file.extend_from_slice(&frame.bytes);
    }
    file
}

/// Checks a decoded file: 48 kHz length, channel count, and each channel within `limit` dB of the ideal tone.
fn check(name: &str, bytes: Vec<u8>, extension: &str, rate: u32, channels: usize, limit: f64) -> f64 {
    let frames = rate as usize;
    let sound = decode_bytes(bytes, Some(extension)).unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(sound.channel_count(), channels, "{name}");
    assert_eq!(sound.frames(), (frames * 48_000).div_ceil(rate as usize), "{name}: frames at 48 kHz");
    let mut worst = f64::NEG_INFINITY;
    for (c, frequency) in [LEFT_HZ, RIGHT_HZ].iter().take(channels).enumerate() {
        let db = error_db(sound.channel(c), *frequency);
        assert!(db < limit, "{name}, channel {c}: {db:.1} dB");
        worst = worst.max(db);
    }
    worst
}

#[test]
fn wav_aiff_and_flac_round_trip_at_every_rate_far_below_minus_60_db() {
    let mut worst = f64::NEG_INFINITY;
    let mut cases = 0;
    for rate in [22_050, 44_100, 48_000, 96_000] {
        for channels in [1, 2] {
            let source = tones(rate, channels, rate as usize);
            let files = [
                ("wav s16", wav(rate, &source, Pcm::S16), "wav"),
                ("wav s24", wav(rate, &source, Pcm::S24), "wav"),
                ("wav f32", wav(rate, &source, Pcm::F32), "wav"),
                ("aiff s16", aiff(rate, &source), "aiff"),
                ("flac s16", flac(rate, &source), "flac"),
            ];
            for (format, bytes, extension) in files {
                let name = format!("{format} {rate} Hz {channels} ch");
                worst = worst.max(check(&name, bytes, extension, rate, channels, -60.0));
                cases += 1;
            }
        }
    }
    println!("decode: {cases} files, worst error {worst:.1} dB against the ideal 48 kHz tone");
    // 16-bit quantisation alone is about -90 dB; the resampler adds less than that.
    assert!(worst < -80.0, "worst {worst:.1} dB");
}

#[test]
fn the_container_is_found_from_the_bytes_without_an_extension() {
    let source = tones(44_100, 2, 44_100);
    let sound = decode_bytes(flac(44_100, &source), None).unwrap();
    assert_eq!(sound.channel_count(), 2);
    let sound = decode_bytes(wav(44_100, &source, Pcm::S16), Some("mp3")).unwrap();
    assert_eq!(sound.frames(), 48_000, "a wrong hint doesn't stop it");
}

#[test]
fn surround_folds_down_to_stereo_with_the_rest_at_minus_3_db() {
    // Six channels: the tones on the first two, then a DC level on one of the others.
    let mut source = tones(48_000, 2, 48_000);
    source.extend((0..4).map(|k| vec![if k == 1 { 0.25 } else { 0.0 }; 48_000]));
    let sound = decode_bytes(wav(48_000, &source, Pcm::F32), Some("wav")).unwrap();
    assert_eq!(sound.channel_count(), 2);
    let offset = 0.25 * std::f64::consts::FRAC_1_SQRT_2;
    for (c, frequency) in [LEFT_HZ, RIGHT_HZ].iter().enumerate() {
        let shifted: Vec<f32> = sound.channel(c).iter().map(|s| (f64::from(*s) - offset) as f32).collect();
        let db = error_db(&shifted, *frequency);
        assert!(db < -100.0, "channel {c}: {db:.1} dB");
    }
}

#[test]
fn files_that_are_not_audio_or_hold_none_say_so() {
    assert!(matches!(decode_bytes(b"hello, this is text".to_vec(), Some("txt")), Err(DecodeError::Unsupported(_))));
    assert!(matches!(decode_bytes(Vec::new(), None), Err(DecodeError::Unsupported(_) | DecodeError::Malformed(_))));
    let empty = wav(48_000, &[Vec::new()], Pcm::S16);
    assert!(decode_bytes(empty, Some("wav")).is_err(), "a file with no samples is no sound");
    let missing = std::env::temp_dir().join("omuzaki-decode-test-no-such-file.wav");
    assert!(matches!(decode_file(&missing), Err(DecodeError::Io(_))));
}

#[test]
fn more_than_ten_minutes_is_refused_for_the_bank() {
    // 8 kHz mono keeps the test file small: 601 s is 9.6 MB.
    let frames = 8_000 * 601;
    let mut file = wav(8_000, &[vec![0.0; 1]], Pcm::S16);
    let data = vec![0u8; frames * 2];
    file.truncate(file.len() - 2);
    let size = data.len() as u32;
    let length = file.len();
    file[length - 4..].copy_from_slice(&size.to_le_bytes());
    file[4..8].copy_from_slice(&(36 + size).to_le_bytes());
    file.extend_from_slice(&data);
    assert_eq!(decode_bytes(file, Some("wav")), Err(DecodeError::TooLong));
}

/// A small seeded generator, so a failing mutation can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(rng: &mut Rng, mut bytes: Vec<u8>) -> Vec<u8> {
    for _ in 0..=rng.below(4) {
        let at = rng.below(bytes.len());
        match rng.below(5) {
            0 => bytes[at] ^= 1 << rng.below(8),
            1 => {
                let end = (at + 1 + rng.below(16)).min(bytes.len());
                for byte in &mut bytes[at..end] {
                    *byte = rng.next() as u8;
                }
            }
            2 => bytes.truncate(at),
            3 => {
                let junk: Vec<u8> = (0..rng.below(64)).map(|_| rng.next() as u8).collect();
                bytes.splice(at..at, junk);
            }
            _ => {
                // A header field set to its extreme, as a fuzzer finds first.
                let end = (at + 4).min(bytes.len());
                bytes[at..end].fill(0xFF);
            }
        }
        if bytes.is_empty() {
            break;
        }
    }
    bytes
}

#[test]
fn a_thousand_damaged_files_error_cleanly_and_never_panic() {
    let source = tones(44_100, 2, 8_192);
    let originals = [
        (wav(44_100, &source, Pcm::S16), "wav"),
        (wav(44_100, &source, Pcm::F32), "wav"),
        (aiff(44_100, &source), "aiff"),
        (flac(44_100, &source), "flac"),
    ];
    let mut rng = Rng(0x5EED_0FD0_C0DE_1444);
    let (mut decoded, mut refused, mut panics) = (0, 0, Vec::new());
    let mut slowest = std::time::Duration::ZERO;
    for case in 0..1_000 {
        let (original, extension) = &originals[case % originals.len()];
        let bytes = mutate(&mut rng, original.clone());
        let started = std::time::Instant::now();
        let result = decode_bytes(bytes, Some(extension));
        slowest = slowest.max(started.elapsed());
        match result {
            Ok(sound) => {
                assert!(sound.frames() > 0 && sound.channel_count() <= 2);
                assert!((0..sound.channel_count()).all(|c| sound.channel(c).iter().all(|s| s.is_finite())));
                decoded += 1;
            }
            Err(DecodeError::Malformed(message)) if message.contains("could not read") => panics.push(case),
            Err(_) => refused += 1,
        }
    }
    println!(
        "mutations: 1000 damaged files, {decoded} still decoded, {refused} refused, {} panics, slowest {slowest:?}",
        panics.len()
    );
    assert!(panics.is_empty(), "the decoder panicked on mutation cases {panics:?}");
}

#[test]
fn a_stream_writes_the_same_audio_as_a_whole_decode_and_waits_for_room() {
    let dir = std::env::temp_dir().join(format!("omuzaki-decode-stream-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tones.flac");
    std::fs::write(&path, flac(44_100, &tones(44_100, 2, 3 * 44_100))).unwrap();
    let whole = decode_file(&path).unwrap();

    // A ring far smaller than the song: the decoder must wait for the reader, never drop. The feed is read the way an
    // engine reads it, at 48 kHz so the samples come through untouched.
    let (producer, consumer) = ring(4_800, DropPolicy::DropNewest, 0);
    let (writer, mut source) = feed_channel(producer, consumer, 48_000, true);
    let stream = stream_file(&path, writer).unwrap();
    let mut heard = Vec::new();
    let (mut left, mut right) = (vec![0.0f32; 512], vec![0.0f32; 512]);
    let mut at = 0;
    while !source.is_finished() {
        let got = source.pull(&mut left, &mut right, 512, at);
        at += 512;
        for i in 0..got {
            heard.extend_from_slice(&[left[i], right[i]]);
        }
        std::thread::yield_now();
    }
    let end = stream.join().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(!end.stopped);
    assert_eq!(end.frames as usize, whole.frames());
    assert_eq!(heard.len(), 2 * whole.frames(), "nothing refused");
    for (i, pair) in heard.chunks(2).enumerate() {
        assert_eq!(pair[0].to_bits(), whole.channel(0)[i].to_bits(), "left, frame {i}");
        assert_eq!(pair[1].to_bits(), whole.channel(1)[i].to_bits(), "right, frame {i}");
    }
}

#[test]
fn a_stream_stops_when_asked_or_when_its_reader_goes() {
    let dir = std::env::temp_dir().join(format!("omuzaki-decode-stop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("tones.wav");
    std::fs::write(&path, wav(48_000, &tones(48_000, 2, 10 * 48_000), Pcm::S16)).unwrap();

    let (producer, consumer) = ring(4_800, DropPolicy::DropNewest, 0);
    let (writer, consumer) = feed_channel(producer, consumer, 48_000, true);
    let stream = stream_file(&path, writer).unwrap();
    stream.stop();
    let end = stream.join().unwrap();
    assert!(end.stopped && (end.frames as usize) < 10 * 48_000);
    drop(consumer);

    let (producer, consumer) = ring(4_800, DropPolicy::DropNewest, 0);
    let (writer, consumer) = feed_channel(producer, consumer, 48_000, true);
    let stream = stream_file(&path, writer).unwrap();
    drop(consumer);
    let end = stream.join().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(end.stopped, "the reader went away");
}
