//! Encodes a seeded synthetic 1080p RGBA clip (a moving gradient with a soft alpha edge, 3 s at 30 fps) to ProRes 4444
//! (`prores_ks`, MOV) and VP9 with alpha (`libvpx-vp9`, WebM), and prints the per-frame encode time.
//!
//! Usage: `ffmpeg-lgpl-encode <output-dir> [seed]`. The clips are not committed (`docs/decisions/0003-assets-outside-git.md`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ffmpeg_next as ffmpeg;
use ffmpeg::{Dictionary, Rational, codec, encoder, format, frame, software::scaling};

/// Clip width in pixels.
const WIDTH: u32 = 1920;
/// Clip height in pixels.
const HEIGHT: u32 = 1080;
/// Frames per second.
const FPS: i32 = 30;
/// Clip length in seconds.
const SECONDS: i64 = 3;
/// Total frames in the clip.
const FRAMES: i64 = FPS as i64 * SECONDS;
/// Seed used when none is given.
const DEFAULT_SEED: u64 = 1424;

/// What can go wrong; every FFmpeg failure keeps its own error text.
#[derive(Debug, thiserror::Error)]
enum SpikeError {
    /// An FFmpeg call failed.
    #[error("ffmpeg: {0}")]
    Ffmpeg(#[from] ffmpeg::Error),
    /// The linked FFmpeg lacks an encoder the spike needs (a build without it, or a GPL-only name).
    #[error("encoder `{0}` is not in the linked FFmpeg build")]
    MissingEncoder(&'static str),
    /// The command line was wrong.
    #[error("usage: ffmpeg-lgpl-encode <output-dir> [seed]")]
    Usage,
    /// Writing the output folder failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// One output format to try.
struct Target {
    /// Encoder name in libavcodec.
    encoder: &'static str,
    /// Container short name for libavformat.
    container: &'static str,
    /// File name inside the output folder.
    file: &'static str,
    /// Pixel format the encoder takes (alpha kept).
    pix_fmt: format::Pixel,
    /// Encoder options (name, value).
    options: &'static [(&'static str, &'static str)],
}

const TARGETS: [Target; 2] = [
    Target {
        encoder: "prores_ks",
        container: "mov",
        file: "prores4444.mov",
        pix_fmt: format::Pixel::YUVA444P10LE,
        // Profile 4: ProRes 4444. `alpha_bits` 16 keeps the alpha plane at full depth.
        options: &[("profile", "4"), ("alpha_bits", "16")],
    },
    Target {
        encoder: "libvpx-vp9",
        container: "webm",
        file: "vp9-alpha.webm",
        pix_fmt: format::Pixel::YUVA420P,
        // libvpx carries alpha only without an alternate reference frame; `row-mt` and `cpu-used` keep it near real time.
        options: &[
            ("auto-alt-ref", "0"),
            ("crf", "30"),
            ("b", "0"),
            ("deadline", "realtime"),
            ("cpu-used", "5"),
            ("row-mt", "1"),
            ("g", "30"),
        ],
    },
];

/// Small deterministic generator (splitmix64) so the clip depends only on the seed.
struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value in `0.0..1.0`.
    fn unit(&mut self) -> f32 {
        // The top 24 bits fit an f32 mantissa exactly.
        (self.next_u64() >> 40) as f32 / 16_777_216.0
    }
}

/// Parameters drawn once from the seed.
struct Look {
    hue_phase: f32,
    speed: f32,
    radius: f32,
}

impl Look {
    fn from_seed(seed: u64) -> Self {
        let mut rng = Rng(seed);
        Self {
            hue_phase: rng.unit() * 6.0,
            speed: 0.5 + rng.unit(),
            radius: 0.30 + 0.10 * rng.unit(),
        }
    }
}

/// Fills `buf` (tightly packed RGBA, straight alpha) with frame `index`: a diagonal gradient that drifts over time, inside
/// a disc whose edge fades over about 4% of the height, so alpha takes every value from 0 to 255.
fn fill_frame(buf: &mut [u8], look: &Look, index: i64) {
    let t = index as f32 / FPS as f32;
    let (w, h) = (WIDTH as f32, HEIGHT as f32);
    let cx = w * (0.5 + 0.2 * (t * look.speed).sin());
    let cy = h * 0.5;
    let feather = h * 0.04;
    let radius = h * look.radius;
    for (i, px) in buf.chunks_exact_mut(4).enumerate() {
        let x = (i as u32 % WIDTH) as f32;
        let y = (i as u32 / WIDTH) as f32;
        let phase = look.hue_phase + (x + y) / (w + h) * 3.0 + t * look.speed;
        let dist = ((x - cx).powi(2) + (y - cy).powi(2)).sqrt();
        let alpha = ((radius + feather - dist) / (2.0 * feather)).clamp(0.0, 1.0);
        px[0] = to_u8(0.5 + 0.5 * phase.sin());
        px[1] = to_u8(0.5 + 0.5 * (phase + 2.094).sin());
        px[2] = to_u8(0.5 + 0.5 * (phase + 4.189).sin());
        px[3] = to_u8(alpha);
    }
}

/// Rounds a `0.0..=1.0` value to a byte.
fn to_u8(v: f32) -> u8 {
    // Clamped to 0..=255 first, so the cast cannot truncate.
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Copies a tightly packed RGBA buffer into an FFmpeg frame, honouring the frame's row stride.
fn copy_rgba(src: &[u8], dst: &mut frame::Video) {
    let stride = dst.stride(0);
    let row = WIDTH as usize * 4;
    let plane = dst.data_mut(0);
    for (s, d) in src.chunks_exact(row).zip(plane.chunks_exact_mut(stride)) {
        d[..row].copy_from_slice(s);
    }
}

/// Timing of one encoded clip.
struct Report {
    /// Per-frame time inside `send_frame` plus draining packets.
    encode: Vec<Duration>,
    /// Total time in the RGBA to YUVA conversion.
    convert: Duration,
    /// Bytes of the finished file.
    bytes: u64,
}

fn encode_clip(target: &Target, dir: &Path, look: &Look) -> Result<Report, SpikeError> {
    let codec = encoder::find_by_name(target.encoder).ok_or(SpikeError::MissingEncoder(target.encoder))?;
    let path: PathBuf = dir.join(target.file);
    let mut octx = format::output_as(&path, target.container)?;
    let tb = Rational::new(1, FPS);

    let mut enc = codec::context::Context::new_with_codec(codec).encoder().video()?;
    enc.set_width(WIDTH);
    enc.set_height(HEIGHT);
    enc.set_format(target.pix_fmt);
    enc.set_time_base(tb);
    enc.set_frame_rate(Some(Rational::new(FPS, 1)));
    if octx.format().flags().contains(format::Flags::GLOBAL_HEADER) {
        enc.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let mut opts = Dictionary::new();
    for (k, v) in target.options {
        opts.set(k, v);
    }
    let mut opened = enc.open_as_with(codec, opts)?;

    let mut ost = octx.add_stream(codec)?;
    ost.set_parameters(&opened);
    ost.set_time_base(tb);
    if target.pix_fmt == format::Pixel::YUVA420P {
        // The WebM muxer reads this tag to write the AlphaMode element, so players know the alpha plane is real.
        let mut meta = Dictionary::new();
        meta.set("alpha_mode", "1");
        ost.set_metadata(meta);
    }
    let stream_tb = ost.time_base();
    octx.write_header()?;
    let stream_tb = octx.stream(0).map_or(stream_tb, |s| s.time_base());

    let mut scaler = scaling::Context::get(
        format::Pixel::RGBA,
        WIDTH,
        HEIGHT,
        target.pix_fmt,
        WIDTH,
        HEIGHT,
        scaling::Flags::BILINEAR,
    )?;
    let mut rgba = vec![0_u8; WIDTH as usize * HEIGHT as usize * 4];
    let mut src = frame::Video::new(format::Pixel::RGBA, WIDTH, HEIGHT);
    let mut yuv = frame::Video::empty();
    let mut report = Report { encode: Vec::new(), convert: Duration::ZERO, bytes: 0 };

    for index in 0..FRAMES {
        fill_frame(&mut rgba, look, index);
        copy_rgba(&rgba, &mut src);
        let started = Instant::now();
        scaler.run(&src, &mut yuv)?;
        report.convert += started.elapsed();
        yuv.set_pts(Some(index));

        let started = Instant::now();
        opened.send_frame(&yuv)?;
        drain(&mut opened, &mut octx, tb, stream_tb)?;
        report.encode.push(started.elapsed());
    }
    let started = Instant::now();
    opened.send_eof()?;
    drain(&mut opened, &mut octx, tb, stream_tb)?;
    // Flushing the encoder's delayed frames belongs to the last frame's cost, not a frame of its own.
    if let Some(last) = report.encode.last_mut() {
        *last += started.elapsed();
    }
    octx.write_trailer()?;
    report.bytes = std::fs::metadata(&path)?.len();
    Ok(report)
}

/// Moves every ready packet from the encoder to the muxer.
fn drain(
    enc: &mut encoder::video::Encoder,
    octx: &mut format::context::Output,
    enc_tb: Rational,
    stream_tb: Rational,
) -> Result<(), SpikeError> {
    let mut packet = ffmpeg::Packet::empty();
    while enc.receive_packet(&mut packet).is_ok() {
        packet.set_stream(0);
        packet.rescale_ts(enc_tb, stream_tb);
        packet.write_interleaved(octx)?;
    }
    Ok(())
}

/// Prints the timing summary for one clip.
fn print_report(target: &Target, report: &Report) {
    let ms: Vec<f64> = report.encode.iter().map(|d| d.as_secs_f64() * 1e3).collect();
    for (i, v) in ms.iter().enumerate() {
        println!("{} frame {i:03} encode {v:.2} ms", target.encoder);
    }
    let total: f64 = ms.iter().sum();
    let mut sorted = ms.clone();
    sorted.sort_by(f64::total_cmp);
    let pick = |q: f64| sorted[((sorted.len() - 1) as f64 * q).round() as usize];
    println!(
        "{}: {} frames, encode mean {:.2} ms, p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms; convert mean {:.2} ms; {} bytes -> {}",
        target.encoder,
        ms.len(),
        total / ms.len() as f64,
        pick(0.5),
        pick(0.95),
        pick(1.0),
        report.convert.as_secs_f64() * 1e3 / ms.len() as f64,
        report.bytes,
        target.file,
    );
}

fn run() -> Result<(), SpikeError> {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().ok_or(SpikeError::Usage)?);
    let seed = match args.next() {
        Some(s) => s.parse().map_err(|_| SpikeError::Usage)?,
        None => DEFAULT_SEED,
    };
    std::fs::create_dir_all(&dir)?;
    ffmpeg::init()?;
    let look = Look::from_seed(seed);
    println!("seed {seed}, {WIDTH}x{HEIGHT} RGBA, {FRAMES} frames at {FPS} fps");
    for target in &TARGETS {
        let report = encode_clip(target, &dir, &look)?;
        print_report(target, &report);
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_depend_only_on_the_seed() {
        let mut a = vec![0_u8; WIDTH as usize * HEIGHT as usize * 4];
        let mut b = a.clone();
        fill_frame(&mut a, &Look::from_seed(7), 12);
        fill_frame(&mut b, &Look::from_seed(7), 12);
        assert_eq!(a, b);
        fill_frame(&mut b, &Look::from_seed(8), 12);
        assert_ne!(a, b);
    }

    #[test]
    fn alpha_has_a_soft_edge() {
        let mut buf = vec![0_u8; WIDTH as usize * HEIGHT as usize * 4];
        fill_frame(&mut buf, &Look::from_seed(DEFAULT_SEED), 0);
        let alphas: Vec<u8> = buf.chunks_exact(4).map(|p| p[3]).collect();
        assert!(alphas.contains(&0) && alphas.contains(&255));
        assert!(alphas.iter().any(|&a| a > 0 && a < 255));
    }
}
